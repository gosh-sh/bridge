# Live `verifyBlock` E2E Runbook — shellnet → Sepolia (bundle-only, Circuits 1A + 2)

Operational guide for running the `bridge-relayer-daemon daemon-live` binary
against a deployed `AckiNackiBridge` on Sepolia, driven by the shellnet
GraphQL endpoint. Covers first-time bootstrap, steady-state operation, and
recovery from the failure modes we have actually hit in production.

**Scope of this runbook.** Bundle-only path: Circuit 1A/1B (attestation) +
Circuit 2 (layer hashes), aggregated by R15 SHPLONK, submitted via
`verifyBlock`. **No** BK-set updates (Circuit 3 / `applyBkSetUpdate`) and
**no** event proofs (Circuit 4 / `withdrawByProof`). 

> **Notation:** `seq_no` is Acki Nacki block sequence number.
> "Key block" = every `SEQ_NO % (W*P) == 0` block; only key blocks trigger a
> bundle proof (currently `W=8, P=64`, so stride 512).

---

## Table of Contents

- [Quick resume checklist (returning to a running system)](#quick-resume-checklist-returning-to-a-running-system)
- [Deploy your own bridge bundle (external users)](#deploy-your-own-bridge-bundle-external-users)
- [Live reference deploy (Alina's shellnet)](#live-reference-deploy-alinas-shellnet)
- [Binary + env prerequisites](#binary--env-prerequisites)
- [Case 1 — First-time bootstrap from a fresh deploy](#case-1--first-time-bootstrap-from-a-fresh-deploy)
- [Case 2 — Steady-state operation](#case-2--steady-state-operation)
- [Case 3 — Clean restart (no state loss)](#case-3--clean-restart-no-state-loss)
- [Case 4 — Restart after RPC-induced hard-abort](#case-4--restart-after-rpc-induced-hard-abort)
- [Case 5 — Restart after on-chain revert](#case-5--restart-after-on-chain-revert)
- [Case 6 — State loss / re-bootstrap from mid-chain](#case-6--state-loss--re-bootstrap-from-mid-chain)
- [Health checks (run any time)](#health-checks-run-any-time)
- [File & state reference](#file--state-reference)
- [Change log / known incidents](#change-log--known-incidents)

---

## Quick resume checklist (returning to a running system)

Run this **before touching anything** — it takes 30 seconds and tells you
exactly which case (below) applies.

```bash
cd <your-checkout>/bridge/crates/an-bridge-prover
export BRIDGE=$(grep '^BRIDGE_ADDRESS=' .env.shellnet | cut -d= -f2)   # your deploy
export RPC=https://ethereum-sepolia-rpc.publicnode.com

# 1. Is the daemon alive?
pgrep -af 'relayer daemon-live' || echo "DAEMON NOT RUNNING"

# 2. Where is local state?
echo "local  last_processed = $(jq -r '.last_processed_seqno'                   relayer-state.json)"
echo "local  attempts       = $(jq -r '.attempts_since_progress'                relayer-state.json)"
echo "local  observed_chain = $(jq -r '.last_observed_on_chain.last_seen_block_seq_no' relayer-state.json)"
echo "state/prover_state mtime: $(stat -f '%Sm' state/prover_state.json 2>/dev/null || echo MISSING)"

# 3. Where is on-chain?
echo "chain  last_seen       = $(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC --json | jq -r '.[0]')"

# 4. Latest log tail
LOG=$(ls -t logs/live_*.log 2>/dev/null | head -1)
echo "latest log: $LOG"
tail -20 "$LOG" 2>/dev/null | grep -E '(ERROR|WARN|verifyBlock|seed policy|stuck|Explicit|Resume)'
```

**Decision matrix from output:**

| pgrep | local == chain | attempts | Go to |
|---|---|---|---|
| running | yes | 0 | Nothing — [Case 2](#case-2--steady-state-operation) (steady-state) |
| running | yes | >0 | Watch — mid-cycle retry; only intervene if hard-aborts |
| not running | yes | 0 | [Case 3](#case-3--clean-restart-no-state-loss) (clean restart) |
| not running | yes | >0 | [Case 4](#case-4--restart-after-rpc-induced-hard-abort) (RPC hard-abort) — reset counter first |
| not running | **no** | any | [Case 5](#case-5--restart-after-on-chain-revert) or [Case 6](#case-6--state-loss--re-bootstrap-from-mid-chain) — do not restart blindly |
| running/not | state/ missing | — | [Case 6](#case-6--state-loss--re-bootstrap-from-mid-chain) (re-bootstrap) |

---

## Deploy your own bridge bundle (external users)

Running this E2E requires an on-chain `AckiNackiBridge` **that you own and
fund**. `verifyBlock` (`AckiNackiBridge.sol:650`) is a
state-mutating `external nonReentrant` function; every relayer submit is a
signed Sepolia transaction. The daemon reads its signer from the
`RELAYER_PRIVATE_KEY` env var (`bridge-relayer-daemon/src/bin/relayer.rs:86`);
there is no default, no fallback, no shared key. You cannot borrow the
reference-deploy addresses below — those are Alina's; only she holds the
key that can advance them.

### 1. Create a fresh burner wallet

```bash
cast wallet new
#  Address:     0x...
#  Private key: 0x...
```

Keep the private key in a local file **outside** the repo. Never reuse a
wallet that holds real funds — the deploy scripts and daemon both accept
the key over env.

### 2. Fund it with Sepolia ETH

Two faucets that reliably deliver **without** an anti-Sybil mainnet-deposit
gate (verified 2026-08):

- **pk910 PoW** — https://sepolia-faucet.pk910.de/  (mine in-browser, ~5–15 min for the target amount)
- **Google Cloud Web3 faucet** — https://cloud.google.com/application/web3/faucet/ethereum/sepolia  (0.05 ETH/day, no PoW)

Faucets that require depositing ≥0.001 ETH on mainnet first (Alchemy /
Infura / QuickNode) are usable once you're funded; they hand out 0.05–0.5
ETH/day and are the practical top-up path after bootstrap.

**Budget.** The one-shot deploy of the 6-contract bundle
(`DeployShellnetE2EBridge.s.sol`: `AckiNackiBridge` + 4 SHPLONK verifiers +
`MockBlockHeaderOracle`) cost **0.051 ETH** on 2026-08-04 (30M gas @
2.4 gwei). Add ~0.001 ETH for the post-deploy `unpause()` tx and a
running budget of ~0.001–0.003 ETH per `verifyBlock` submit (one per
512-block stride). **Target ≥ 0.1 ETH before deploy**, ≥ 0.5 ETH for a
multi-day E2E run.

### 3. Deploy the contract bundle

The canonical deploy script is
[`contracts/ethereum/script/DeployShellnetE2EBridge.s.sol`](../../../contracts/ethereum/script/DeployShellnetE2EBridge.s.sol).
Full deployer-side runbook lives at
[`docs/shellnet_e2e_acceptance_runbook.md`](../../../docs/shellnet_e2e_acceptance_runbook.md);
the minimum env needed is:

```bash
cd contracts/ethereum
cp .env.example .env                              # then edit:
#   SEPOLIA_RPC_URL=https://ethereum-sepolia-rpc.publicnode.com
#   PRIVATE_KEY=<your burner from §1>
#   ETHERSCAN_API_KEY=<optional, for source verification>

# Compute genesis anchors against live shellnet chain head.
# --at-head picks the newest W*P = 1024-block key-block boundary ≤ head:
#   GENESIS_LAST_SEEN_BLOCK_SEQNO      = that boundary (the seed key block)
#   GENESIS_PREV_MAX_LEVEL_LAYER_HASH  = layer-1 root observed at that block
#                                        (same value the bridge will stamp on-chain)
#   GENESIS_BK_SET_COMMITMENT          = Poseidon commitment of bk_set.shellnet.json
#                                        (stable while shellnet BK rotation is off)
cd ../../crates/an-bridge-prover/bridge-prover-lib
cargo run --release --bin compute_bridge_anchors -- \
  --at-head \
  --gql-endpoint https://shellnet.ackinacki.org/graphql

cd ../../contracts/ethereum
set -a && source .env && set +a
export GENESIS_BK_SET_COMMITMENT=0x...
export GENESIS_PREV_MAX_LEVEL_LAYER_HASH=0x...
export GENESIS_LAST_SEEN_BLOCK_SEQNO=<from compute_bridge_anchors>
export WIRE_WITHDRAW_BY_PROOF=true            # required by constructor since 7a645e5
export WITHDRAW_ACC_FR=0x1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a  # placeholder OK for verifyBlock-only

forge script script/DeployShellnetE2EBridge.s.sol:DeployShellnetE2EBridge \
  --rpc-url $SEPOLIA_RPC_URL --broadcast --slow

# Contract is deployed PAUSED. Unpause with:
cast send $BRIDGE 'unpause()' --rpc-url $SEPOLIA_RPC_URL --private-key $PRIVATE_KEY
```

The broadcast record ends up in
`contracts/ethereum/broadcast/DeployShellnetE2EBridge.s.sol/11155111/run-latest.json` —
extract the 6 contract addresses from there.

### 4. Wire the daemon to your deploy

Populate `crates/an-bridge-prover/.env.shellnet` (create if missing):

```bash
RPC_URL=https://ethereum-sepolia-rpc.publicnode.com
BRIDGE_ADDRESS=<AckiNackiBridge from step 3>
RELAYER_PRIVATE_KEY=<your burner from step 1>
BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql
BRIDGE_BOOTSTRAP_SEQNO=<GENESIS_LAST_SEEN_BLOCK_SEQNO from step 3>
BRIDGE_BK_SET_CONFIG=./bk_set.shellnet.json
BRIDGE_PARAMS_DIR=./params
BRIDGE_STATE_DIR=./state
BRIDGE_AGGREGATOR_DIR=../bridge-evm-aggregator
BRIDGE_VERIFIERS_DIR=../../contracts/ethereum/verifiers
```

Now jump to [Binary + env prerequisites](#binary--env-prerequisites) and
[Case 1 — First-time bootstrap from a fresh deploy](#case-1--first-time-bootstrap-from-a-fresh-deploy).

---

## Live reference deploy (Alina's shellnet)

The addresses below are Alina's **working** verifyBlock-only shellnet
deploy on Sepolia. External users cannot advance them (Alina holds the
signer) but they are useful as read-only reference — every `cast call`
example in this runbook targets these values, and you can compare your
own deploy's `expectedPrevAnchor(1)` / `storedBkSetCommitment()` shapes
against them for sanity.

| Item | Value |
|---|---|
| Network | Sepolia (chain 11155111) |
| RPC | `https://ethereum-sepolia-rpc.publicnode.com` |
| `AckiNackiBridge` | `0x827169ac5DdF31f6cCE8C5026a8501dEc3a5253a` |
| `PrimaryAggregatorVerifier` | `0x0AE4061E336dF9cF988807FF3616Ec9BBA734d1C` |
| `FallbackAggregatorVerifier` | `0x3774Fa01589C49e6DdE596d2a8F86a3dfEE49cEf` |
| `LayerHashesAggregatorVerifier` | `0xa3208EFd0926948D8f1C3092D81B4Ff979d874f3` |
| `BridgeWithdrawalAggregatorVerifier` | `0x65782C62FAC71FAB4672488DAd027BBf84f30B04` |
| `MockBlockHeaderOracle` | `0x50217f995139cE12D6C747fFb1145034Ec80cE78` |
| Bootstrap seed seq_no | `5495808` |
| Genesis `bk_set_commitment` | `0x08eb0a1892e4f75a8b5c8cff69322f95bf0437c371903998c9365fbe293ca71c` |
| Genesis `prev_max_level_layer_hash` | `0x10dcf878d2958d3eca4c23544bae2be13069595907e2b1bee5bb1756e8c11f76` |

*Local note (Alina/Claude only):* the burner deployer key, per-deploy
change history, and full genesis-anchor paper trail live in
`~/HALO2_TVM_EXPERIMENTS/bridge-deployer.txt` — a personal, off-tree file.
Not shipped and not linked from this doc.

Any redeploy invalidates all seven contract addresses + the seed seq_no —
regenerate `.env.shellnet` (see [Deploy your own bridge bundle](#deploy-your-own-bridge-bundle-external-users)
step 4, or [Case 6](#case-6--state-loss--re-bootstrap-from-mid-chain) for
in-place re-seed against an existing contract).

---

## Binary + env prerequisites

Working directory: `crates/an-bridge-prover/`. The daemon reads env from
`.env.shellnet` — all `daemon-live` CLI flags are exposed as `BRIDGE_*` env
vars, so a `source .env.shellnet` + `cargo run` is enough.

**One-time setup (per fresh clone):**

```bash
cd crates/an-bridge-prover

# 1. Build the daemon binary (either profile works)
cargo build --release -p bridge-relayer-daemon --bin relayer
#   -> ./target/release/relayer

# 2. Build the aggregator subprocess (mandatory for daemon-live)
cd ../bridge-evm-aggregator
cargo build --release
cd ../an-bridge-prover

# 3. Provision Hermez K=21 SRS (one-time, ~7 min).
#    See TECHNICAL_README.md "KZG SRS provisioning".
```

**Env file sanity (before any launch):**

```bash
cd crates/an-bridge-prover
for v in RPC_URL BRIDGE_ADDRESS RELAYER_PRIVATE_KEY \
         BRIDGE_GQL_ENDPOINT BRIDGE_AGGREGATOR_DIR BRIDGE_VERIFIERS_DIR \
         BRIDGE_PARAMS_DIR BRIDGE_STATE_DIR BRIDGE_BK_SET_CONFIG \
         BRIDGE_BOOTSTRAP_SEQNO; do
  grep -q "^${v}=" .env.shellnet && echo "  ok  $v" || echo "  FAIL $v"
done
```

All ten variables must be present. `BRIDGE_BOOTSTRAP_SEQNO` must equal the
contract's `storedLastSeenBlockSeqNo` at construction — i.e. the
`GENESIS_LAST_SEEN_BLOCK_SEQNO` you passed at deploy time (see
[Deploy your own bridge bundle §3](#3-deploy-the-contract-bundle)). Verify
on-chain with:

```bash
cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC
```

---

## Case 1 — First-time bootstrap from a fresh deploy

**When to use.** Contract just deployed; no `state/prover_state.json` yet.

**Pre-flight (contract sanity):**

```bash
set -a && source .env.shellnet && set +a
export BRIDGE=$BRIDGE_ADDRESS
export RPC=$RPC_URL

cast call $BRIDGE 'paused()(bool)'                          --rpc-url $RPC   # false
cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)'      --rpc-url $RPC   # == $BRIDGE_BOOTSTRAP_SEQNO
cast call $BRIDGE 'expectedPrevAnchor(uint8)(uint256)' 1    --rpc-url $RPC   # matches env GENESIS_PREV_MAX_LEVEL_LAYER_HASH
cast call $BRIDGE 'storedBkSetCommitment()(uint256)'        --rpc-url $RPC   # matches env GENESIS_BK_SET_COMMITMENT
```

If any of the four don't match your deploy's genesis values (from the
`compute_bridge_anchors` output you pinned at deploy time), **stop** —
the deploy is broken. Do not launch the daemon.

**Cold-start launch:**

```bash
cd crates/an-bridge-prover
mkdir -p state logs
rm -f state/*.json          # ensure clean bootstrap (file-first guard sees no state → seeds from env)

set -a && source .env.shellnet && set +a
TS=$(date +%Y%m%d_%H%M%S)
nohup ./target/release/relayer daemon-live > logs/live_cold_${TS}.log 2>&1 &
echo "PID=$!"
```

**Expected log signature (first 30s):**

```
INFO bridge_prover_lib::bk_set_bootstrap:  BK-set bootstrap: mode=file, loaded 5 signers from ./bk_set.shellnet.json
INFO bridge_prover_lib::keys::primary:     loaded primary VK from cache
INFO bridge_prover_lib::keys::fallback:    loaded fallback VK from cache
INFO bridge_prover_lib::keys::layer:       loaded layer VK from cache
INFO bridge_prover_lib::keys::event:       loaded event VK from cache
INFO bridge_prover_lib::bk_set_bootstrap:  chain-config check OK: ./bk_set.shellnet.json matches prover_bk_set.commitment
INFO relayer: LiveProverDriver seed policy seed_policy=Explicit(<BRIDGE_BOOTSTRAP_SEQNO>)   ← cold-start signature (e.g. 5495808 on the live reference deploy)
```

`seed_policy=Explicit(N)` means the daemon is seeding from
`BRIDGE_BOOTSTRAP_SEQNO`. `seed_policy=Resume` would mean it found existing
`state/prover_state.json` and is resuming — wrong for cold start.

**First cycle takes ~15 min:** GQL fetch of seed block → real-chain-builder
Merkle root → Circuit 2 proof (~2 min) → aggregation (~7 min at `layer PK`
load) → submit → wait ~30-60s for Sepolia confirmation → `ack_last_bundle`
→ `state/prover_state.json` written for the first time.

---

## Case 2 — Steady-state operation

Once bootstrapped, cadence is **~13 min per key-block** (limited by Circuit 2
+ SHPLONK aggregation, not RPC).

**Where things get written per successful cycle:**

| Path | Written by | Trigger |
|---|---|---|
| `submissions/verifyBlock_seq<N>_fin<T>_<ts>.json` | `bridge.rs:679-724` (env-gated by `BRIDGE_DUMP_SUBMISSIONS_DIR`) | Every submit attempt (before tx send) |
| `relayer-state.json` | `relayer.rs:149` | Every on-chain observation cycle (~every block) |
| `state/prover_state.json` + `state/prover_bk_set.json` | `live_source.rs:141-157` (`persist_driver`) | Every successful `ack_last_bundle` after on-chain confirmation |

**Steady-state watch commands:**

```bash
# Live daemon log
tail -f crates/an-bridge-prover/logs/live_*.log

# Latest submissions (cadence check)
watch -n 30 'ls -lt crates/an-bridge-prover/submissions/ | head -6'

# On-chain progress
watch -n 60 "cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC"
```

**Progress signature (per successful cycle):**

```
INFO bridge_prover_lib::live_driver::bundle: key block <N>: Circuit 2 proof generated in <ms>
INFO bridge_relayer_daemon::aggregated_source: aggregated_source: wrap+aggregate attestation + layer
INFO bridge_relayer_daemon::bridge: dumped verifyBlock submission to submissions/verifyBlock_seq<N>_fin0_<ts>.json
INFO bridge_relayer_daemon::relayer: verifyBlock confirmed  seq_no=<N>  block=<sepolia_block>
```

Absence of the last line for >5 min after `dumped verifyBlock submission` =
transport / receipt lag — go to [Case 4](#case-4--restart-after-rpc-induced-hard-abort).

---

## Case 3 — Clean restart (no state loss)

**When to use.** Daemon is running, no failures — you just want to restart
(e.g. after `git pull` + rebuild).

```bash
cd crates/an-bridge-prover

# 1. Send SIGTERM, wait for graceful exit
kill $(pgrep -f 'relayer daemon-live')
sleep 5
pkill -9 -f 'aggregate-proof' 2>/dev/null   # clean up any orphan child

# 2. Rebuild if needed
cargo build --release -p bridge-relayer-daemon --bin relayer

# 3. Relaunch (state files preserved → file-first guard triggers Resume)
set -a && source .env.shellnet && set +a
TS=$(date +%Y%m%d_%H%M%S)
nohup ./target/release/relayer daemon-live > logs/live_restart_${TS}.log 2>&1 &
```

**Verify Resume:** log must contain `LiveProverDriver seed policy
seed_policy=Resume`, NOT `Explicit(...)`. If you see `Explicit(...)` after a
restart, `state/prover_state.json` is missing — go to [Case 6](#case-6--state-loss--re-bootstrap-from-mid-chain).

---

## Case 4 — Restart after RPC-induced hard-abort

**Symptoms.** Daemon exited by itself. Last log lines:

```
WARN bridge_relayer_daemon::relayer: bridge reverted attempts=3
   reason=verifyBlock send failed: error sending request for url (...) 
   OR
   reason=tx confirmation error: transaction was not confirmed within the timeout
ERROR bridge_relayer_daemon::daemon: daemon: stuck — hard aborting
   error=Stuck { seq_no: <N>, attempts: 3, reason: "..." }
Error: relayer stuck on seqNo=<N> after 3 rejects: verifyBlock: ...
```

The `9b07b24` hard-abort logic counts **transport failures as rejections**
and kills the daemon after N=3. This is currently a known false-positive:
transport failures should retry with backoff, not abort.

**Recovery — always run these three checks before relaunch:**

### 4a. Verify no txs actually landed (nonce check)

```bash
RELAYER_ADDR=$(cast wallet address --private-key $RELAYER_PRIVATE_KEY)
cast nonce $RELAYER_ADDR --rpc-url $RPC
cast nonce $RELAYER_ADDR --rpc-url $RPC --block pending
```

If `latest == pending`, no txs in mempool. If `pending > latest`, wait 1-2 min
for pending to clear before restart (otherwise nonce collision).

### 4b. Drift check — local state vs on-chain

```bash
cd crates/an-bridge-prover

L_SEQ=$(jq -r '.last_observed_on_chain.last_seen_block_seq_no' relayer-state.json)
L_BK=0x$(python3 -c "print(f'{int(\"$(jq -r '.last_observed_on_chain.bk_set_commitment' relayer-state.json)\", 16):064x}')")
L_PREV=0x$(python3 -c "print(f'{int(\"$(jq -r '.last_observed_on_chain.prev_max_level_layer_hash' relayer-state.json)\", 16):064x}')")

C_SEQ=$(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC --json | jq -r '.[0]')
C_BK=0x$(python3 -c "print(f'{$(cast call $BRIDGE storedBkSetCommitment\(\)\(uint256\) --rpc-url $RPC --json | jq -r '.[0]'):064x}')")
C_PREV=0x$(python3 -c "print(f'{$(cast call $BRIDGE storedPrevMaxLevelLayerHash\(\)\(uint256\) --rpc-url $RPC --json | jq -r '.[0]'):064x}')")

[ "$L_SEQ" = "$C_SEQ" ]   && echo "seq OK"  || echo "SEQ DRIFT  local=$L_SEQ chain=$C_SEQ"
[ "$L_BK"  = "$C_BK" ]    && echo "bk OK"   || echo "BK DRIFT   local=$L_BK  chain=$C_BK"
[ "$L_PREV"= "$C_PREV" ]  && echo "prev OK" || echo "PREV DRIFT local=$L_PREV chain=$C_PREV"
```

- **Zero drift → proceed to 4c.**
- **Any drift → STOP.** Investigate what advanced on-chain (someone else's
  submission? contract redeploy?) before touching anything. Never restart
  through drift — you'll instantly hit `PrevAnchorMismatch` /
  `BlockSeqNoNotMonotonic` and the abort loop kicks in again.

### 4c. Reset the attempts counter

```bash
jq '.last_attempt_seqno = .last_processed_seqno
  | .attempts_since_progress = 0
  | .bk_update_attempts_since_progress = 0' relayer-state.json > relayer-state.json.tmp \
  && mv relayer-state.json.tmp relayer-state.json
jq . relayer-state.json
```

Without this, the daemon inherits `attempts_since_progress=3` from disk and
hard-aborts on the very next transport blip.

### 4d. Relaunch

Same as [Case 3](#case-3--clean-restart-no-state-loss). Expect
`seed_policy=Resume`. First cycle regenerates the proof from scratch
(prior proof was in RAM, lost on exit) — ~13 min to next submit.

**If it dies again from RPC:** consider swapping `RPC_URL` in
`.env.shellnet` to an Alchemy / Infura / Ankr endpoint. Public-node RPCs are
rate-limited and drop long-lived receipt polls.

---

## Case 5 — Restart after on-chain revert

**Symptoms.** Daemon exited via hard-abort, log shows:

```
WARN bridge_relayer_daemon::relayer: bridge reverted attempts=3
   reason=verifyBlock send failed: server returned an error response:
   error code 3: execution reverted, data: "0x<selector>"
```

**Decode the revert selector.** Any of these is a real proof/state problem,
not a transport blip:

| Selector | Error | Root cause pattern |
|---|---|---|
| `0x87bf1c06` | `AttestationProofRejected()` | Adapter equality check on a public input failed. **Bug class: BN254 Fr canonicalization** — if this fires on `blockId`, the client fix in `bridge-relayer-daemon/src/types.rs:83` (`U256::from_be_bytes(b.block_id_be) % BN254_FR_MODULUS`) is missing/reverted. See [Change log — 2026-08-03 BN254 Fr fix](#change-log--known-incidents). |
| `0x...PrevAnchorMismatch` | `PrevAnchorMismatch(supplied, stored)` | Local prev-anchor state diverged from on-chain `expectedPrevAnchor(numLayers)`. Either the daemon crashed mid-tx (extremely rare) or the chain advanced without us. |
| `0x...BkSetCommitmentMismatch` | `BkSetCommitmentMismatch(...)` | On-chain BK-set was rotated by an `applyBkSetUpdate` we don't know about, OR `bk_set.shellnet.json` drifted from live shellnet BLS keys. |
| `0x...BlockSeqNoNotMonotonic` | `BlockSeqNoNotMonotonic(supplied, stored)` | We're trying to submit a `seq_no ≤ storedLastSeenBlockSeqNo`. Almost always: state loss + wrong `BRIDGE_BOOTSTRAP_SEQNO`. |

Decode via `cast 4byte $SELECTOR` or use `cast call ...` to dry-run the
failing submission and get a decoded revert reason:

```bash
LATEST=$(ls -t crates/an-bridge-prover/submissions/verifyBlock_seq*.json | head -1)
cast call $BRIDGE \
  "verifyBlock(uint8,bytes,bytes,uint256,uint256,uint64,uint8,uint256[10],uint256)" \
  $(jq -r '.fin_type'                        "$LATEST") \
  $(jq -r '.attestation_proof_hex'           "$LATEST") \
  $(jq -r '.layer_hashes_proof_hex'          "$LATEST") \
  $(jq -r '.block_id_uint256'                "$LATEST") \
  $(jq -r '.bk_set_commitment_uint256'       "$LATEST") \
  $(jq -r '.block_seq_no'                    "$LATEST") \
  $(jq -r '.num_layers'                      "$LATEST") \
  "[$(jq -r '.layer_hashes_uint256 | join(",")' "$LATEST")]" \
  $(jq -r '.prev_max_level_layer_hash_uint256' "$LATEST") \
  --rpc-url $RPC
```

Success = returns nothing (void). Failure = decoded revert reason.

**Do NOT** just reset the counter and restart — the proof itself is broken.
Fix the root cause first, then follow [Case 4c → 4d](#4c-reset-the-attempts-counter).

---

## Case 6 — State loss / re-bootstrap from mid-chain

**When to use.** `state/prover_state.json` deleted / corrupted, OR contract
redeployed at a different `storedLastSeenBlockSeqNo`.

**This is destructive.** Only proceed if you've confirmed the contract is at
a known seed and no in-flight state is worth preserving.

```bash
cd crates/an-bridge-prover

# 1. Read the contract's CURRENT last_seen from chain
CURRENT=$(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC --json | jq -r '.[0]')
echo "chain last_seen = $CURRENT"

# 2. Update BRIDGE_BOOTSTRAP_SEQNO in .env.shellnet to match
#    (must equal what compute_bridge_anchors would produce for that seed)
grep '^BRIDGE_BOOTSTRAP_SEQNO=' .env.shellnet
# Manually edit if needed — MUST equal $CURRENT

# 3. Archive any existing state
TS=$(date +%Y%m%d_%H%M%S)
[ -d state ] && mv state state.stale_${TS}
[ -f relayer-state.json ] && mv relayer-state.json relayer-state.json.stale_${TS}
mkdir -p state

# 4. Regenerate genesis anchors from the seed block (sanity)
cd ../bridge-prover-lib
cargo run --release --bin compute_bridge_anchors -- \
  --seed-seqno $CURRENT \
  --gql-endpoint https://shellnet.ackinacki.org/graphql
# Compare its output to `expectedPrevAnchor(1)` and `storedBkSetCommitment()`
# on-chain. All three must match. If not: env / contract are out of sync —
# fix the contract deploy before proceeding.
cd ../an-bridge-prover

# 5. Cold-start launch (identical to Case 1)
set -a && source .env.shellnet && set +a
nohup ./target/release/relayer daemon-live > logs/live_rebootstrap_${TS}.log 2>&1 &
```

**Expect `seed_policy=Explicit($CURRENT)`** in the log. Not `Resume`.

**Why re-bootstrap is dangerous.** If `BRIDGE_BOOTSTRAP_SEQNO` doesn't match
`storedLastSeenBlockSeqNo` on-chain, the first submit reverts with
`BlockSeqNoNotMonotonic` (if you're behind) or `PrevAnchorMismatch` (if
you're ahead of chain but chain expects a different prev-anchor). See the
[Change log](#change-log--known-incidents) `2026-08-03 — chicken-and-egg
fix` entry for the constructor-arg (`genesisLastSeenBlockSeqNo`) that
makes this diagnosable rather than a hard hang on the first submit.

---

## Health checks (run any time)

**On-chain state snapshot:**

```bash
set -a && source .env.shellnet && set +a
export BRIDGE=$BRIDGE_ADDRESS
export RPC=$RPC_URL
echo "paused:            $(cast call $BRIDGE 'paused()(bool)' --rpc-url $RPC)"
echo "last_seen:         $(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC --json | jq -r '.[0]')"
# num_layers derived from getLatestPerLayer() — highest index with a nonzero hash.
# (Since storage-v2 / commit f8c5ba0, storedNumLayers()/storedLayerHashes(uint256)/getStoredLayerHashes() are gone.)
echo "latest_per_layer:  $(cast call $BRIDGE 'getLatestPerLayer()(uint256[10])' --rpc-url $RPC)"
echo "bk_last_update:    $(cast call $BRIDGE 'storedLastBkSetUpdateSeqNo()(uint64)' --rpc-url $RPC --json | jq -r '.[0]')"
echo "bk_commitment:     $(cast call $BRIDGE 'storedBkSetCommitment()(uint256)' --rpc-url $RPC)"
echo "expectedPrev(1):   $(cast call $BRIDGE 'expectedPrevAnchor(uint8)(uint256)' 1 --rpc-url $RPC)"
echo "storedPrev(genesis): $(cast call $BRIDGE 'storedPrevMaxLevelLayerHash()(uint256)' --rpc-url $RPC)"
# Since storage-v2 (f8c5ba0), storedPrevMaxLevelLayerHash is `immutable` — it
# holds the genesis seed forever, not the last-block max-level.
```

**Daemon liveness:**

```bash
pgrep -a -f 'relayer daemon-live'                  # parent PID + cmdline
pgrep -a -f 'aggregate-proof'                      # active aggregator subprocess (should exist mid-cycle)
```

**Recent activity:**

```bash
# Last successful ack (mtime of state/prover_state.json)
stat -f "%Sm  %N" crates/an-bridge-prover/state/prover_state.json

# Last submitted block
ls -t crates/an-bridge-prover/submissions/verifyBlock_seq*.json | head -1 \
  | xargs -I {} sh -c 'jq -r "\"submitted seq_no=\" + (.block_seq_no|tostring)" {}'

# Cadence — deltas between latest 5 submissions
ls -lt crates/an-bridge-prover/submissions/verifyBlock_seq*.json | head -5
```

**Wallet balance:**

```bash
RELAYER_ADDR=$(cast wallet address --private-key $RELAYER_PRIVATE_KEY)
cast balance $RELAYER_ADDR --rpc-url $RPC --ether
# Each verifyBlock costs ~0.001-0.003 ETH depending on Sepolia gas price.
# Refill from the faucets listed under
# [Deploy your own bridge bundle §2](#2-fund-it-with-sepolia-eth)
# if <0.5 ETH.
```

---

## File & state reference

Everything lives under `crates/an-bridge-prover/` (the daemon's working
directory). The sibling `crates/bridge-relayer-daemon/` directory is
**source code only** — no runtime data lands there.

```
crates/an-bridge-prover/
├── .env.shellnet                    ← config (env vars, gitignored)
├── bk_set.shellnet.json             ← bootstrap BK-set (5 signers)
├── relayer-state.json               ← verifier-side cursor + on-chain observation cache
├── state/
│   ├── prover_state.json            ← LiveProverDriver snapshot (~574 KB) — auth. resume point
│   └── prover_bk_set.json           ← active BK-set snapshot (updated only via applyBkSetUpdate)
├── submissions/
│   └── verifyBlock_seq<N>_fin<T>_<ts>.json   ← calldata dumps (env-gated by BRIDGE_DUMP_SUBMISSIONS_DIR)
├── logs/
│   └── live_<label>_<ts>.log        ← daemon stdout+stderr
├── params/                          ← SRS + circuit VKs/PKs (~17 GB, DO NOT WIPE)
└── target/release/relayer           ← the binary
```

**Persistence triggers (see [`../src/live_source.rs:115,132,210,259`](../src/live_source.rs)):**

- `state/prover_state.json` + `state/prover_bk_set.json` are written together
  by `persist_driver` on every successful `ack_last_bundle` /
  `ack_bk_update`. Not on every submit — only on confirmed ACK.
- `relayer-state.json` is written on every on-chain observation refresh
  (~every block).

**Cleanup rules:**

- `submissions/` — safe to prune anytime; purely diagnostic.
- `logs/` — safe to prune, but keep the most recent for post-mortem.
- `state/`, `relayer-state.json` — **NEVER** delete a running daemon's
  active state. To reset, archive to `state.stale_<ts>/` +
  `relayer-state.json.stale_<ts>` first (see [Case 6](#case-6--state-loss--re-bootstrap-from-mid-chain)).
- `params/` — never delete; keygen takes ~7 min per circuit.

---

## Change log / known incidents

Newest first. Each entry captures **what happened, why, what changed, and
the reference commits/paths** so we don't have to reconstruct history next
time we come back to the runbook.

### 2026-08-04 — Deploy #5 (storage v2.0 + NB-Q1/Q8 remediation)

- **Why re-deploy.** Four contract changes landed between 2026-08-03 and
  2026-08-04 and were folded into a fresh deploy rather than run
  mixed-source/bytecode against Deploy #4.
- **Contract changes:**
  - `f8c5ba0` — storage v2.0. `storedNumLayers` + `storedLayerHashes[10]`
    removed; hot-path SSTOREs on both eliminated (~31.9 k gas / verifyBlock).
    `storedPrevMaxLevelLayerHash` promoted to `immutable` (holds the
    genesis seed forever); new `getLatestPerLayer()` view is the correct
    per-layer state observation.
  - `7da8878` — `applyBkSetUpdate` folds BK-set commitments LE (NB-Q1
    blocker from Sergey's 07-27 answer).
  - `09c1686` — `withdrawByProof` flat `_isKnownAnchor` across all 10
    windows (NB-Q1 Option D; no feature flag).
  - `7a645e5` — `DeployShellnetE2EBridge` now requires the C4 verifier +
    `WITHDRAW_ACC_FR` on any chain != anvil (NB-Q8). The
    `BridgeWithdrawalAggregatorVerifier` .bin ships in the bundle even
    though this runbook does not exercise `withdrawByProof`.
- **ABI removals versus prior deploys.** Any tooling calling
  `storedNumLayers()`, `storedLayerHashes(uint256)` or
  `getStoredLayerHashes()` will revert against Deploy #5. Health-check
  block in this runbook updated to use `getLatestPerLayer()` instead.
- **Fresh genesis anchors** were regenerated via
  `compute_bridge_anchors --at-head` against shellnet GraphQL. Bootstrap
  seed advanced from `4887552` (Deploy #4) to `5495808` (Deploy #5,
  bundle boundary 1024). Genesis `bk_set_commitment` unchanged (shellnet
  BK rotation is off).
- **Reference addresses** for Alina's live shellnet deploy are in
  [Live reference deploy](#live-reference-deploy-alinas-shellnet).
- **How to re-deploy your own bundle:** see
  [Deploy your own bridge bundle](#deploy-your-own-bridge-bundle-external-users).

### 2026-08-04 — RPC-transport hard-abort false-positive

- **Symptom.** Daemon exited at 11:10:38 local after 3 consecutive tx-send
  failures on seq `4891136`. First failure was `error sending request` (a
  transport error); the next two were `tx confirmation timeout`.
- **Nonce check confirmed zero txs actually landed** — the daemon was
  killed by its own retry-counter, not the chain.
- **Root cause.** The `9b07b24` hard-abort logic in
  `bridge-relayer-daemon/src/relayer.rs` treats transport failures
  (network / receipt polling) identically to on-chain reverts. `N=3`
  transport hiccups on a public RPC (publicnode.com) trip the abort.
- **Recovery (this incident).**
  1. Ran the drift check (§Case 4b) → zero drift.
  2. Reset counter atomically:
     `jq '.last_attempt_seqno = .last_processed_seqno | .attempts_since_progress = 0'`.
  3. Relaunched via [Case 3](#case-3--clean-restart-no-state-loss).
     `seed_policy=Resume` confirmed in log.
  4. First cycle regenerated the proof for `4891136` in ~13 min → confirmed
     on Sepolia → `state/prover_state.json` bumped mtime to 11:47.
- **Follow-up (not yet landed).** Refactor the hard-abort classifier so
  transport-error variants only trigger exponential backoff, and only
  on-chain revert variants count towards the N=3 abort budget. Documented
  as an open item; the runbook workaround (§Case 4c) is the current
  mitigation.

### 2026-08-03 — BN254 Fr canonicalization client fix

- **Symptom.** `verifyBlock` on shellnet block `4888576` reverted with
  `AttestationProofRejected()` (selector `0x87bf1c06`). Root-caused to the
  SHPLONK adapter's Fr equality prelude reading a raw 32-byte BE
  `blockId` that was `≥ r` (BN254 scalar modulus).
- **Fix (client-side).** In
  `bridge-relayer-daemon/src/types.rs:81-83`, reduce the raw chain hash
  mod `BN254_FR_MODULUS` before packing it into `U256`:
  ```rust
  block_id: U256::from_be_bytes(b.block_id_be) % BN254_FR_MODULUS,
  ```
  Constant lives in `withdrawal::BN254_FR_MODULUS`.
- **Note.** `BkSetUpdateData::block_id` (line 106) deliberately keeps the
  un-reduced form — `applyBkSetUpdate` compares against a raw SHA-256
  Merkle root (`AckiNackiBridge.sol:834`), not an Fr scalar.
- **Verified txs (post-fix).**
  - `0x12110bb7…` — first bundle after fix (seq `4890624`).
  - `0x5194250e…` — subsequent bundle (seq `4891136`, ACK'd at 11:47 on
    2026-08-04 after the RPC-abort re-run above).
- **Commits (branch `refactoring_and_review_bridge_relayer_demon`):**
  `487a869` (types.rs Fr reduction), `0fc2851`, `ca106f5` (surrounding
  cleanup + BN254_FR_MODULUS constant).
- **Contract-side follow-up.** Adding the same `% BN254_FR` step inside
  the three SHPLONK adapter contracts would make the client fix
  unnecessary; would require redeploy of `PrimaryAggregatorVerifier`,
  `FallbackAggregatorVerifier`, `LayerHashesAggregatorVerifier` — all
  immutable references in `AckiNackiBridge` so the bridge itself would
  need redeploy too. Not planned; client workaround is stable.

### 2026-08-03 — Deploy #4 (`storedLastSeenBlockSeqNo=0` chicken-and-egg fix)

- Deploys #1–3 constructed the contract with
  `storedLastSeenBlockSeqNo=0`, which required a bootstrap `verifyBlock`
  call with `block_seq_no > 0` to advance it — but the first legit
  `expectedPrevAnchor` check couldn't be satisfied until state was
  seeded. Chicken-and-egg.
- Deploy #4 added `genesisLastSeenBlockSeqNo` as a constructor arg
  (`AckiNackiBridge.sol` constructor). The client's
  `BRIDGE_BOOTSTRAP_SEQNO` must equal this value.
- Current active deploy: see
  [Live reference deploy](#live-reference-deploy-alinas-shellnet) (Deploy #5
  superseded #4 on 2026-08-04 — see entry above).

---

**Editing this runbook.** When adding a new incident: keep entries dated,
one-paragraph, and always cite the commit / file:line so the next
maintainer can jump straight to the change without spelunking through
`git log`. Old entries can be pruned once the underlying fix has been
proven for >30 days across restarts.
