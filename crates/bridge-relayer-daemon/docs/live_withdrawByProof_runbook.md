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
[`live_relayer_bridge_verifyBlock_runbook.md`](./live_relayer_bridge_verifyBlock_runbook.md), which
this doc extends.

> **Notation.** `seq_no` = Acki Nacki block sequence number.
> "Covering bundle" = the first bundle whose `key_seq_no ≥ event_seq_no`
> that is verified on-chain. Withdrawals can only be submitted after
> their covering bundle lands (Circuit 4 anchor-chain verifies against
> a `layer_hashes` root that `verifyBlock` has already committed).
> With `W=128, P=8`, an **L1** bundle covers `W·P = 1024` seq_nos
> (~5.7 min chain-time at 3 seq/s). An **L2** bundle covers
> `W² = 16384` seq_nos (~91 min chain-time). Case 8+ covers the L2 flow.

---

## Table of Contents

- [Quick resume checklist (returning mid-flow)](#quick-resume-checklist-returning-mid-flow)
- [Timing model — why fresh-deploy demos need tight lookahead](#timing-model--why-fresh-deploy-demos-need-tight-lookahead)
- [Reference values (chain-invariant on shellnet)](#reference-values-chain-invariant-on-shellnet)
- [Binary + env prerequisites](#binary--env-prerequisites)
- [Case 1 — First-time E2E from a fresh deploy (optimal sequence)](#case-1--first-time-e2e-from-a-fresh-deploy-optimal-sequence)
- [Case 2 — Follow-up withdrawal on an existing deploy](#case-2--follow-up-withdrawal-on-an-existing-deploy)
- [Case 3 — Event captured but daemon far behind head](#case-3--event-captured-but-daemon-far-behind-head)
- [Case 4 — Prover subprocess timeout / OOM](#case-4--prover-subprocess-timeout--oom)
- [Case 5 — On-chain `withdrawByProof` revert](#case-5--on-chain-withdrawbyproof-revert)
- [Case 6 — USDCBridge key drift (burn side)](#case-6--usdcbridge-key-drift-burn-side)
- [Case 7 — `WithdrawTreasuryShortfall` — bridge treasury empty](#case-7--withdrawtreasuryshortfall--bridge-treasury-empty)
- [Case 8 — Fresh L2 deploy: first E2E withdrawal](#case-8--fresh-l2-deploy-first-e2e-withdrawal)
- [Case 9 — Sequential L2 withdrawals (stress-test loop)](#case-9--sequential-l2-withdrawals-stress-test-loop)
- [L2 timing model](#l2-timing-model)
- [Health checks](#health-checks)
- [File & state reference](#file--state-reference)
- [Change log / known incidents](#change-log--known-incidents)

---

## Quick resume checklist (returning mid-flow)

```bash
cd /Users/alinat/HALO2_TVM_EXPERIMENTS/bridge/crates/an-bridge-prover

# Source the per-mode env — pick L1_config or L2_config depending on the
# lane you were running. 
export BRIDGE_CONFIG_DIR=./L1_config       # or ./L2_config for the L2 lane
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
export BRIDGE=$BRIDGE_ADDRESS              # short alias used below
export RPC=$RPC_URL

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

The Circuit 4 proof is anchored to `layer_hashes[L]` of the **covering
bundle** at anchor layer `L` — the first bundle whose layer-`L` key
seq_no is ≥ `event_seq_no` (stride = 1024 for L1, 16384 for L2).
`withdrawByProof` reverts until `verifyBlock(covering_bundle)` has
landed. End-to-end wall time =

```
t_e2e = max(0, covering_bundle_seqno − daemon_last_verified) / bundle_stride
        × prover_wall_time_per_bundle              # daemon catch-up
      + prover_wall_time_per_bundle                # covering bundle itself
      + circuit_4_prove_time + submit_time         # withdraw leg
```

Stride and per-bundle wall time depend on `L`: **L1** = `W·P = 1024`
seq_nos (~5.7 min chain-time, ~12 min prover); **L2** = `W² = 16384`
seq_nos (~91 min chain-time, ~101 min prover — see
[L2 timing model](#l2-timing-model) for the level=2 table). L1 fast-case
table (warm PK cache, ~5 min for C4+submit):

| daemon lag at burn (bundles) | catch-up | +covering | +C4+submit | **total** |
|---|---|---|---|---|
| 0 (burn during bundle 1 window) | 0 | 12 | 5 | **~17 min** |
| 1 (burn during bundle 2 window) | 12 | 12 | 5 | **~29 min** |
| 7 (Deploy #7 accident, see change log) | 84 | 12 | 5 | **~101 min** |

**Consequence for fresh demos.** Genesis anchors baked into
`L{1,2}_config/env` by `compute_bridge_anchors --level {1|2} --at-head`
embed the chain-head lookahead at the time you run the tool. That
lookahead **is** the daemon's starting lag. Two rules:

1. **L1 only — run `deploy_bridge_bundle.sh` immediately after
   `compute_bridge_anchors`.** Chain moves at 3 b/s (~180 seq_nos/min)
   while your seed stays fixed; a 10 min gap costs a whole bundle of
   L1 catch-up because the L1 daemon is subcritical (τ ≈ 13.5 min per
   bundle vs 5.7 min chain-time, so accumulated gap never closes).
   Under L2 this rule does not apply: L2 is supercritical and the wait
   is bounded (~1 h avg, ~2 h ceiling) independent of anchor freshness
   or daemon uptime by construction.
2. **Fire the burn during the daemon's first bundle-processing window
   (L1 only).** With `--anchor-layer auto`, `withdraw-e2e` waits through
   `--event-wait-s` and uses covering bundle 1 or 2. L2 has no analogous
   fast case — bundle 1 already costs ~101 min regardless of burn timing.

---

## Reference values (chain-invariant on shellnet)

Per-deploy addresses (`BRIDGE_ADDRESS`, the four aggregator verifiers,
`MockBlockHeaderOracle`, `BRIDGE_BOOTSTRAP_SEQNO`, genesis
`prev_max_level_layer_hash`) rotate on every redeploy — read them from
`crates/an-bridge-prover/L{1,2}_config/env` (rewritten by
`scripts/deploy_bridge_bundle.sh` on each deploy), not from this doc.
Deployer / relayer / owner wallet is the single shared shellnet burner
`0xb586356D52eAee055Ca569Ff412DFeFFc5bB2307` documented in
[`shellnet.common`](../../an-bridge-prover/shellnet.common) or smth that you deployed yourself (see instruction how to deploy and fund your wallet here [`live_relayer_bridge_verifyBlock_runbook.md`](./live_relayer_bridge_verifyBlock_runbook.md)).

Two values are chain-invariant on shellnet and worth naming here:

| Item | Value |
|---|---|
| Genesis `bk_set_commitment` | `0x08eb0a1892e4f75a8b5c8cff69322f95bf0437c371903998c9365fbe293ca71c` |
| `WITHDRAW_ACC_FR` (USDCBridge account_id as Fr) | `0x1a1a…1a1a` (canonical, palindromic — see below) |

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
submit will revert on the equality check — **stop and redeploy** with the
correct `WITHDRAW_ACC_FR` exported before invoking
[`crates/an-bridge-prover/scripts/deploy_bridge_bundle.sh`](../../an-bridge-prover/scripts/deploy_bridge_bundle.sh).

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
# BRIDGE_CONFIG_DIR must already be exported (./L1_config or ./L2_config)
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
for v in RPC_URL BRIDGE_ADDRESS RELAYER_PRIVATE_KEY BRIDGE_GQL_ENDPOINT; do
  [ -n "${!v}" ] && echo "  ok  $v" || echo "  FAIL $v"
done
```

`shellnet.common` holds `RPC_URL`, `RELAYER_PRIVATE_KEY`,
`BRIDGE_GQL_ENDPOINT`, `BRIDGE_BK_SET_CONFIG`, `BRIDGE_PARAMS_DIR`,
`BRIDGE_AGGREGATOR_DIR`, `BRIDGE_VERIFIERS_DIR`; `$BRIDGE_CONFIG_DIR/env` sources it
and layers on `BRIDGE_ADDRESS`, `BRIDGE_BOOTSTRAP_SEQNO`,
`BRIDGE_ANCHOR_LEVEL`, `BRIDGE_CONFIG_DIR`. The withdraw scripts
under `scripts/launch_withdraw_e2e*.sh` source the same per-mode env
file before invoking the binary.

The withdraw command reads `RPC_URL`, `BRIDGE_ADDRESS`, `RELAYER_PRIVATE_KEY`
via clap `env` attrs; `BRIDGE_GQL_ENDPOINT` must be aliased to
`GQL_ENDPOINT` on the command line (see Case 1).

---

## Case 1 — First-time E2E from a fresh deploy (optimal sequence)

**When to use — L1 only, one-shot fresh-deploy demo.** Prove out the
`withdrawByProof` leg once, in minimum wall time (~17 min best-case).
This is a **tight sequence**: bridge Ethereum contracts deploy →
bundle-prover daemon cold-starts → burn fires, all inside one narrow
window. The L1 daemon is subcritical (see
[Timing model](#timing-model--why-fresh-deploy-demos-need-tight-lookahead))
so its lag grows forever — a second withdraw against the same L1 deploy
soon slides past the wait-time budget. For **regular, repeated**
withdrawals use L2 mode:
[Case 8](#case-8--fresh-l2-deploy-first-e2e-withdrawal) (first L2 cycle)
and [Case 9](#case-9--sequential-l2-withdrawals-stress-test-loop)
(stress loop).

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

The printed anchors do not need to be pasted anywhere by hand —
[`crates/an-bridge-prover/scripts/deploy_bridge_bundle.sh`](../../an-bridge-prover/scripts/deploy_bridge_bundle.sh)
re-derives them internally and writes the corresponding
`L{1,2}_config/env`. The manual `compute_bridge_anchors` call above is
only useful for a freshness sanity-check before triggering the script.

### Step 1 — Deploy the bridge

Delegate to the automated wrapper — it derives anchors against fresh
chain head, deploys the 6-bridge contract bundle in Ethereum, extracts `BRIDGE_ADDRESS`,
and rewrites `L1_config/env`:

```bash
cd crates/an-bridge-prover
set -a && source shellnet.common && set +a
PRIVATE_KEY=$RELAYER_PRIVATE_KEY LEVEL=1 ./scripts/deploy_bridge_bundle.sh
```

### Step 2 — Seed the bridge treasury (fresh deploy only)

`withdrawByProof` pays out from `treasuryBalance` (`AckiNackiBridge.sol:1188`).
A fresh deploy starts at zero; the crypto path can pass and the tx will
still revert with `WithdrawTreasuryShortfall(pub.amount, treasuryBalance)`
(selector `0xbb651fce`). The only path that increments `treasuryBalance`
is `deposit()` (`AckiNackiBridge.sol:578-593`) — there is no admin setter.
Seed it once, then reuse across demos on the same deploy.

Full recipe in [Case 7](#case-7--withdrawtreasuryshortfall--bridge-treasury-empty).
Quick version — mint 1 USDC from the Aave Sepolia faucet, deposit into
the bridge:

```bash
export BRIDGE=<new_address>
export USDC=0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8
export FAUCET=0xC959483DBa39aa9E78757139af0e9a2EDEb3f42D
export WALLET=0xb586356D52eAee055Ca569Ff412DFeFFc5bB2307      # relayer/deployer (single shared burner)
export AMOUNT=1000000                                          # 1.000000 USDC (6 decimals)

# 1. Mint test USDC to the wallet (permissionless faucet)
cast send $FAUCET 'mint(address,address,uint256)' $USDC $WALLET $AMOUNT \
  --rpc-url $RPC --private-key $RELAYER_PRIVATE_KEY

# The seeding uses TWO contracts, TWO methods:
#   * USDC.approve(bridge, amount)   — ERC20 allowance on the token contract
#   * bridge.deposit(amount, …)      — the actual bridge call
# Only step 3 touches the bridge. Step 2 is a plain ERC20 approve on USDC;
# it authorizes the `usdc.transferFrom(msg.sender, ...)` that deposit()
# runs internally (AckiNackiBridge.sol:585).

# 2. USDC.approve(spender=$BRIDGE, value=$AMOUNT) — target contract is $USDC
cast send $USDC 'approve(address,uint256)' $BRIDGE $AMOUNT \
  --rpc-url $RPC --private-key $RELAYER_PRIVATE_KEY

# 3. bridge.deposit($AMOUNT, workchain=0, anAccount=0x11…11) — target contract is $BRIDGE
#    (dummy AN destination is harmless on testnet — no AN listener will
#    credit this phantom Deposit event)
cast send $BRIDGE 'deposit(uint256,int8,bytes32)' \
  $AMOUNT 0 0x1111111111111111111111111111111111111111111111111111111111111111 \
  --rpc-url $RPC --private-key $RELAYER_PRIVATE_KEY

# 4. Verify
cast call $BRIDGE 'treasuryBalance()(uint256)' --rpc-url $RPC   # -> 1000000
```

### Step 3 — Cold-start the bundle daemon

Full procedure in the [verifyBlock runbook Case 1](./live_relayer_bridge_verifyBlock_runbook.md#case-1--first-time-bootstrap-from-a-fresh-deploy).
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
# Chain progress ($BRIDGE / $RPC come from the sourced $BRIDGE_CONFIG_DIR/env — see
# Quick resume checklist; do NOT paste an address literal here, it rotates per deploy)
watch -n 30 'cast call $BRIDGE storedLastSeenBlockSeqNo\(\)\(uint64\) --rpc-url $RPC'

# Wait until chain last_seen ≥ (event_seq_no rounded UP to next 1024 boundary)
```

### Step 6 — Run `withdraw-e2e --dry-run`

```bash
cd crates/an-bridge-prover
# BRIDGE_CONFIG_DIR must already be exported (./L1_config or ./L2_config)
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
TS=$(date +%Y%m%d_%H%M%S)

./target/release/relayer withdraw-e2e \
  --gql-endpoint $BRIDGE_GQL_ENDPOINT \
  --prover-state-path "$BRIDGE_CONFIG_DIR/state/prover_state.json" \
  --window-size 128 \
  --bridge-account-id 1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a \
  --bridge-dapp-id    1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a \
  --anchor-layer auto \
  --work-dir "$BRIDGE_CONFIG_DIR/work_dir" \
  --an-bridge-prover-dir . \
  --prover-out-dir "$BRIDGE_CONFIG_DIR/proofs" \
  --prover-seq-no $(date +%s) \
  --dry-run \
  2>&1 | tee logs/withdraw_dry_${BRIDGE_CONFIG_DIR##*/}_${TS}.log
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

**When to use.** Bridge is already deployed. Bundle daemon has been
running for hours/days. Prior withdrawal (or none) already succeeded;
you want to send another burn through the same rails.

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
  [Case 6](./live_relayer_bridge_verifyBlock_runbook.md#case-6--state-loss--re-bootstrap-from-mid-chain)
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
| `AttestationProofRejected()` | SHPLONK adapter equality prelude failed | C4 proof public inputs don't match on-chain-stored values. Most common: `acc_fr` drift (see [`WITHDRAW_ACC_FR` derivation](#reference-values-chain-invariant-on-shellnet)), or `layer_hashes[1]` mismatch (covering bundle not yet verified — you jumped the gun). |
| `WithdrawalAlreadyExecuted(msg_id)` | Same `msg_id` used twice | The `withdraw-e2e` command was re-run against the same captured event. Fire a fresh burn. |
| `AnchorNotFound(key_seq_no)` | Covering bundle's `layer_hashes[1]` not on-chain | Wait for the bundle daemon to submit + confirm the covering bundle, then retry. |
| `WithdrawTreasuryShortfall(uint256,uint256)` = `0xbb651fce` | `pub.amount > treasuryBalance` (AckiNackiBridge.sol:1188) | Crypto path already passed; only the payout leg is blocked. Seed the treasury via `deposit()` — see [Case 7](#case-7--withdrawtreasuryshortfall--bridge-treasury-empty). |

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
side (wait, redeploy) and re-running the same command regenerates the
same proof against the warm cache.

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

## Case 7 — `WithdrawTreasuryShortfall` — bridge treasury empty

**Symptom.** Dry-run (or real submit) reverts with selector `0xbb651fce`
decoded as `WithdrawTreasuryShortfall(<pub.amount>, <treasuryBalance>)`.
`treasuryBalance == 0` on a fresh deploy is the common case; a partial
seed followed by a larger burn is the other.

**Why this happens.** In real cross-chain operation, `treasuryBalance` is
grown by users bridging IN (`deposit()`), and payouts on the AN→ETH leg
draw from that pool. Shellnet demos usually burn on the AN side without a
prior ETH→AN deposit — so the treasury never funds itself organically.
`AckiNackiBridge.sol:1188` enforces `pub.amount > treasuryBalance` →
revert; there is no admin bypass and no auto-supply from AAVE (the AAVE
integration is a yield sink for surplus, not a payout source).

**Fix — mint from Aave faucet + deposit.** Testnet USDC lives at the
Aave Sepolia market address; the same faucet the fork tests use
(`test/AckiNackiBridgeAaveFork.t.sol:68-73`) has a permissionless
`mint(address token, address to, uint256 amount)`:

```bash
cd crates/an-bridge-prover
# BRIDGE_CONFIG_DIR must already be exported (./L1_config or ./L2_config)
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a

export USDC=0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8
export FAUCET=0xC959483DBa39aa9E78757139af0e9a2EDEb3f42D
export WALLET=$(cast wallet address --private-key $RELAYER_PRIVATE_KEY)
export AMOUNT=1000000    # cover at least one burn (1.000000 USDC)

# 1. Mint test USDC into the relayer wallet
cast send $FAUCET 'mint(address,address,uint256)' $USDC $WALLET $AMOUNT \
  --rpc-url $RPC_URL --private-key $RELAYER_PRIVATE_KEY

cast call $USDC 'balanceOf(address)(uint256)' $WALLET --rpc-url $RPC_URL   # should show AMOUNT

# TWO contracts, TWO methods — only step 3 touches the bridge:
#   * USDC.approve(bridge, amount)   — ERC20 allowance on the token contract
#   * bridge.deposit(amount, …)      — the actual bridge call, which pulls
#                                      via usdc.transferFrom(...) internally
#                                      (AckiNackiBridge.sol:585)

# 2. USDC.approve(spender=$BRIDGE_ADDRESS, value=$AMOUNT) — target is $USDC
cast send $USDC 'approve(address,uint256)' $BRIDGE_ADDRESS $AMOUNT \
  --rpc-url $RPC_URL --private-key $RELAYER_PRIVATE_KEY

# 3. bridge.deposit(...) — target is $BRIDGE_ADDRESS
#    (dummy AN destination — harmless on testnet)
cast send $BRIDGE_ADDRESS 'deposit(uint256,int8,bytes32)' \
  $AMOUNT 0 0x1111111111111111111111111111111111111111111111111111111111111111 \
  --rpc-url $RPC_URL --private-key $RELAYER_PRIVATE_KEY

# 4. Confirm
cast call $BRIDGE_ADDRESS 'treasuryBalance()(uint256)' --rpc-url $RPC_URL   # -> AMOUNT
```

**Why the dummy AN destination is safe on testnet.** `deposit()` emits a
`Deposit(depositId, msg.sender, amount, anWorkchain, anAccount, ts)` event
that a production AN-side listener would consume to credit the AN
recipient. On shellnet demos there is no such listener wired for
seed-only deposits, so the phantom event just sits in event history. Do
NOT do this on a bridge with a live AN-side indexer — pay to a real
`anAccount` you control on that path, or drain via a legit withdraw
after seeding.

**Re-run the withdraw.** After the deposit lands, re-run the same
`withdraw-e2e` (or `replay_withdraw_shplonk.sh`) command. The proof is
deterministic per `(event, prover_state)`, so if it dry-ran successfully
against the empty treasury it will submit successfully now.

**Scaling.** Seed size to cover the burn(s) you plan to test. Default
`test_deploy_and_withdraw_only.py` fires 1 USDC — mint 10 USDC once and
you're good for the demo cycle. Excess USDC in the treasury is not lost:
it stays available for future withdraws, and can optionally be swept to
AAVE via `supplyToAave()` for yield.

---

## Case 8 — Fresh L2 deploy: first E2E withdrawal

**When to use.** Deploying the bridge with `BRIDGE_ANCHOR_LEVEL=2` to
exercise L2-anchoring end-to-end. Everything from Case 1 applies with
three level-swaps below; the differences below are the only L2-specific
deviations.

**Level-swap summary.**

| L1 setting | L2 setting |
|---|---|
| `BRIDGE_ANCHOR_LEVEL=1` (or unset) | `BRIDGE_ANCHOR_LEVEL=2` |
| Bundle stride = `W·P = 1024` seq_nos (~5.7 min chain-time) | Bundle stride = `W² = 16384` seq_nos (~91 min chain-time) |
| `compute_bridge_anchors` picks `layer == 1` | `compute_bridge_anchors --level 2` picks `layer == 2` |
| `withdraw-e2e --anchor-layer auto` | `withdraw-e2e --anchor-layer 2 --i-know-the-wait` (strict L2) |
| Fire burn within ~5 min of daemon start | Burn timing is irrelevant to floor wait (see L2 timing model) |

### Step L0 — Emit L2 genesis anchors

L2 seed emission requires the daemon to have observed at least one T₂
(W²-aligned) boundary. Chain reaches a T₂ every ~91 min, so if your
freshness window is short, `compute_bridge_anchors --level 2` may return
"no L2 boundary seen yet, retry after next T₂". Wait or re-run.

```bash
cd crates/an-bridge-prover/bridge-prover-lib
cargo run --release --bin compute_bridge_anchors -- \
  --level 2 \
  --at-head \
  --gql-endpoint https://shellnet.ackinacki.org/graphql
# Emits:
#   GENESIS_ANCHOR_LEVEL=2
#   GENESIS_LAST_SEEN_BLOCK_SEQ_NO=<W²-aligned seq_no>
#   GENESIS_PREV_MAX_LEVEL_LAYER_HASH=<L2 T₂ root>
#   GENESIS_BK_SET_COMMITMENT=<current BK-set Poseidon commitment>
```

**L2 freshness rule.** Chain moves W² every ~91 min. A staleness of
10 min before `forge script` runs costs ~11% of one bundle vs L1's
"10 min = one whole bundle" — so L2 is much more forgiving on the
compute-anchors → deploy gap. Still keep it under ~10 min for cleanliness.

As with L1, the emitted values do not need hand-copying —
`deploy_bridge_bundle.sh` re-derives them for `--level 2` and rewrites
`crates/an-bridge-prover/L2_config/env` (which sources
`../shellnet.common` and only overrides the 4 L2-specific lines).

### Step L1 — Deploy with L2 wiring

Same wrapper as Case 1 Step 1 with `LEVEL=2` — the constructor is
level-opaque (see change log below), so only the env values differ:

```bash
cd crates/an-bridge-prover
set -a && source shellnet.common && set +a
PRIVATE_KEY=$RELAYER_PRIVATE_KEY LEVEL=2 ./scripts/deploy_bridge_bundle.sh
```

**Post-deploy sanity — confirm W²-alignment on-chain:**

```bash
export BRIDGE=<new_address>
LAST=$(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC --json | jq -r '.[0]')
python3 -c "print('L2-aligned:', $LAST % 16384 == 0, 'last_seen:', $LAST)"
```

If `L2-aligned == False`, the deploy consumed an L1 seed by mistake —
**redeploy**. The daemon's startup stride-alignment check will refuse to
run against a non-W²-aligned contract when `BRIDGE_ANCHOR_LEVEL=2`.

### Step L2 — Seed the bridge treasury

Identical to Case 1 Step 2 — level-agnostic.

### Step L3 — Cold-start `daemon-live` under L2

Provide `--anchor-level 2` (or `BRIDGE_ANCHOR_LEVEL=2` in env). Everything
else per the verifyBlock runbook Case 1.

```bash
export BRIDGE_ANCHOR_LEVEL=2
./target/release/relayer daemon-live \
  --anchor-level 2 \
  ... all other flags per verifyBlock runbook Case 1 ...
```

**Expected log signature (within 30s of start):**

```
INFO daemon-live: anchor_mode=L2, bundle_stride=16384
INFO daemon-live: on-chain last_seen=<seed>, stride-aligned=OK
INFO daemon-live: seed_policy=Explicit(<seed>), anchor_level=2
```

**Startup drift refusals.** If either of the following appears, stop
and fix before proceeding:

- `refuse: on-chain last_seen (=X) % 16384 != 0` — deploy consumed L1
  seed but daemon is starting L2. Redeploy with L2 genesis values.
- `refuse: anchor_level mismatch (state=1, cfg=2)` — stale L1
  `L2_config/state/prover_state.json` re-used across the redeploy (or
  the wrong `$BRIDGE_CONFIG_DIR` was sourced). Delete the file (or start with a fresh
  `L2_config/state/`) and restart to force re-seed.

### Step L4 — Wait for the first L2 bundle to land

Under L1 this is ~5.7 min chain + ~10 min prover. Under **L2** it is
~91 min chain + ~10 min prover ≈ **101 min worst-case, ~50 min typical**
(depending on when start relative to next T₂).

```bash
watch -n 60 'cast call $BRIDGE storedLastSeenBlockSeqNo\(\)\(uint64\) --rpc-url $RPC'
# Wait until the value bumps by exactly 16384 (one L2 stride).
```

Once the value increments, both `_layerWindows[1]` (L1) and
`_layerWindows[2]` (L2) are populated on-chain in a single
`verifyBlock` call — `AckiNackiBridge.sol:_appendLayerHashes` iterates
all active layer slots per successful proof (lines 902–913).

### Step L5 — Fire the burn

Same as Case 1 Step 4 (`test_deploy_and_withdraw_only.py`); the burn
side is level-agnostic. Note the printed `seq_no` — it feeds the
covering-bundle math below.

### Step L6 — Wait for covering L2 bundle

```bash
E=<event_seq_no from python output>
COVER=$(( (E + 16383) / 16384 * 16384 ))    # next W²-boundary ≥ E
L=$(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC --json | jq -r '.[0]')
python3 -c "print(f'event={$E} last_seen={$L} covering_T2={$COVER}  wait≈{max(0,($COVER-$L))*0.5/60:.0f} min chain + ~10 min prover')"
# Poll storedLastSeenBlockSeqNo until >= COVER
```

### Step L7 — Run `withdraw-e2e` with **explicit** L2

**Do NOT use `--anchor-layer auto`** under L2 stress testing. Auto
probes L1 first; whenever the event falls inside the L1 window that
follows the covering T₂ (the common case) it resolves to L1 and the
run silently downgrades. Force strict L2:

```bash
cd crates/an-bridge-prover
BRIDGE_CONFIG_DIR=./L2_config
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
TS=$(date +%Y%m%d_%H%M%S)

./target/release/relayer withdraw-e2e \
  --gql-endpoint $BRIDGE_GQL_ENDPOINT \
  --prover-state-path "$BRIDGE_CONFIG_DIR/state/prover_state.json" \
  --window-size 128 \
  --bridge-account-id 1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a \
  --bridge-dapp-id    1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a \
  --anchor-layer 2 \
  --i-know-the-wait \
  --work-dir "$BRIDGE_CONFIG_DIR/work_dir" \
  --an-bridge-prover-dir . \
  --prover-out-dir "$BRIDGE_CONFIG_DIR/proofs" \
  --prover-seq-no $(date +%s) \
  --dry-run \
  2>&1 | tee logs/withdraw_l2_dry_${TS}.log
```

**Expected log signature (differences vs L1):**

```
INFO enrich_witness: anchor_layer_mode=Explicit(2), i_know_the_wait=true
INFO enricher: filling ... timeout_s=7200        # 2 h (post-2026-08-18)
INFO resolved anchor: L2 (mode=Explicit(2), auto_escalated=false)
INFO chain built: anchor_layer=L2, active_links=1, ...
INFO enricher: witness ready  layer_idx=1        # 0-indexed → L2
```

The `layer_idx=1` line is the ground-truth confirmation that the
witness is L2-anchored; anything else (`layer_idx=0`) means an
accidental L1 fallback happened. Under `Explicit(2)` this cannot occur
by construction — the enricher passes the level through and the slot
lookup indexes `layer_windows[1]` unconditionally.

Then real submit — same as Case 1 Step 7.

**If the enricher timeout expires** (currently `ENRICH_TIMEOUT = 120 min`
per `bridge-relayer-daemon/src/withdraw_e2e/driver.rs:172`), the daemon
never landed a covering L2 bundle inside 2 h. That is a bundle-lane
issue, not a withdraw-lane issue — check `daemon-live` logs and the
verifyBlock runbook.

---

## Case 9 — Sequential L2 withdrawals (stress-test loop)

**When to use.** After Case 8's first cycle succeeds, drive 2–3
additional burns through the same L2 rails to exercise repeated L2
anchoring under real chain motion.

**Session budget.** With one bundle ~101 min end-to-end and ~5 min per
`withdraw-e2e` cycle, expect ~2 h between successful withdrawals.
Three cycles fit in a ~6 h operator session.

**Loop shape (per cycle N = 2, 3, …):**

```bash
# 1. Confirm previous WithdrawalExecuted landed
cast logs --address $BRIDGE --rpc-url $RPC \
  'event WithdrawalExecuted(uint256,address,uint256,uint256)' \
  --from-block -1000 | tail -5

# 2. Confirm treasury still funded (seed 3–5 USDC once at Case 8 Step L2)
cast call $BRIDGE 'treasuryBalance()(uint256)' --rpc-url $RPC

# 3. Fire fresh burn
MODE=shellnet python3 python/test_deploy_and_withdraw_only.py

# 4. Wait for covering T₂ (Case 8 Step L6 math)

# 5. Run withdraw-e2e with a fresh --prover-seq-no
./target/release/relayer withdraw-e2e \
  ... same flags as Case 8 Step L7 ... \
  --prover-seq-no $(date +%s)
```

**What to watch between cycles.**

- Daemon should log a fresh Circuit-2 bundle every ~91 min.
- On-chain `LayerAnchorAppended` should fire **twice per bundle**
  (once for L1 slot, once for L2 slot). Missing L2 emissions signal a
  lost `verifyBlock` on the bundle lane — pause and inspect the daemon
  before continuing the loop.
- Between cycles, `storedLastSeenBlockSeqNo` should be a multiple of
  16384 modulo bundle count. Off-multiple = state divergence.

**Failure isolation.** Because the on-chain anchor path is level-opaque
(constructor stores a single genesis scalar; `_expectedPrevAnchor` uses
per-layer picks; `withdrawByProof`'s `_isKnownAnchor` scans all
layers — see change log), any revert in cycle N ≥ 2 is almost
certainly reproducing a Case 5 failure mode, not something L2-specific.
Start with the Case 5 catalog before diagnosing L2.

---

## L2 timing model

Extends [Timing model — why fresh-deploy demos need tight lookahead](#timing-model--why-fresh-deploy-demos-need-tight-lookahead).
Every "12 min" in the L1 formula becomes "~101 min" under L2. There is
no L2 fast-case analogous to L1's "burn inside bundle 1 window ≈ 17 min".

| Scenario | catch-up | +covering | +C4+submit | **total** |
|---|---|---|---|---|
| Burn during bundle-1 proving window, L1 anchor unavailable | 0 | 101 | 5 | **~106 min** |
| Burn just after bundle-1 T₂ landed | 101 | 0 | 5 | **~106 min** |
| Burn during bundle-2 proving window | 0 | 101 | 5 | **~106 min** |
| Daemon lag = 1 L2 bundle at burn time | 101 | 101 | 5 | **~207 min** |

**Enrich timeout.** `ENRICH_TIMEOUT` was bumped from 90 → 120 min in
`bridge-relayer-daemon/src/withdraw_e2e/driver.rs:172` to fit the L2
worst-case (~101 min chain + ~10 min prover + 10 min slack). Two hours
is snug — if a bundle stalls beyond that, `withdraw-e2e` errors out and
the operator must diagnose the bundle lane before retrying.

**Consequence for demos.** L2 stress testing is not a "quick demo".
Plan for a half-day session per 3-cycle run, and treat the ~50 min
typical between-cycle wait as unavoidable. L2's value is stress-testing
the deeper anchor chain — timing efficiency is not the goal.

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
cast balance 0xb586356D52eAee055Ca569Ff412DFeFFc5bB2307 --rpc-url $RPC --ether
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

### 2026-08-18 — L2 anchoring code-complete + operator readiness (pre-Deploy #12)

- **Change.** All 8 stages of
  [`docs/l2_anchoring_implementation_plan.md`](./l2_anchoring_implementation_plan.md)
  landed (commit `bf0d41a` closes stages 5–8; `c7923c7` covers 1–4).
  Level-parametric across the stack:
  `compute_bridge_anchors --level {1|2}`, `daemon-live --anchor-level`
  (env `BRIDGE_ANCHOR_LEVEL`), and the existing
  `withdraw-e2e --anchor-layer {auto|1|2} --i-know-the-wait`.
  `BootstrapSeed` v2 and `BridgeState` v5 persist `anchor_level` for
  cross-startup drift detection; the relayer daemon refuses on-chain
  stride mis-alignment at boot.
- **`ENRICH_TIMEOUT` bumped 90 → 120 min.** File
  `bridge-relayer-daemon/src/withdraw_e2e/driver.rs:172`. L2's worst-case
  single-bundle wait is ~101 min chain + ~10 min prover; the previous
  90-min budget was L1-tuned and would time out before the enricher
  could resolve a fresh T₂ boundary. L1 unaffected in healthy runs
  (typical L1 resolve time is seconds).
- **Contract is level-opaque — confirmed by code read.**
  `AckiNackiBridge.sol` constructor (lines 509–537) stores only a
  scalar `_vb.genesisPrevMaxLevelLayerHash`; `_layerWindows` starts
  empty. `_expectedPrevAnchor` (lines 990–997) returns that scalar iff
  `_highestActiveLayer() == 0` (first verifyBlock). After the first
  successful proof, `_appendLayerHashes` (lines 902–913) writes both
  `_layerWindows[1]` and `_layerWindows[2]` in one call when the proof
  carries `numLayers = 2`. No Solidity change required for L2 deploys.
- **Cases 8, 9, and L2 timing model added to this runbook.** Case 8 is
  the operator sequence for the first L2 E2E cycle; Case 9 is the
  sequential-withdrawal stress loop (2–3 cycles per session).
- **Not yet exercised live.** Deploy #12 (first L2 Sepolia deploy) has
  not run yet. Case 8's dry-run and Case 9's loop will land as a
  subsequent change-log entry once executed.

### 2026-08-18 — Deploy #10 first live `WithdrawalExecuted` + treasury-seeding case

- **Milestone.** First successful on-chain `withdrawByProof` against
  `AckiNackiBridge 0xa44E35151962684f54Af8aaD2675E726ED848E59` — tx
  `0x35d7254b430f1e475ef16d7f60b295bd0226c906ec5b019d4b2ca408ca657c85`
  at Sepolia block 11,513,799 (`WithdrawalExecuted` emitted; 1.000000
  USDC delivered to `0x742d35Cc…f44e`).
- **Ordering discovery.** After the SHPLONK pipeline fix (commit
  `b22f6c7`, driver.rs now composes `Circuit4ShplonkPipeline`), dry-run
  passed the crypto path (anchor check + verifier both green) but the
  submit reverted with `WithdrawTreasuryShortfall(1_000_000, 0)`
  (selector `0xbb651fce`). Root cause was operational, not
  cryptographic: Deploy #10 was freshly bootstrapped and no one had ever
  bridged IN, so `treasuryBalance == 0`.
- **Fix landed in this runbook.** New [Case 7](#case-7--withdrawtreasuryshortfall--bridge-treasury-empty)
  with the Aave-faucet recipe (`FAUCET.mint(USDC, wallet, amount)` →
  `USDC.approve(bridge)` → `bridge.deposit(amount, 0, 0x11…11)`). Also
  added a treasury-seed step (now
  [Step 2](#step-2--seed-the-bridge-treasury-fresh-deploy-only)) to
  Case 1 so future fresh-deploy demos do the seed BEFORE firing the
  burn, and added the selector row to Case 5's revert table.
- **Rule of thumb.** Fresh deploys must seed the treasury or every
  `withdrawByProof` will revert on the payout leg regardless of proof
  quality. Faucet + deposit costs ~2 tx (<30s wall), fund enough to cover
  the demo's burns. Existing deploys inherit their prior treasury; check
  with `cast call $BRIDGE 'treasuryBalance()(uint256)'`.

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
