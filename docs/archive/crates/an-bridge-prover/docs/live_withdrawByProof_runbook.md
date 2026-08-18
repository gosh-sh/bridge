> **⚠️ ARCHIVED 2026-08-18 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/ETH-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

# Live `withdrawByProof` E2E Runbook — shellnet → Sepolia (Circuit 4)

Operational guide for driving a full AN→ETH withdrawal E2E through the
in-process `relayer withdraw-e2e` orchestrator against a deployed
`AckiNackiBridge` on Sepolia. Covers first-time end-to-end demo from a
fresh deploy, follow-up withdrawals on an existing deploy, and the failure
modes we have actually hit.

**Scope of this runbook.** The event/proof/submit path: capture live
`WithdrawalInitiated` ExtOut → export partial witness → enrich against
`prover_state.json` → Circuit 4 SHPLONK aggregate → `withdrawByProof`.
**Assumes** the bundle-only path (Circuits 1A + 2 via `daemon-live`) is
already running or has just been launched — that lane is covered in
[`live_verifyBlock_runbook.md`](./live_verifyBlock_runbook.md), which
this doc extends.

> **Notation.** `seq_no` = Acki Nacki block sequence number.
> "Covering bundle" = the first bundle whose `key_seq_no ≥ event_seq_no`
> that is verified on-chain. Withdrawals can only be submitted after
> their covering bundle lands (Circuit 4 anchor-chain verifies against
> a `layer_hashes` root that `verifyBlock` has already committed).
> With `W=128, P=8`, one bundle covers 1024 seq_nos of AN chain time.

---

## Table of Contents

- [Quick resume checklist (returning mid-flow)](#quick-resume-checklist-returning-mid-flow)
- [Timing model — why fresh-deploy demos need tight lookahead](#timing-model--why-fresh-deploy-demos-need-tight-lookahead)
- [Reference addresses (current deploy)](#reference-addresses-current-deploy)
- [Binary + env prerequisites](#binary--env-prerequisites)
- [Case 1 — First-time E2E from a fresh deploy (optimal sequence)](#case-1--first-time-e2e-from-a-fresh-deploy-optimal-sequence)
- [Case 2 — Follow-up withdrawal on an existing deploy](#case-2--follow-up-withdrawal-on-an-existing-deploy)
- [Case 3 — Event captured but daemon far behind head](#case-3--event-captured-but-daemon-far-behind-head)
- [Case 4 — Prover subprocess timeout / OOM](#case-4--prover-subprocess-timeout--oom)
- [Case 5 — On-chain `withdrawByProof` revert](#case-5--on-chain-withdrawbyproof-revert)
- [Case 6 — USDCBridge key drift (burn side)](#case-6--usdcbridge-key-drift-burn-side)
- [Health checks](#health-checks)
- [File & state reference](#file--state-reference)
- [Change log / known incidents](#change-log--known-incidents)

---

## Quick resume checklist (returning mid-flow)

```bash
cd /Users/alinat/HALO2_TVM_EXPERIMENTS/bridge/crates/an-bridge-prover
export BRIDGE=0x59dE8848bD5B3F1BD02AF9D269ab313AFa1d900B
export RPC=https://ethereum-sepolia-rpc.publicnode.com

# 1. Is the bundle daemon alive and current?
pgrep -af 'relayer daemon-live' || echo "DAEMON NOT RUNNING"
echo "chain last_seen   = $(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC --json | jq -r '.[0]')"
echo "prover_state mtime: $(stat -f '%Sm' state/prover_state.json 2>/dev/null || echo MISSING)"

# 2. Did the burn fire? If yes, which seq_no?
ls -lt work_dir/witness_event_*.json 2>/dev/null | head -3
ls -lt proofs/proof_event_*.json    2>/dev/null | head -3

# 3. What's the last on-chain withdrawal?
cast logs --address $BRIDGE --rpc-url $RPC \
  'event WithdrawalExecuted(uint256,address,uint256,uint256)' --from-block -5000 | tail -20
```

**Decision matrix:**

| Daemon | Event captured | Proof generated | Go to |
|---|---|---|---|
| running, current | no | no | fire the burn — [Case 1](#case-1--first-time-e2e-from-a-fresh-deploy-optimal-sequence) step 5, or [Case 2](#case-2--follow-up-withdrawal-on-an-existing-deploy) |
| running, current | yes | no | run `withdraw-e2e` (proof + submit) |
| running, behind | yes | no | [Case 3](#case-3--event-captured-but-daemon-far-behind-head) |
| running, current | yes | yes, revert | [Case 5](#case-5--on-chain-withdrawbyproof-revert) |
| not running | any | any | Fix the bundle lane first — see verifyBlock runbook Case 3–6 |

---

## Timing model — why fresh-deploy demos need tight lookahead

The Circuit 4 proof is anchored to `layer_hashes[1]` of the **covering bundle**
— the first key block `k` where `event_seq_no ≤ k`. On-chain
`withdrawByProof` will revert until `verifyBlock(covering_bundle)` has
already landed. So end-to-end wall time =

```
t_e2e = max(0, covering_bundle_seqno − daemon_last_verified) / bundle_stride
        × prover_wall_time_per_bundle              # daemon catch-up
      + prover_wall_time_per_bundle                # covering bundle itself
      + circuit_4_prove_time + submit_time         # withdraw leg
```

With `W·P = 1024` stride, `~12 min` per bundle wall time (warm PK cache),
and `~5 min` for C4+submit, the "how far behind daemon is" term dominates:

| daemon lag at burn (bundles) | catch-up | +covering | +C4+submit | **total** |
|---|---|---|---|---|
| 0 (burn during bundle 1 window) | 0 | 12 | 5 | **~17 min** |
| 1 (burn during bundle 2 window) | 12 | 12 | 5 | **~29 min** |
| 7 (Deploy #7 accident, see change log) | 84 | 12 | 5 | **~101 min** |

**Consequence for fresh demos.** Genesis anchors baked into `.env.shellnet`
by `compute_bridge_anchors --at-head` embed the chain-head lookahead at
the time you run the tool. That lookahead **is** the daemon's starting
lag. Two rules:

1. **Deploy immediately after computing anchors.** Every minute of delay
   drives chain forward at 3 b/s (~180 seq_nos/min) while your seed is
   stuck. A 10 min gap costs one whole bundle of catch-up.
2. **Fire the burn during the daemon's first bundle-processing window.**
   The `withdraw-e2e` orchestrator will patiently wait through
   `--event-wait-s`, then use covering bundle 1 or 2. If you fire before
   the daemon even starts, the event lands too early and you wait for
   the covering bundle regardless.

The Deploy #7 → #8 rerun (2026-08-17, see change log) collapsed a
projected 90 min wait to ~17 min by re-running `compute_bridge_anchors`
right before deploy (75-block lookahead vs 262) and firing the burn
inside bundle 1's window.

---

## Reference addresses (current deploy)

Deploy #8 (2026-08-17). Source of truth: latest `Deploy #N` section of
[`../../../bridge-deployer.txt`](../../../bridge-deployer.txt) and the
forge broadcast log at
`contracts/ethereum/broadcast/DeployShellnetE2EBridge.s.sol/11155111/run-latest.json`.

| Item | Value |
|---|---|
| Network | Sepolia (chain 11155111) |
| `AckiNackiBridge` | `0x59dE8848bD5B3F1BD02AF9D269ab313AFa1d900B` |
| `PrimaryAggregatorVerifier` | `0x80089e338834826e54362e439d554f3e0facfbc9` |
| `FallbackAggregatorVerifier` | `0xe6f3b60b24ee3750f455054a1320fcbf5a11784c` |
| `LayerHashesAggregatorVerifier` | `0xcae03555a63c7a78a2c0554093cb98c2aa307aa7` |
| `BridgeWithdrawalAggregatorVerifier` (C4) | `0xe0bd31797b3d64dec85a0957304ae5ccc29efd2e` |
| `MockBlockHeaderOracle` | `0x312e2e8f159cae9d85cc0a0dfdf0cf1184a08f5a` |
| Bootstrap seed seq_no | `8768512` (W·P=1024-aligned) |
| Genesis `bk_set_commitment` | `0x08eb0a1892e4f75a8b5c8cff69322f95bf0437c371903998c9365fbe293ca71c` |
| Genesis `prev_max_level_layer_hash` | `0x28df66280644ceb9c08e0a8a5ac924939521577006f00c1b1c6538e6c0474449` |
| `WITHDRAW_ACC_FR` (USDCBridge account_id as Fr) | `0x1a1a…1a1a` (canonical, palindromic — see below) |
| Deployer / relayer wallet | `0x841709B6842233d8474aeA1d773e8d0F7c7c0B9f` |
| Owner (paused/unpause) | `0xb586356D52eAee055Ca569Ff412DFeFFc5bB2307` |

**`WITHDRAW_ACC_FR` derivation** (verify once per deploy — should never
change on shellnet):

```bash
# Shellnet USDCBridge account_id is deterministically [0x1a; 32]
python3 -c "
b = bytes([0x1a]*32)
r = 21888242871839275222246405745257275088548364400416034343698204186575808495617
# LE and BE coincide for palindromic bytes → both < r → no reduction needed
print(int.from_bytes(b, 'little') == int.from_bytes(b, 'big'),
      int.from_bytes(b, 'little') < r)
print(int.from_bytes(b, 'little'))   # 11806252235961651298089590628336806290921645495320214372650577192963691649562
"

# Verify on-chain equality
cast call $BRIDGE 'expectedWithdrawAcc()(uint256)' --rpc-url $RPC
# should print: 11806252235961651298089590628336806290921645495320214372650577192963691649562
```

If `expectedWithdrawAcc()` returns anything other than the palindromic
value above, the deploy used a non-canonical `WITHDRAW_ACC_FR`. Every C4
submit will revert on the equality check — **stop and redeploy** with
the correct `WITHDRAW_ACC_FR` in
[`contracts/ethereum/.env.shellnet`](../../../contracts/ethereum/.env.shellnet).

---

## Binary + env prerequisites

Working directory: `crates/an-bridge-prover/`.

**One-time setup (per fresh clone):**

```bash
cd crates/an-bridge-prover

# 1. Build the relayer (same binary as daemon-live)
cargo build --release -p bridge-relayer-daemon --bin relayer

# 2. Build the aggregator subprocess (mandatory for both lanes)
cd ../bridge-evm-aggregator && cargo build --release && cd ../an-bridge-prover

# 3. Build the Circuit 4 event prover
cargo build --release -p bridge-event-halo2-prover
#   -> ./target/release/bridge-event-halo2-prover
```

**Env sanity (in addition to bundle-lane vars from parent runbook):**

```bash
cd crates/an-bridge-prover
for v in RPC_URL BRIDGE_ADDRESS RELAYER_PRIVATE_KEY BRIDGE_GQL_ENDPOINT; do
  grep -q "^${v}=" .env.shellnet && echo "  ok  $v" || echo "  FAIL $v"
done
```

The withdraw command reads `RPC_URL`, `BRIDGE_ADDRESS`, `RELAYER_PRIVATE_KEY`
via clap `env` attrs; `BRIDGE_GQL_ENDPOINT` must be aliased to
`GQL_ENDPOINT` on the command line (see Case 1).

---

## Case 1 — First-time E2E from a fresh deploy (optimal sequence)

**When to use.** Contract just deployed. Bundle daemon just cold-started.
You want to demonstrate the on-chain withdraw leg with minimum wall time.

**The optimal sequence** — every step gates the next; do not interleave:

### Step 0 — Pre-deploy anchor freshness check

Anchors staler than ~3 min lose bundles of catch-up time. Confirm the
lookahead is tight before running forge.

```bash
cd crates/an-bridge-prover/bridge-prover-lib
cargo run --release --bin compute_bridge_anchors -- \
  --at-head \
  --gql-endpoint https://shellnet.ackinacki.org/graphql
# note the printed seed_seqno and (chain_head - seed_seqno) — should be < 200
```

Paste the printed anchors into
`contracts/ethereum/.env.shellnet` and
`crates/an-bridge-prover/.env.shellnet` (see the Deploy #8 template in
git history for exact field names).

### Step 1 — Deploy the bridge

```bash
cd contracts/ethereum
set -a && source .env.shellnet && set +a
forge script script/DeployShellnetE2EBridge.s.sol:DeployShellnetE2EBridge \
  --rpc-url $SEPOLIA_RPC_URL --broadcast --slow
# verify the printed AckiNackiBridge, then paste into daemon .env.shellnet
```

### Step 2 — Unpause

```bash
export BRIDGE=<new_address>
cast send $BRIDGE 'unpause()' --rpc-url $RPC --private-key $OWNER_PK
cast call $BRIDGE 'paused()(bool)' --rpc-url $RPC   # false
```

### Step 3 — Cold-start the bundle daemon

Full procedure in the [verifyBlock runbook Case 1](./live_verifyBlock_runbook.md#case-1--first-time-bootstrap-from-a-fresh-deploy).
Verify the log line `seed_policy=Explicit(<seed_seqno>)` appears within
30s. The daemon needs a running proof pipeline before the burn fires —
otherwise the covering bundle waits on daemon startup instead of on
Circuit 4.

### Step 4 — Fire the burn

The AN-side burn is orchestrated by
[`python/test_deploy_and_withdraw_only.py`](../python/test_deploy_and_withdraw_only.py)
— the only external step in the withdraw E2E.

```bash
cd crates/an-bridge-prover
MODE=shellnet python3 python/test_deploy_and_withdraw_only.py
```

The script deploys a fresh Multisig (two-shot GiverV3.sendCurrencyWithFlag
17→1 on shellnet), mints USDC via `USDCBridge.mintAndSend`, calls
`initiate_withdrawal`, and polls GQL for the `WithdrawalInitiated`
ExtOut. It prints the captured `seq_no` + `block_hash` + `msg_id`.

**Timing sanity.** If the printed `seq_no` is within one bundle stride
(`< daemon_last_verified + 1024`), you'll land coverage in the next
bundle (bundle 2 typically). Anything worse and the wait grows linearly
— see the [timing model](#timing-model--why-fresh-deploy-demos-need-tight-lookahead).

### Step 5 — Wait for the covering bundle

```bash
# Chain progress
watch -n 30 'cast call 0x59dE8848bD5B3F1BD02AF9D269ab313AFa1d900B \
  storedLastSeenBlockSeqNo\(\)\(uint64\) --rpc-url $RPC'

# Wait until chain last_seen ≥ (event_seq_no rounded UP to next 1024 boundary)
```

### Step 6 — Run `withdraw-e2e --dry-run`

```bash
cd crates/an-bridge-prover
set -a && source .env.shellnet && set +a
TS=$(date +%Y%m%d_%H%M%S)

./target/release/relayer withdraw-e2e \
  --gql-endpoint $BRIDGE_GQL_ENDPOINT \
  --prover-state-path state/prover_state.json \
  --window-size 128 \
  --bridge-account-id 1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a \
  --bridge-dapp-id    1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a \
  --anchor-layer auto \
  --work-dir work_dir \
  --an-bridge-prover-dir . \
  --prover-out-dir proofs \
  --prover-seq-no $(date +%s) \
  --dry-run \
  2>&1 | tee logs/withdraw_dry_${TS}.log
```

Expected log signature:

```
INFO capture_next_withdrawal_event: matched dst=:...:026a, seq_no=<N>
INFO enrich_witness: anchor_layer_mode=Auto, chosen L=1, key_seq_no=<K>
INFO subprocess_prover: bridge-event-halo2-prover start
INFO subprocess_prover: aggregate SHPLONK ok, calldata_len=<bytes>
INFO submit_withdraw: dry-run eth_call OK — would submit withdrawByProof(...)
```

Dry-run returning OK proves the proof is well-formed and the on-chain
adapter accepts it. If dry-run reverts, jump to [Case 5](#case-5--on-chain-withdrawbyproof-revert)
— **do not** submit for real.

### Step 7 — Real submit

Re-run the same command **without** `--dry-run`. The prover PK cache is
warm from step 6 so total wall time collapses to `submit + confirm`
(~30–60s). Look for `withdrawByProof confirmed tx=0x...`.

Verify:

```bash
cast logs --address $BRIDGE --rpc-url $RPC \
  'event WithdrawalExecuted(uint256,address,uint256,uint256)' \
  --from-block -100
```

---

## Case 2 — Follow-up withdrawal on an existing deploy

**When to use.** Bridge is already unpaused. Bundle daemon has been running
for hours/days. Prior withdrawal (or none) already succeeded; you want to
send another burn through the same rails.

```bash
# 1. Confirm daemon is current (within one bundle of head)
cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC --json | jq -r '.[0]'
# vs chain head via GQL
curl -s -X POST https://shellnet.ackinacki.org/graphql \
  -H 'content-type: application/json' \
  -d '{"query":"{ blockchain { blocks(last: 1) { edges { node { seq_no } } } } }"}' \
  | jq -r '.data.blockchain.blocks.edges[0].node.seq_no'
# lag = head - last_seen. Should be < 1024. If not, wait or run Case 3.

# 2. Fire the burn (same script as Case 1 step 4)
MODE=shellnet python3 python/test_deploy_and_withdraw_only.py

# 3. Run withdraw-e2e --dry-run then real submit (Case 1 steps 6-7)
```

No fresh anchors needed. No redeploy. Bundle daemon's ongoing cadence
covers the event within one W·P stride.

---

## Case 3 — Event captured but daemon far behind head

**Symptom.** `test_deploy_and_withdraw_only.py` captured a burn at
`event_seq_no=E`. On-chain `storedLastSeenBlockSeqNo() = L`. `E − L` >
one bundle stride (i.e. the covering bundle is not yet the next-verified
one).

**Diagnostic.**

```bash
E=<event_seq_no from python output>
L=$(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC --json | jq -r '.[0]')
COVER=$(( (E + 1023) / 1024 * 1024 ))    # next W·P boundary ≥ E
BUNDLES_TO_WAIT=$(( (COVER - L) / 1024 ))
WALL_MIN=$(( BUNDLES_TO_WAIT * 12 ))
echo "event=$E last_seen=$L covering=$COVER  wait ≈ ${WALL_MIN} min"
```

**If `WALL_MIN < 60`.** Just wait. The daemon is doing the right thing;
each successful bundle bumps `L` by 1024. Poll `storedLastSeenBlockSeqNo`
every ~12 min. Once `L ≥ COVER`, run the `withdraw-e2e` command from
Case 1 step 6.

**If `WALL_MIN ≥ 60`.** You may have miscalibrated the fresh-deploy
sequence. Options:

- **Wait it out** — the event is durably captured in the GQL/state; the
  proof will succeed once coverage lands.
- **Abandon and redeploy** — only worth it on a fresh testnet where the
  bridge has no other users. Follow the verifyBlock runbook's
  [Case 6](./live_verifyBlock_runbook.md#case-6--state-loss--re-bootstrap-from-mid-chain)
  to re-seed at a fresh W·P boundary near current head, then re-run the
  burn. This is what Deploy #7 → #8 did on 2026-08-17 (see change log).

Do **not** try to short-circuit by manually advancing `storedLastSeenBlockSeqNo`
— there is no such path. The only way to move `L` forward is via
`verifyBlock`, and `verifyBlock` requires the bundle-proof pipeline.

---

## Case 4 — Prover subprocess timeout / OOM

**Symptom.** `withdraw-e2e` fails during the prove stage:

```
ERROR subprocess_prover: bridge-event-halo2-prover exited status=<code>
   OR
ERROR subprocess_prover: timeout after 1800s
```

**Checks.**

1. Params directory size — Circuit 4 needs the C4 PK (~ several GB) in
   `params/`. `du -sh params/` should be ~17 GB total.
2. Free disk on `params/` filesystem — if a prior run truncated the C4
   PK due to ENOSPC, remove the partial `.pk` and let the next run
   regenerate (adds ~5 min to that cycle).
3. RAM headroom — Circuit 4 K=19 needs ~40 GB peak. If host swap-thrashes,
   the 1800s timeout expires without progress.

**Recovery.** After freeing resources:

```bash
./target/release/relayer withdraw-e2e \
  ...same flags as Case 1 step 6... \
  --prover-timeout-s 3600
```

Bumping `--prover-timeout-s` doesn't fix a real OOM — it just delays the
inevitable. Use only when the pipeline was slow (e.g. cold PK cache), not
when the log shows repeated swap.

---

## Case 5 — On-chain `withdrawByProof` revert

**Symptom.** `withdraw-e2e --dry-run` (or real submit) fails with a
Sepolia revert. The log prints the selector.

**Decode with `cast 4byte`** or via `withdrawByProof`'s declared errors:

| Selector | Error | Root cause pattern |
|---|---|---|
| `AttestationProofRejected()` | SHPLONK adapter equality prelude failed | C4 proof public inputs don't match on-chain-stored values. Most common: `acc_fr` drift (see [`WITHDRAW_ACC_FR` derivation](#reference-addresses-current-deploy)), or `layer_hashes[1]` mismatch (covering bundle not yet verified — you jumped the gun). |
| `WithdrawalAlreadyExecuted(msg_id)` | Same `msg_id` used twice | The `withdraw-e2e` command was re-run against the same captured event. Fire a fresh burn. |
| `AnchorNotFound(key_seq_no)` | Covering bundle's `layer_hashes[1]` not on-chain | Wait for the bundle daemon to submit + confirm the covering bundle, then retry. |
| `PausedError()` | Bridge is paused | `cast send $BRIDGE 'unpause()' --private-key $OWNER_PK`. |

**Dry-run trace (any revert):**

```bash
# Re-run the exact eth_call with --trace for a decoded reason
cast call $BRIDGE \
  'withdrawByProof(bytes,uint256[13])' \
  <calldata_hex_from_log> \
  '[<pi array from log>]' \
  --rpc-url $RPC --trace
```

**Do NOT** delete `state/prover_state.json` or the witness JSON — the
proof is deterministic per `(event, prover_state)`. Fixing the on-chain
side (unpause, wait, redeploy) and re-running the same command
regenerates the same proof against the warm cache.

---

## Case 6 — USDCBridge key drift (burn side)

**Symptom.** `test_deploy_and_withdraw_only.py` fails during
`mintAndSend` step with `exit_code=209` (or similar TVM signature error).

**Root cause pattern.** Bundled `python/contracts/USDCBridge.shellnet.keys.json`
public key ≠ on-chain `getOwnerPubkey`. Historically this has drifted
when Sehor rotated the USDCBridge owner without pushing keys to this
repo (see [`bridge_python_orchestrator_usdc_keys`](../../../memory-hint)).

**Check + fix.**

```bash
cd crates/an-bridge-prover

# 1. Compare local key vs on-chain
LOCAL_PUB=$(jq -r '.public' python/contracts/USDCBridge.shellnet.keys.json)
# On-chain (via tvm-cli or GQL — see python/helper for helpers)
python3 -c "
from python.helper.tonos_helper import get_owner_pubkey  # actual util path
print(get_owner_pubkey(
  address='0:<USDCBridge_addr>',
  gql='https://shellnet.ackinacki.org/graphql',
))"

# 2. If they differ, overlay from acki-nacki config
cp ../../../acki-nacki/config/USDCBridge.keys.json \
   python/contracts/USDCBridge.shellnet.keys.json
```

If neither key matches — the shellnet operator rotated USDCBridge
ownership. Ask Sehor for the current keypair. This is not a bridge bug;
the USDCBridge is external state.

---

## Health checks

**Prove-lane snapshot:**

```bash
# Latest captured witness
ls -lt work_dir/witness_event_*.json 2>/dev/null | head -3

# Latest generated proof
ls -lt proofs/proof_event_*.json 2>/dev/null | head -3

# Circuit 4 PK cache (should be present after first run)
ls -lh params/*event* params/*withdraw* 2>/dev/null
```

**Sepolia snapshot:**

```bash
# All-time WithdrawalExecuted events (deploys are short-lived, so scanning to genesis is cheap)
cast logs --address $BRIDGE --rpc-url $RPC \
  'event WithdrawalExecuted(uint256,address,uint256,uint256)' --from-block 0

# Bridge's escrowed balance
cast call $BRIDGE 'totalEscrowed()(uint256)' --rpc-url $RPC 2>/dev/null || \
  echo "totalEscrowed getter not exposed on this deploy — check via ERC20.balanceOf on the escrow"

# Relayer wallet
cast balance 0x841709B6842233d8474aeA1d773e8d0F7c7c0B9f --rpc-url $RPC --ether
```

---

## File & state reference

Everything lives under `crates/an-bridge-prover/`. Withdrawal E2E adds
three directories on top of the bundle-lane layout in the parent runbook:

```
crates/an-bridge-prover/
├── work_dir/
│   └── witness_event_<seq>.json     ← enriched witness (input to Circuit 4 prover)
├── proofs/
│   └── proof_event_<seq>.json       ← aggregated SHPLONK calldata + PI (env-gated by --prover-out-dir)
├── python/
│   ├── test_deploy_and_withdraw_only.py    ← AN-side burn orchestrator (only external step)
│   └── contracts/
│       └── USDCBridge.shellnet.keys.json   ← USDCBridge owner keypair (must match live chain)
└── logs/
    └── withdraw_<label>_<ts>.log    ← `relayer withdraw-e2e` stdout+stderr
```

**Cleanup rules:**

- `work_dir/`, `proofs/` — safe to prune between demos. Proof
  regeneration is deterministic; only cost is CPU (~5 min per proof with
  warm cache).
- `python/contracts/USDCBridge.shellnet.keys.json` — **NEVER** commit
  changes upstream; keys are shellnet-operator state, not repo state.

---

## Change log / known incidents

Newest first.

### 2026-08-17 — Deploy #8: tight-lookahead rerun after Deploy #7 mis-timing

- **Symptom.** Deploy #7 (`0x822E98…2f90`, seed_seqno=8759296, +262
  lookahead) launched at 09:15 local. Bundle daemon started, but the
  burn (`test_deploy_and_withdraw_only.py`) fired ~30 min later at
  chain head — event landed at seq_no 8767488 while daemon anchor was
  still at 8759296. Covering bundle was 8 bundles ahead of daemon anchor;
  projected wait ≈ 90 min.
- **Root cause.** Between `compute_bridge_anchors --at-head` and the
  actual burn, chain advanced ~8 bundles. The lookahead+lag interacted
  so the event's covering bundle was `daemon_anchor + 8·W·P` instead of
  `daemon_anchor + 1·W·P`.
- **Recovery (this incident).**
  1. Killed the daemon (`kill -9` after SIGTERM was ignored during
     mid-flight aggregator subprocess) and archived `state/` +
     `relayer-state.json` under `state.deploy7_20260817_094240/` and
     `relayer-state.deploy7_20260817_094453.json`.
  2. Re-ran `compute_bridge_anchors --at-head` immediately before the
     new forge deploy. Fresh anchors seed=8768512, +75 lookahead only.
  3. Deploy #8 landed at `0x59dE8848bD5B3F1BD02AF9D269ab313AFa1d900B`
     with all C4 wiring intact (`accFr` canonical `0x1a1a…1a1a`).
  4. Bundle daemon cold-started with `seed_policy=Explicit(8768512)`.
  5. Burn fired at chain head during bundle 1's proving window; event
     captured at seq_no `8770355`. Covering bundle = 8770560 = bundle 2.
  6. Total wall time ~17 min (vs 90 min projected on Deploy #7) — a
     3.3× speedup driven entirely by anchor freshness + burn timing.
- **Rule of thumb landed in this doc.** For fresh demos, run
  `compute_bridge_anchors --at-head` within 3 min of `forge script`, and
  fire the burn within 5 min of daemon startup. Miss either window and
  the wait grows by ~12 min per additional bundle of catch-up.

### 2026-08-17 — `withdraw-e2e` CLI subcommand landed

- **Change.** Commit `4085c5f` added
  `bridge-relayer-daemon/src/withdraw_e2e/{mod.rs, driver.rs, capture.rs}`
  and the `Cmd::WithdrawE2E` CLI variant in `src/bin/relayer.rs`.
  Replaces the Python driver's steps 5–7 (event capture, witness
  enrichment, proving, on-chain submit) with a single in-process
  entry point.
- **First on-chain demo.** Deploy #8, this runbook. Prior E2E validations
  (2026-08-15 seq 8251308 etc.) were daemon-verified via
  `bridge-verifier-daemon`, NOT via on-chain `withdrawByProof`. Deploy #8
  is the first time `withdrawByProof` executes against a live proof
  produced by the Rust relayer.

### 2026-08-13 — Horizontal-chain event proving landed (fire-window dropped)

- **Change.** Commit `7bb3da9` removed the fire-window constraint that
  had forced burns to land inside a specific W·P slot. Combined with
  `2eacdf7` (explicit L1/L2 dispatch in `build_event_anchor_chain`) the
  orchestrator can now anchor an event at any layer L ≥ 1 as long as the
  covering bundle is verified.
- **Consequence for this runbook.** No fire-window guard. Just fire the
  burn — `--anchor-layer auto` picks L1 for anything within one bundle
  of the covering key, L2 for anything within `W` bundles, etc.
  `--i-know-the-wait` is only needed if you're forcing an explicit
  layer ≥ 2 (rarely the right call for demos).

### 2026-08-06 — `WIRE_WITHDRAW_BY_PROOF` mandatory outside anvil

- **Change.** Commit `a43993b` in the contracts repo made C4 wiring
  mandatory on any chain other than anvil (chainid 31337). Sepolia
  deploys **must** provide `WITHDRAW_ACC_FR` at construction; the
  constructor rejects zero.
- **Consequence.** Never set `WIRE_WITHDRAW_BY_PROOF=false` for
  Sepolia. Every deploy from #4 onward has C4 baked in.

---

**Editing this runbook.** Match the parent runbook's rules: dated
incidents, one-paragraph, cite commits/paths. Prune once the underlying
change has been stable for >30 days across demos.
