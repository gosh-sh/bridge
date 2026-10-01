# Live `withdrawByProofBundle` E2E Runbook — shellnet → Sepolia (Circuit 4 + MultiHop)

Operational guide for driving a full AN→ETH withdrawal E2E through the
in-process `relayer withdraw-e2e` orchestrator against a deployed
`AckiNackiBridge` on Sepolia. Covers first-time end-to-end demo from a
fresh deploy, follow-up withdrawals on an existing deploy, and the failure
modes we have actually hit.

> **Naming note.** The single-shot entry-point historically called
> `withdrawByProof` was renamed to `withdrawByProofBundle` when the
> withdrawal proof was split into a `BridgeEventFinalProof` +
> `BridgeMultiHopProof` bundle. The filenames of this runbook and the
> paired changelog keep the legacy `withdrawByProof` string for
> git-history continuity only. Everywhere below, "withdraw entry-point"
> means `withdrawByProofBundle`.

**Scope of this runbook.** The event/proof/submit path: capture live
`WithdrawalInitiated` ExtOut → export partial witness → enrich against
`prover_state.json` → Circuit 4 FinalProof SHPLONK aggregate (+ optional
per-hop MultiHop SHPLONK aggregates) → `withdrawByProofBundle`.
**Assumes** the bundle-only path (Circuits 1A + 2 via `daemon-live`) is
already running or has just been launched — that lane is covered in
[`live_relayer_bridge_verifyBlock_runbook.md`](./live_relayer_bridge_verifyBlock_runbook.md), which
this doc extends.

> **Honest scope — single-operator, single-burn-in-flight (relayer
> daemon path only).** The `relayer withdraw-e2e` binary described in
> this runbook has **no per-user or per-msg-id targeting**. It
> correlates the target burn to the on-chain event by "youngest
> matching `WithdrawalInitiated` ExtOut from the shared USDCBridge
> account, minus a baseline snapshot taken at capture-stage start" —
> nothing more (`withdraw_e2e/capture.rs:73-117`). The CLI exposes
> `--bridge-account-id`, `--bridge-dapp-id`, `--event-dst`, and
> `--replay-latest`; there is no `--target-msg-id`, no
> `--target-seq-no`. `--prover-seq-no` is only a local filename stamp.
>
> Concretely this means, for the daemon path:
>
> - **Concurrent burns from different users through the same
>   `USDCBridge` race.** Both operators' `withdraw-e2e` invocations
>   see both events as new, both pop the youngest, both prove the same
>   event. The other burn is orphaned.
> - **Rapid back-to-back burns from the same operator race.** If a
>   second burn fires during the first `withdraw-e2e` capture stage,
>   the tool captures the second (youngest wins). The first is
>   orphaned in-flight.
> - **`--replay-latest` picks youngest globally.** If any newer
>   `WithdrawalInitiated` has surfaced since your burn (another
>   operator, a stray test run), you prove the wrong one.
>
> The Python driver
> [`generate_withdrawals_with_live_event_proving.py`](../../bridge-prover-libraries/python/generate_withdrawals_with_live_event_proving.py)
> that fires the burn uses the same baseline+wait pattern and logs the
> captured event's `block_seq_no` / `msg_id`, but that identity is
> **not plumbed into `withdraw-e2e`**. The two tools coordinate only by
> "shared queue was drained" convention.
>
> This runbook assumes one operator running one burn at a time,
> waiting for each `withdraw-e2e` cycle to complete before starting
> the next. That is enough for shellnet demos and stress loops; it is
> **not** enough for concurrent-operator or production use of the
> `relayer withdraw-e2e` binary.
>
> **`ackinacki-bridge withdraw` is different.** The third-party
> end-user CLI at
> `crates/bridge-prover-libraries/ackinacki-bridge/` fires its burn
> inline and then targets the resulting `WithdrawalInitiated` ExtOut
> by chain-following the multisig transaction hash — `transaction(hash:
> an_tx_hash).out_messages → dst == USDCBridge →
> message(hash).dst_transaction.out_messages → dst ==
> makeAddrExtern(618)` (`withdraw_e2e/capture.rs::capture_targeted_withdrawal_event`).
> That is multi-user-safe: two operators firing in the same second
> each see only their own tx's outbound chain, so neither can capture
> the other's event. This runbook does not cover the third-party CLI;
> see its own `README.md` for operator instructions.

> **Notation.** `seq_no` = Acki Nacki block sequence number.
> "Covering bundle" = the first bundle whose `key_seq_no ≥ event_seq_no`
> that is verified on-chain. Withdrawals can only be submitted after
> their covering bundle lands (Circuit 4 anchor-chain verifies against
> a `layer_hashes` root that `verifyBlock` has already committed).
> With `W=128, P=8`, an **L1** bundle covers `W·P = 1024` seq_nos
> (~5.7 min chain-time at 3 seq/s). An **L2** bundle covers
> `W² = 16384` seq_nos (~91 min chain-time). Case 2 covers the L2 flow.
>
> **Bundle-shape notation.** `withdrawByProofBundle` accepts a 13-slot
> `finalPublicInputs` array (11 legacy slots at `[0..10]` + `xBlockId`
> at `PUB_X_BLOCK_ID = 11` + `yBlockId` at `PUB_Y_BLOCK_ID = 12`;
> `FINAL_PI_LEN = 13` — `AckiNackiBridge.sol:1347–1355`), a `bytes
> finalProof`, and matched-length arrays `hopPublicInputs[][]` +
> `hopProofs[]` where each hop is exactly two PIs
> `[HOP_START, HOP_END]` (`MULTI_HOP_PI_LEN = 2`) and the total hop
> count is bounded by `N_BUNDLE_MAX = 20`. "Same-thread claim" = empty
> `hopPublicInputs`/`hopProofs` slices, contract requires
> `xBlockId == yBlockId`. "Cross-thread claim" = one or more hops, and
> the contract requires `yBlockId == hop0.start`, folds
> `hop0.start → hop0.end → hop1.start → … → hopLast.end`, and
> requires `hopLast.end == xBlockId` (`AckiNackiBridge.sol:1480–1514`). All
> in-tree callers under `crates/bridge-relayer-daemon/src/bin/relayer.rs`
> and `crates/ackinacki-bridge/src/orchestrator.rs` now plumb the
> hop chain through: `PartnerWithdrawalProof::hop_pis()` /
> `hop_proofs()` decode the `hops_hex` array the driver writes into
> `proof_event_*.json` (populated from the persistent block-graph resolver's
> `Y → … → X` route and `generate_multi_hop_proof`), and pass it to
> `submit_withdraw_bundle` alongside the outer Circuit-4 calldata.
> Same-thread events resolve to empty vecs and take the
> `xBlockId == yBlockId` fast path; cross-thread events (2- to 4-thread
> multi-thread node runs) exercise the full hop-fold path with N ≥ 1
> hops.

---

## Table of Contents

- [Quick resume checklist (returning mid-flow)](#quick-resume-checklist-returning-mid-flow)
- [Timing model — why fresh-deploy demos need tight lookahead](#timing-model--why-fresh-deploy-demos-need-tight-lookahead)
- [Reference values (chain-invariant on shellnet)](#reference-values-chain-invariant-on-shellnet)
- [Binary + env prerequisites](#binary--env-prerequisites)
- [Case 1 — L1: First-time E2E from a fresh deploy (optimal sequence)](#case-1--l1-first-time-e2e-from-a-fresh-deploy-optimal-sequence)
- [Case 2 — L2: fresh deploy and steady-state operation](#case-2--l2-fresh-deploy-and-steady-state-operation)
  - [Case 2a — Fresh L2 deploy: first E2E withdrawal](#case-2a--fresh-l2-deploy-first-e2e-withdrawal)
  - [Case 2b — Sequential L2 withdrawals (steady-state / stress loop)](#case-2b--sequential-l2-withdrawals-steady-state--stress-loop)
- [Case 3 — Incidents & failure modes](#case-3--incidents--failure-modes)
  - [Case 3a — Prover subprocess timeout / OOM](#case-3a--prover-subprocess-timeout--oom)
  - [Case 3b — On-chain `withdrawByProofBundle` revert](#case-3b--on-chain-withdrawbyproofbundle-revert)
  - [Case 3c — USDCBridge key drift (burn side)](#case-3c--usdcbridge-key-drift-burn-side)
  - [Case 3d — `WithdrawTreasuryShortfall` — bridge treasury empty](#case-3d--withdrawtreasuryshortfall--bridge-treasury-empty)
- [L2 timing model](#l2-timing-model)
- [Health checks](#health-checks)
- [File & state reference](#file--state-reference)

---

## Quick resume checklist (returning mid-flow)

```bash
cd /Users/alinat/HALO2_TVM_EXPERIMENTS/bridge/crates/bridge-prover-libraries

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
| running, current | no | no | start `withdraw-e2e --dry-run`, then fire the burn — [Case 1](#case-1--l1-first-time-e2e-from-a-fresh-deploy-optimal-sequence) Steps 4–5 (L1 one-shot) or [Case 2b](#case-2b--sequential-l2-withdrawals-steady-state--stress-loop) (L2 follow-up) |
| running, current | yes | no | run `withdraw-e2e --replay-latest` (event already captured; skip baseline) |
| running, behind | yes | no | [Case 1 Step 6 note](#step-6--watch-dry-run-complete) (L1 catch-up) — enricher retry loop absorbs the wait |
| running, current | yes | yes, revert | [Case 3b](#case-3b--on-chain-withdrawbyproofbundle-revert) |
| not running | any | any | Fix the bundle lane first — see verifyBlock runbook Case 3–6 |

---

## Timing model — why fresh-deploy demos need tight lookahead

The Circuit 4 FinalProof is anchored to `layer_hashes[L]` of the
**covering bundle** at anchor layer `L` — the first bundle whose
layer-`L` key seq_no is ≥ `event_seq_no` (stride = 1024 for L1, 16384
for L2). `withdrawByProofBundle` reverts until
`verifyBlock(covering_bundle)` has landed. End-to-end wall time =

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
`crates/bridge-prover-libraries/L{1,2}_config/env` (rewritten by
`scripts/deploy_bridge_bundle.sh` on each deploy), not from this doc.
Deployer / relayer / owner wallet is the single shared shellnet burner
`0xb586356D52eAee055Ca569Ff412DFeFFc5bB2307` documented in
[`shellnet.common`](../../bridge-prover-libraries/shellnet.common) or smth that you deployed yourself (see instruction how to deploy and fund your wallet here [`live_relayer_bridge_verifyBlock_runbook.md`](./live_relayer_bridge_verifyBlock_runbook.md)).

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
[`crates/bridge-prover-libraries/scripts/deploy_bridge_bundle.sh`](../../bridge-prover-libraries/scripts/deploy_bridge_bundle.sh).

---

## Binary + env prerequisites

Working directory: `crates/bridge-prover-libraries/`.

**One-time setup (per fresh clone):**

```bash
cd crates/bridge-prover-libraries

# 1. Build the relayer (same binary as daemon-live)
cargo build --release -p bridge-relayer-daemon --bin relayer

# 2. Build the aggregator subprocess (mandatory for both lanes)
cd ../bridge-evm-aggregator && cargo build --release && cd ../bridge-prover-libraries

# 3. Build the Circuit 4 event prover
cargo build --release -p bridge-event-halo2-prover
#   -> ./target/release/bridge-event-halo2-prover

# 4. Provision the Hermez KZG SRS + offline keygen (skip if $BRIDGE_PARAMS_DIR
#    already contains multi_hop_manifest.json + event_manifest.json).
#    Both keygen bins are idempotent (warm cache → instant no-op) and
#    flock-guarded, so re-running while a daemon is up is safe. Doing them
#    now avoids the ~7-min synchronous stall on first daemon launch (K=19
#    Circuit-4 keygen spikes RSS >10 GB). See `MULTITHREAD_MIGRATION_PLAN.md`
#    §8. `$BRIDGE_PARAMS_DIR` needs ≥20 GB free.
cargo run --release -p bridge-prover-lib --bin bootstrap_hermez_srs -- \
    --params-dir "$BRIDGE_PARAMS_DIR"
cargo run --release -p bridge-prover-lib --bin keygen_bridge_final -- \
    --params-dir "$BRIDGE_PARAMS_DIR"
cargo run --release -p bridge-prover-lib --bin keygen_bridge_multi_hop -- \
    --params-dir "$BRIDGE_PARAMS_DIR"
```

**Env sanity (in addition to bundle-lane vars from parent runbook):**

```bash
cd crates/bridge-prover-libraries
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

## Local multi-thread devnet (state_v2) — cross-thread source

**When you need this:** you want to exercise the cross-thread hop in
Circuit 4 end-to-end. Shellnet is single-thread on `v3.0.6.an`, so the
`is_same_thread` selector always fires the trivial branch there. The
target-side wire-format probe
(`bridge-event-witness/src/bin/probe_tvm_decode.rs`) can only reach parity
verdict against a real state_v2 node.

**Prerequisite: state_v2-compatible tool binaries.** The MT harness spawns
child threads and generates a new zerostate — it needs `tvm-cli`, `sold`,
`tvm-debugger`, `zerostate-helper`, `node-helper` all built from a
tvm-sdk tree compatible with `state_v2`. A `v3.0.6.an`-era `tvm-cli`
against a state_v2 node fails silently at zerostate generation and leaves
the docker compose stack un-bootable. Point the env vars below at
state_v2 builds.

**Prerequisite: Docker Desktop VM ≥ 12 GiB.** Five nodes × ~2 GiB plus
aerospike; below 8 GiB aerospike hits `stop-writes` and block production
halts at seq_no ≈ 500 with the symptom looking like a chain hang.

**Bring up 2 threads.** Populate `bridge/multithreading/bins_<OS>/`
with state_v2-compatible builds of `tvm-cli`, `sold`, `tvm-debugger`,
`zerostate-helper`, `node-helper` (see
[`../../../multithreading/bins_macOS/README.md`](../../../multithreading/bins_macOS/README.md)),
then source the env script and run from an `acki-nacki` checkout on
branch `feature/node-3953-add-test-slow-block-builder-with-300ms-per-block-build-on`:

```sh
# From bridge/ root:
source multithreading/bins_macOS/env.sh    # or bins_linux/env.sh on Linux
export MESSAGE_ARCHIVE_OTEL_RUN_ID="local-2-thread-$(date +%Y%m%d-%H%M)"

cd "$ACKI_NACKI_ROOT"
python3 tests/mt/cli.py test-multithread-cross-thread \
  --threads 2 \
  --total 20000 \
  --hold-burst-total 5000 \
  --hold-quiet-seconds 0 \
  --batch-size 200 \
  --deploy-value 12000000000000 \
  --minimum-balance 8000000000000 \
  --hold-seconds 1800 \
  --timeout 2400
```

`env.sh` exports `CLI_NAME`, `TVM_CLI`, `SOLD`, `TVM_DEBUGGER`,
`ZEROSTATE_HELPER`, `NODE_HELPER` (all resolved to the binaries in
that directory) plus `DISABLE_MV=true`. Exporting them ahead of
`cli.py` also bypasses the auto-discovery bug at
`tests/mt/cli.py:1050`, which otherwise walks symlinks into
`tvm-sdk/target/release/` where `zerostate-helper` doesn't exist. If
binaries live elsewhere, either symlink them into `bins_<OS>/` or
export the six vars by hand.

The `--hold-*` flags are load-bearing — dropping them lets the child thread
starve and finalization stalls (the warm-up burst gets no cross-thread
refills, the child thread's last block never gets a BK quorum of
attestations, stays *prefinalized*, and `authority_switch` refuses to
open round 0 → dead thread → cross-thread messages queue up but never
get delivered → test times out). The cyclic-hold path is gated at
`tests/mt/cli.py:3046` — both `--hold-seconds` and `--hold-burst-total`
must be set for it to activate. Full end-to-end reasoning + Pane
layout for a multi-path research session is in
[`../../../multithreading/runbooks/run_multipath_session.md`](../../../multithreading/runbooks/run_multipath_session.md).

**Wait for the split.** The harness logs `split thread` when the second
thread is stable. `BRIDGE_GQL_ENDPOINT` should point at any node's port
8600 (all nodes serve the merged view).

**Sanity probe before bridge deploy.** Confirm the bridge's `v3.0.6.an`
tvm-sdk pin still decodes the node's BOCs (see the top of this section):

```sh
cd crates/bridge-prover-libraries
cargo run --release -p bridge-event-witness --bin probe-tvm-decode -- \
    --gql-url http://127.0.0.1:8600/graphql \
    --account-id <64-hex account emitting ExtOut> \
    --dapp-id    <64-hex dapp id> \
    --limit 3 -v
```

Exit 0 = safe to proceed. Exit 1 = wire format changed, migrate the
bridge's tvm-sdk pins to `state_v2` before continuing. Exit 2 = cell
descriptor layout changed, also update `boc_walk::build_cell_repr_data`.

**Firing a real cross-thread `WithdrawalInitiated`.** The paced-workload
harness spawns two threads and cycles arbitrary messages between them,
but it does not orchestrate a USDC deposit → cross-thread burn on its
own. To exercise the Circuit-4 multi-hop path you additionally need to:

1. Deploy the AN bridge (`contracts/an/exchange/eccUSDCBridge`) using
   the harness's zerostate. The deploy typically lands on thread 0.
2. Deploy a caller account whose routing places it on thread 1 (the
   split thread). AN routes by DApp ID at zerostate time — use a
   distinct DApp ID for the caller.
3. Fund the caller with ECC[3] USDC via `USDCBridge.mintAndSend` from
   the giver (`config/USDCBridge.keys.json` is the mint authority).
4. Call `burn(recipient, amount)` on the caller. The internal
   cross-thread call to the bridge fires `WithdrawalInitiated` in a
   block on thread 1 whose reference graph is reachable from thread 0. The
   daemon resolves the exact event block from `PrivateWitness.block_id_hex`,
   uses the resolver's nearest reachable thread-0 block directly as Y, and
   materialises the resulting `Y → … → X` Leaf-7 path. Circuit 4 opens Y
   in its history hierarchy; it does not advance Y along thread-0 parents.

TODO — the exact `tvm-cli` invocations for steps 1-4 are not in this
runbook yet. Fill in on first successful E2E; the harness spawns the
threads but the bridge/caller wiring is bespoke per deployment.

---

## Case 1 — L1: First-time E2E from a fresh deploy (optimal sequence)

**When to use — L1 only, one-shot fresh-deploy demo.** Prove out the
`withdrawByProofBundle` leg once, in minimum wall time (~17 min
best-case).
This is a **tight sequence**: bridge Ethereum contracts deploy →
bundle-prover daemon cold-starts → burn fires, all inside one narrow
window. The L1 daemon is subcritical (see
[Timing model](#timing-model--why-fresh-deploy-demos-need-tight-lookahead))
so its lag grows forever — a second withdraw against the same L1 deploy
soon slides past the wait-time budget. For **regular, repeated**
withdrawals use L2 mode:
[Case 2a](#case-2a--fresh-l2-deploy-first-e2e-withdrawal) (first L2 cycle)
and [Case 2b](#case-2b--sequential-l2-withdrawals-steady-state--stress-loop)
(stress loop).

**The optimal sequence** — every step gates the next; do not interleave:

### Step 0 — Pre-deploy anchor freshness check

Anchors staler than ~3 min lose bundles of catch-up time. Confirm the
lookahead is tight before running forge.

```bash
cd crates/bridge-prover-libraries/bridge-prover-lib
cargo run --release --bin compute_bridge_anchors -- \
  --at-head \
  --gql-endpoint https://shellnet.ackinacki.org/graphql
# note the printed seed_seqno and (chain_head - seed_seqno) — should be < 200
```

The printed anchors do not need to be pasted anywhere by hand —
[`crates/bridge-prover-libraries/scripts/deploy_bridge_bundle.sh`](../../bridge-prover-libraries/scripts/deploy_bridge_bundle.sh)
re-derives them internally and writes the corresponding
`L{1,2}_config/env`. The manual `compute_bridge_anchors` call above is
only useful for a freshness sanity-check before triggering the script.

### Step 1 — Deploy the bridge

Delegate to the automated wrapper — it derives anchors against fresh
chain head, deploys the 6-bridge contract bundle in Ethereum, extracts `BRIDGE_ADDRESS`,
and rewrites `L1_config/env`:

```bash
cd crates/bridge-prover-libraries
set -a && source shellnet.common && set +a
PRIVATE_KEY=$RELAYER_PRIVATE_KEY LEVEL=1 ./scripts/deploy_bridge_bundle.sh
```

### Step 2 — Seed the bridge treasury (fresh deploy only)

`withdrawByProofBundle` pays out from `treasuryBalance`
(`AckiNackiBridge.sol:1532–1533`). A fresh deploy starts at zero; the
crypto path can pass and the tx will still revert with
`WithdrawTreasuryShortfall(pub.amount, treasuryBalance)` (selector
`0xbb651fce`). The only path that increments `treasuryBalance` is
`deposit()` (`AckiNackiBridge.sol:578-593`) — there is no admin setter.
Seed it once, then reuse across demos on the same deploy.

Full recipe in [Case 3d](#case-3d--withdrawtreasuryshortfall--bridge-treasury-empty).
Quick version — fund the wallet from Circle's Sepolia faucet, then
approve + deposit into the bridge:

```bash
export BRIDGE=<new_address>
# Confirm which USDC the fresh deploy wired (DeployShellnetE2EBridge.s.sol
# pins Circle canonical Sepolia USDC; the older Pruvendo mock
# 0x94a9D9…5e4C8 is retired):
cast call $BRIDGE 'usdc()(address)' --rpc-url $RPC
# expect: 0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238

export USDC=0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238         # Circle canonical Sepolia USDC
export WALLET=0xb586356D52eAee055Ca569Ff412DFeFFc5bB2307       # relayer/deployer (single shared burner)
export AMOUNT=1000000                                          # 1.000000 USDC (6 decimals)

# 1. Fund $WALLET from Circle's public faucet.
#    Open https://faucet.circle.com, pick "Ethereum Sepolia",
#    paste $WALLET, request USDC (10 per call, throttled per addr/IP).
#    Confirm balance landed:
cast call $USDC 'balanceOf(address)(uint256)' $WALLET --rpc-url $RPC

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

**Note on self-minting.** Circle's FiatToken has a minter allowlist, so
`cast send $USDC 'mint(…)'` from a random burner reverts with
`FiatToken: caller is not a minter`. Always fund the wallet via
Circle's faucet, never via direct `mint`.

### Step 3 — Cold-start the bundle daemon

Full procedure in the [verifyBlock runbook Case 1](./live_relayer_bridge_verifyBlock_runbook.md#case-1--first-time-bootstrap-from-a-fresh-deploy).
Verify the log line `seed_policy=Explicit(<seed_seqno>)` appears within
30s. The daemon needs a running proof pipeline before the burn fires —
otherwise the covering bundle waits on daemon startup instead of on
Circuit 4.

### Step 4 — Start `withdraw-e2e --dry-run` in one shell

Start the tool **before** firing the burn. It snapshots a baseline of
existing `WithdrawalInitiated` msg_ids, then polls GQL every 2s for a
NEW event. When the burn fires in Step 5, capture picks it up in <2s
without any recovery flag. Baseline + wait is the tool's default and
correct shape for a planned demo (`withdraw_e2e/capture.rs:54-117`).

Once the event is captured, `run_once` enters an internal 30s-poll
retry loop against `prover_state.json` for up to 2 h
(`withdraw_e2e/driver.rs:156-212`, `ENRICH_TIMEOUT = 120 min`) —
reloading the file each attempt as the daemon writes new bundles.
That loop **is** the "wait for the covering bundle" — no manual poll
of `storedLastSeenBlockSeqNo` needed.

```bash
cd crates/bridge-prover-libraries
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
  --event-wait-s 900 \
  --work-dir "$BRIDGE_CONFIG_DIR/work_dir" \
  --bridge-prover-libraries-dir . \
  --prover-out-dir "$BRIDGE_CONFIG_DIR/proofs" \
  --prover-seq-no $(date +%s) \
  --dry-run \
  2>&1 | tee logs/withdraw_dry_${BRIDGE_CONFIG_DIR##*/}_${TS}.log
```

`--event-wait-s 900` gives you 15 min to fire the burn in Step 5
(default is 5 min). The log will show
`waiting for WithdrawalInitiated ExtOut event` — that's the cue to
proceed.

### Step 5 — Fire the burn in another shell

```bash
cd crates/bridge-prover-libraries
MODE=shellnet python3 python/test_deploy_and_withdraw_only.py
```

The script deploys a fresh Multisig (two-shot GiverV3.sendCurrencyWithFlag
17→1 on shellnet), mints USDC via `USDCBridge.mintAndSend`, calls
`initiate_withdrawal`, and polls GQL for the `WithdrawalInitiated`
ExtOut. It prints the captured `seq_no` + `block_hash` + `msg_id`.

The Step 4 shell picks the event up within one poll interval and moves
on to enrichment / prove.

**Timing sanity.** If the printed `seq_no` is within one bundle stride
(`< daemon_last_verified + 1024`), coverage lands in the next bundle
(~12 min). Anything worse and the wait grows linearly — see the
[timing model](#timing-model--why-fresh-deploy-demos-need-tight-lookahead).

### Step 6 — Watch dry-run complete

Expected log signature in the Step 4 shell:

```
INFO capture_next_withdrawal_event: matched dst=:...:026a, seq_no=<N>
INFO enricher: filling ...  timeout_s=7200
INFO enricher attempt        (repeats every 30s until covering bundle lands)
INFO enricher: witness ready  layer_idx=0
INFO subprocess_prover: bridge-event-halo2-prover start
INFO subprocess_prover: aggregate SHPLONK ok, calldata_len=<bytes>
INFO submit_withdraw: dry-run eth_call OK — would submit withdrawByProofBundle(...)
```

Dry-run OK proves the proof is well-formed and the on-chain adapter
accepts it. If dry-run reverts, jump to
[Case 3b](#case-3b--on-chain-withdrawbyproofbundle-revert) — **do not**
submit for real.

> **Note — L1 only: what if the burn fired late and the enricher is
> looping past ~60 min?** L1 is subcritical, so every minute you delay
> the burn past daemon startup adds ~1.4 bundles of catch-up. Diagnose:
>
> ```bash
> E=<event_seq_no from python output>
> L=$(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC --json | jq -r '.[0]')
> COVER=$(( (E + 1023) / 1024 * 1024 ))    # next W·P boundary ≥ E
> BUNDLES_TO_WAIT=$(( (COVER - L) / 1024 ))
> WALL_MIN=$(( BUNDLES_TO_WAIT * 12 ))
> echo "event=$E last_seen=$L covering=$COVER  wait ≈ ${WALL_MIN} min"
> ```
>
> - **`WALL_MIN < 60`** — let the enricher keep looping; it will succeed.
> - **`WALL_MIN ≥ 120`** — the enricher will time out. Either
>   (a) wait, kill after timeout, and rerun with `--replay-latest`
>   once the covering bundle lands; or (b) redeploy at fresh chain head
>   per verifyBlock runbook
>   [Case 6](./live_relayer_bridge_verifyBlock_runbook.md#case-6--state-loss--re-bootstrap-from-mid-chain).
>   For **regular repeated** withdrawals switch to L2 (Case 2) — L2 is
>   supercritical, so daemon lag is bounded by construction.

### Step 7 — Real submit

Re-run the same command **without** `--dry-run` (drop `--event-wait-s`
too — a fresh `withdraw-e2e` invocation takes a new baseline snapshot
that will INCLUDE the prior burn's msg_id, so default capture would
never surface it. Add `--replay-latest` to skip baseline and pick the
youngest matching event):

```bash
./target/release/relayer withdraw-e2e \
  ... same flags as Step 4, minus --dry-run ... \
  --replay-latest \
  2>&1 | tee logs/withdraw_real_${BRIDGE_CONFIG_DIR##*/}_${TS}.log
```

The prover PK cache is warm from Step 6, so total wall time collapses
to `submit + confirm` (~30–60s). Look for
`withdrawByProofBundle confirmed tx=0x...`.

Verify:

```bash
cast logs --address $BRIDGE --rpc-url $RPC \
  'event WithdrawalExecuted(uint256,address,uint256,uint256)' \
  --from-block -100
```

---

## Case 2 — L2: fresh deploy and steady-state operation

L2 anchoring is the **only** regime that supports repeated withdrawals
against a single deploy. Chain moves `W² = 16384` seq_no every ~91 min;
prover finishes each bundle in ~10 min (SHPLONK-wrapped), giving
`ρ = 0.15` — supercritical. Daemon lag is bounded independent of uptime.
L1 (Case 1) is subcritical and can only support a one-shot demo per
deploy — see [prover throughput analysis](../../bridge-prover-libraries/docs/prover_throughput_analysis.md).

- **[Case 2a](#case-2a--fresh-l2-deploy-first-e2e-withdrawal)** — first
  E2E withdrawal on a fresh L2 deploy (baseline sequence, ~106 min).
- **[Case 2b](#case-2b--sequential-l2-withdrawals-steady-state--stress-loop)** —
  follow-up / repeated withdrawals against the same L2 deploy after
  Case 2a succeeds. This is the only "steady-state" path — L1 cannot
  support it.

---

### Case 2a — Fresh L2 deploy: first E2E withdrawal

**When to use.** Deploying the bridge with `BRIDGE_ANCHOR_LEVEL=2` to
exercise L2-anchoring end-to-end. Everything from Case 1 applies with
the level-swaps below; those are the only L2-specific deviations.

**Level-swap summary.**

| L1 setting | L2 setting |
|---|---|
| `BRIDGE_ANCHOR_LEVEL=1` (or unset) | `BRIDGE_ANCHOR_LEVEL=2` |
| Bundle stride = `W·P = 1024` seq_nos (~5.7 min chain-time) | Bundle stride = `W² = 16384` seq_nos (~91 min chain-time) |
| `compute_bridge_anchors` picks `layer == 1` | `compute_bridge_anchors --level 2` picks `layer == 2` |
| `withdraw-e2e --anchor-layer auto` | `withdraw-e2e --anchor-layer 2 --i-know-the-wait` (strict L2) |
| Fire burn within ~5 min of daemon start | Burn timing is irrelevant to floor wait (see L2 timing model) |

### Step 1 — Deploy and bootstrap `daemon-live` under L2

Follow the verifyBlock runbook
[Case 1](./live_relayer_bridge_verifyBlock_runbook.md#case-1--first-time-bootstrap-from-a-fresh-deploy)
(unified L1/L2 first-time bootstrap) and
[Case 7](./live_relayer_bridge_verifyBlock_runbook.md#case-7--l2-anchored-cold-start---anchor-level-2)
(L2-anchored cold-start specifics) end-to-end, substituting `LEVEL=2` /
`BRIDGE_CONFIG_DIR=./L2_config` / `BRIDGE_ANCHOR_LEVEL=2` throughout.
Those cases cover `compute_bridge_anchors --level 2`, the T₂-boundary
retry rule, `deploy_bridge_bundle.sh LEVEL=2`, and `daemon-live
--anchor-level 2` startup with drift-refusal diagnostics.

**Two withdraw-side sanity checks worth doing inline:**

```bash
# 1. Confirm the deployed contract landed on a W²-aligned seed.
export BRIDGE=<new_address>
LAST=$(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC --json | jq -r '.[0]')
python3 -c "print('L2-aligned:', $LAST % 16384 == 0, 'last_seen:', $LAST)"
# 2. Confirm the daemon booted L2 (search logs within 30 s of start):
#    INFO daemon-live: anchor_mode=L2, bundle_stride=16384
```

Then wait for the first L2 bundle to land (~91 min chain + ~10 min
prover ≈ 101 min worst-case, ~50 min typical depending on start
alignment vs next T₂) — `storedLastSeenBlockSeqNo` bumps by exactly
`16384`. Once it does, both `_layerWindows[1]` (L1) and
`_layerWindows[2]` (L2) are populated in a single `verifyBlock` call —
`AckiNackiBridge.sol:_appendLayerHashes` iterates all active layer slots
per successful proof (lines 902–913).

### Step 2 — Seed the bridge treasury

Identical to [Case 1 Step 2](#step-2--seed-the-bridge-treasury-fresh-deploy-only) — level-agnostic.

### Step 3 — Start `withdraw-e2e --anchor-layer 2 --i-know-the-wait --dry-run` in one shell

**Do NOT use `--anchor-layer auto`** under L2 stress testing. Auto
probes L1 first; whenever the event falls inside the L1 window that
follows the covering T₂ (the common case) it resolves to L1 and the
run silently downgrades. Force strict L2 with `--anchor-layer 2
--i-know-the-wait` (the ack is required for explicit L(n≥2) modes;
CLI enforces it — `bin/relayer.rs:483-486`).

Same "start before burning" reasoning as [Case 1 Step 4](#step-4--start-withdraw-e2e---dry-run-in-one-shell):
baseline + wait for a new event is the tool's default correct shape,
and the internal 2 h enricher retry loop absorbs the L2 chain-wait.

```bash
cd crates/bridge-prover-libraries
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
  --event-wait-s 900 \
  --work-dir "$BRIDGE_CONFIG_DIR/work_dir" \
  --bridge-prover-libraries-dir . \
  --prover-out-dir "$BRIDGE_CONFIG_DIR/proofs" \
  --prover-seq-no $(date +%s) \
  --dry-run \
  2>&1 | tee logs/withdraw_l2_dry_${TS}.log
```

Wait for the log line `waiting for WithdrawalInitiated ExtOut event`
before proceeding to Step 4.

### Step 4 — Fire the burn in another shell

```bash
cd crates/bridge-prover-libraries
MODE=shellnet python3 python/test_deploy_and_withdraw_only.py
```

Same script as Case 1 — burn side is level-agnostic. Note the printed
`seq_no` for the diagnostic in Step 5.

### Step 5 — Watch dry-run complete

Expected log signature in the Step 3 shell:

```
INFO capture_next_withdrawal_event: matched dst=:...:026a, seq_no=<N>
INFO enrich_witness: anchor_layer_mode=Explicit(2), i_know_the_wait=true
INFO enricher: filling ... timeout_s=7200                    # 2 h budget
INFO enricher attempt        (repeats every 30s until covering L2 bundle lands)
INFO resolved anchor: L2 (mode=Explicit(2), auto_escalated=false)
INFO chain built: anchor_layer=L2, active_links=1, ...
INFO enricher: witness ready  layer_idx=1                    # 0-indexed → L2
INFO submit_withdraw: dry-run eth_call OK — would submit withdrawByProofBundle(...)
```

`layer_idx=1` is the ground-truth confirmation that the witness is
L2-anchored; anything else (`layer_idx=0`) means an accidental L1
fallback happened. Under `Explicit(2)` this cannot occur by
construction — the enricher passes the level through and the slot
lookup indexes `layer_windows[1]` unconditionally.

**How long to expect.** Chain-side wait to next W²-boundary is bounded
by ~91 min; add ~10 min prover ⇒ ~50 min typical, ~101 min
worst-case. Diagnostic (optional):

```bash
E=<event_seq_no from python output>
COVER=$(( (E + 16383) / 16384 * 16384 ))    # next W²-boundary ≥ E
L=$(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC --json | jq -r '.[0]')
python3 -c "print(f'event={$E} last_seen={$L} covering_T2={$COVER}  wait≈{max(0,($COVER-$L))*0.5/60:.0f} min chain + ~10 min prover')"
```

**If the enricher's 2 h budget expires** (`ENRICH_TIMEOUT = 120 min`,
`withdraw_e2e/driver.rs:162`), the daemon never landed a covering L2
bundle. That's a bundle-lane issue, not a withdraw-lane issue — check
`daemon-live` logs and the verifyBlock runbook.

### Step 6 — Real submit

Same as [Case 1 Step 7](#step-7--real-submit): drop `--dry-run`, add
`--replay-latest` (the burn is already in the baseline of any new
`withdraw-e2e` run), keep everything else including `--anchor-layer 2
--i-know-the-wait`.

---

### Case 2b — Sequential L2 withdrawals (steady-state / stress loop)

**When to use.** After Case 2a's first cycle succeeds, drive 2–3
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

# 2. Confirm treasury still funded (seed 3–5 USDC once at Case 2a Step 2)
cast call $BRIDGE 'treasuryBalance()(uint256)' --rpc-url $RPC

# 3. Start withdraw-e2e --dry-run in one shell (baseline snapshot,
#    then internal 2 h enricher wait). Same flags as Case 2a Step 3,
#    with a fresh --prover-seq-no.
./target/release/relayer withdraw-e2e \
  ... same flags as Case 2a Step 3 ... \
  --prover-seq-no $(date +%s)

# 4. Fire fresh burn in another shell
MODE=shellnet python3 python/test_deploy_and_withdraw_only.py

# 5. Wait for the Step 3 shell to complete dry-run OK (~50–101 min).

# 6. Real submit — same as Case 2a Step 6 (drop --dry-run, add --replay-latest).
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
per-layer picks; `withdrawByProofBundle`'s `_isKnownLayerAnchor` scans
only the window named by `anchorLayer` — see change log), any revert in
cycle N ≥ 2 is almost certainly reproducing a Case 3b failure mode,
not something L2-specific.
Start with the Case 3b catalog before diagnosing L2.

---

## Case 3 — Incidents & failure modes

When a `withdraw-e2e` run fails or reverts, one of the following four
incident patterns almost always applies. Grouped by where the failure
surfaces:

- **[Case 3a](#case-3a--prover-subprocess-timeout--oom)** — prover
  subprocess timeout / OOM (host side; before the on-chain call).
- **[Case 3b](#case-3b--on-chain-withdrawbyproofbundle-revert)** — on-chain
  `withdrawByProofBundle` revert (proof is well-formed; contract rejects it).
- **[Case 3c](#case-3c--usdcbridge-key-drift-burn-side)** — USDCBridge
  key drift (burn side; the AN-side script itself fails before an event
  is ever emitted).
- **[Case 3d](#case-3d--withdrawtreasuryshortfall--bridge-treasury-empty)** —
  `WithdrawTreasuryShortfall` (crypto path passes; payout leg reverts
  because the bridge treasury has no USDC).

---

### Case 3a — Prover subprocess timeout / OOM

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
  ...same flags as Case 1 Step 4, plus --replay-latest... \
  --prover-timeout-s 3600
```

Bumping `--prover-timeout-s` doesn't fix a real OOM — it just delays the
inevitable. Use only when the pipeline was slow (e.g. cold PK cache), not
when the log shows repeated swap.

---

### Case 3b — On-chain `withdrawByProofBundle` revert

**Symptom.** `withdraw-e2e --dry-run` (or real submit) fails with a
Sepolia revert. The log prints the selector.

**Decode with `cast 4byte`** or via `withdrawByProofBundle`'s declared
errors (`AckiNackiBridge.sol:460–467, 1391–1533`):

| Selector | Error | Root cause pattern |
|---|---|---|
| — | `WithdrawByProofBundleDisabled()` | Either `bridgeWithdrawalFinalVerifier()` or `bridgeMultiHopVerifier()` is unset on this deploy (`AckiNackiBridge.sol:1391–1394`). Neither getter should ever be `address(0)` on a Sepolia deploy — see the [`bridgeWithdrawalFinalVerifier` / `bridgeMultiHopVerifier` pre-flight](#dry-run-trace-any-revert) below. Anvil is the only chain where `WIRE_WITHDRAW_BY_PROOF=false` is permitted; if you hit this on Sepolia, the deploy is broken. |
| `WithdrawalProofRejected()` | FinalProof (`BridgeWithdrawalAggregatorVerifier`) SHPLONK adapter equality prelude failed, or the crypto pairing failed | The 13 inner-instance slots recovered from `finalProof` calldata (`instances[…]`, see `contracts/ethereum/verifiers/BridgeWithdrawalAggregatorVerifier.sol`) don't match the `WithdrawalFinalPublicInputs` struct assembled from `finalPublicInputs[0..12]` — or `_verifyShplonk` returned false. Note that `acc_fr` drift and a missing/mis-selected anchor both revert earlier with the more specific `WithdrawIdentityMismatch` (`AckiNackiBridge.sol:1437`) and `UnknownAnchor` respectively, so if you're seeing `WithdrawalProofRejected` those two are already ruled out. Look for a stale/mis-packed calldata blob or a fresh keygen against the wrong VK. |
| `MultiHopProofRejected(uint256 hopIndex)` | Hop `hopIndex`'s SHPLONK proof failed against `BridgeMultiHopAggregatorVerifier` | Only reachable on cross-thread claims (`hopPublicInputs.length > 0`). The failing hop's `(hopStartBlockId, hopEndBlockId)` pair did not verify under the MultiHop VK, or its calldata is stale. Same-thread claims (`hopPublicInputs.length == 0`) cannot fire this. |
| `SameThreadEndpointsMismatch()` | Empty `hopPublicInputs` / `hopProofs` but `finalPublicInputs[PUB_X_BLOCK_ID] != finalPublicInputs[PUB_Y_BLOCK_ID]` (`AckiNackiBridge.sol:1482–1485`) | The witness claims same-thread by omitting hops, but its `xBlockId` and `yBlockId` slots disagree. Either the witness enrichment failed to identify a same-thread event and should have emitted at least one hop, or the FinalProof PIs are corrupted. Regenerate the witness — same-thread requires `xBlockId == yBlockId` by construction. |
| `SameThreadRequiresEmptyHopChain(uint256 hopCount)` | `hopPublicInputs.length > 0` but `xBlockId == yBlockId` (`AckiNackiBridge.sol:1492–1494`) | Mirror of the above: hops supplied against a same-thread event. Regenerate. |
| `HopChainHeadMismatch()` / `HopChainTailMismatch()` / `AdjacentHopBlockIdMismatch(uint256 i)` | The `xBlockId → hop0.start → hop0.end → … → hopLast.end → yBlockId` fold is broken at head, tail, or between hop `i` and `i+1` (`AckiNackiBridge.sol:1503–1513`) | Witness composition dropped or reordered a hop. Regenerate. |
| `FinalPublicInputsBadLength(uint256 got, uint256 expected)` | `finalPublicInputs.length != FINAL_PI_LEN` (13) (`AckiNackiBridge.sol:1397–1399`) | Client / CLI encoded the wrong-length array. `FINAL_PI_LEN` is a compile-time constant of 13 on this contract. |
| `HopPublicInputsBadLength(uint256 hopIndex, uint256 got, uint256 expected)` | Hop `hopIndex` did not carry exactly 2 PIs (`MULTI_HOP_PI_LEN`) (`AckiNackiBridge.sol:1409–1413`) | Client passed the wrong shape inside the `uint256[][]`. |
| `HopPublicInputsHopProofsLengthMismatch(uint256,uint256)` / `HopBundleLengthOverflow(uint256 got, uint256 max)` | `hopPublicInputs.length != hopProofs.length`, or count exceeds `N_BUNDLE_MAX = 20` (`AckiNackiBridge.sol:1400–1407, 89`) | Client bug. `N_BUNDLE_MAX` is a compile-time constant. |
| `WithdrawIdentityMismatch()` | `pub.dappFr`/`pub.accFr` don't match the bridge's stored withdrawal identity | Reverts before crypto (`AckiNackiBridge.sol:1437`). `bridgeWithdrawalAccFr` (and its `dappFr` sibling) are `immutable` on this contract (`AckiNackiBridge.sol:229`), so a witness "regeneration" cannot clear this — the same witness would be rejected on a rerun. The fix is on the ETH side: redeploy `AckiNackiBridge` with `(dappFr, accFr)` set to the AN-side withdrawal identity the prover is aimed at, or point the prover at the identity the currently-deployed bridge was constructed with. **This installation cannot pay out burns that have already been emitted against the "wrong" identity.** The public inputs of a Circuit-4 FinalProof bind `accFr` (slot 7) and `dappFr` (slot 6) at proving time — these are the bridge-withdrawal identity slots, distinct from `senderAccFr` (slot 5), which is the burning account's own Fr-encoded ID and is not what the contract compares. At `withdrawByProofBundle` time `AckiNackiBridge.sol:1437` checks `pub.dappFr == bridgeWithdrawalDappFr` and `pub.accFr == bridgeWithdrawalAccFr` (both `immutable`, `AckiNackiBridge.sol:229`), so a burn whose event carries identity X will never verify against a bridge deployed for identity Y. If the operator switches identities by redeploying, users whose burns were emitted before the switch have to be made whole outside this contract (custodial refund, or a fresh burn against the new identity). See [`WITHDRAW_ACC_FR` derivation](#reference-values-chain-invariant-on-shellnet). |
| `NullifierAlreadyUsed(uint256)` | Same nullifier consumed twice | The `withdraw-e2e` command was re-run against the same captured event (identical `(block_id, tokenId, amount, recipient, sender, events_pos)` 7-tuple → identical Poseidon nullifier). Fire a fresh burn — no proof-side workaround exists. `events_pos` is bound into the preimage, so two *distinct* `WithdrawalInitiated` events in the same AN block do not collide — this error truly means the same event was replayed. |
| `UnknownAnchor(uint256 finalRoot)` | `pub.finalRoot` not present in `_layerWindows[pub.anchorLayer]` | Covering bundle not yet on-chain at the given anchor layer, or the proof was built against a stale/mis-selected anchor. Wait for the bundle daemon to submit + confirm the covering bundle, then retry — or fix the anchor selection upstream. |
| `LayerOutOfRange(uint8)` | `pub.anchorLayer` outside `1..=MAX_LAYER_HASHES` (`AckiNackiBridge.sol:1471–1475`) | Wrong anchor layer supplied. `MAX_LAYER_HASHES = 10` is a compile-time constant in `AckiNackiBridge.sol:65`; check the witness picked a layer inside that range. |
| `WithdrawTreasuryShortfall(uint256,uint256)` = `0xbb651fce` | `pub.amount > treasuryBalance` (`AckiNackiBridge.sol:1532–1533`) | Crypto path already passed; only the payout leg is blocked. Seed the treasury via `deposit()` — see [Case 3d](#case-3d--withdrawtreasuryshortfall--bridge-treasury-empty). |

Note: `WithdrawByProofDisabled()` from the pre-split contract has been
renamed to `WithdrawByProofBundleDisabled()`; any older log or trace
still mentioning the legacy selector is against a pre-bundle deploy and
does not apply to the current codebase.

**Dry-run trace (any revert):**

```bash
# 1. Confirm the split-verifier wiring is intact. Either address zero
#    → the entry-point is disabled and every call reverts with
#    WithdrawByProofBundleDisabled().
cast call $BRIDGE 'bridgeWithdrawalFinalVerifier()(address)' --rpc-url $RPC
cast call $BRIDGE 'bridgeMultiHopVerifier()(address)'        --rpc-url $RPC

# 2. Re-run the exact eth_call with --trace for a decoded reason.
#    withdrawByProofBundle takes a uint256[13] finalPublicInputs, a
#    bytes finalProof, a uint256[][] hopPublicInputs (each inner array
#    is exactly [hopStart, hopEnd]), and a bytes[] hopProofs of the
#    same outer length. Same-thread claims pass empty [] / [] for the
#    two hop arrays.
#
#    finalPublicInputs slot order (13 total):
#      [0]  tokenId
#      [1]  amount
#      [2]  recipientHi
#      [3]  recipientLo
#      [4]  dstChainId
#      [5]  senderAccFr
#      [6]  dappFr
#      [7]  accFr
#      [8]  nullifier
#      [9]  finalRoot
#      [10] anchorLayer
#      [11] xBlockId  (PUB_X_BLOCK_ID)
#      [12] yBlockId  (PUB_Y_BLOCK_ID)
cast call $BRIDGE \
  'withdrawByProofBundle(uint256[],bytes,uint256[][],bytes[])' \
  "[<13 comma-separated finalPublicInputs values from log>]" \
  <final_calldata_hex_from_log> \
  "[]" \
  "[]" \
  --rpc-url $RPC --trace

# For a cross-thread claim (not in scope for the current CLI, but the
# contract accepts it), replace the two trailing "[]" args with, e.g.:
#   "[[<hop0_start>,<hop0_end>],[<hop1_start>,<hop1_end>]]"  \
#   "[<hop0_calldata_hex>,<hop1_calldata_hex>]"
```

**Do NOT** delete `state/prover_state.json` or the witness JSON — the
proof is deterministic per `(event, prover_state)`. Fixing the on-chain
side (wait, redeploy) and re-running the same command regenerates the
same proof against the warm cache.

---

### Case 3c — USDCBridge key drift (burn side)

**Symptom.** `test_deploy_and_withdraw_only.py` fails during
`mintAndSend` step with `exit_code=209` (or similar TVM signature error).

**Root cause pattern.** Bundled `python/contracts/USDCBridge.shellnet.keys.json`
public key ≠ on-chain `getOwnerPubkey`. 

**Check + fix.**

```bash
cd crates/bridge-prover-libraries

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

### Case 3d — `WithdrawTreasuryShortfall` — bridge treasury empty

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

**Fix — fund the wallet from Circle's Sepolia faucet, then deposit.**
`DeployShellnetE2EBridge.s.sol` wires the bridge against Circle
canonical Sepolia USDC (`0x1c7D…7238`) — the same contract Circle's
public faucet at <https://faucet.circle.com> dispenses. The retired
Pruvendo mock (`0x94a9D9…5e4C8`) with permissionless `mint(…)` no
longer applies. Circle's FiatToken has a minter allowlist, so
self-`mint` via `cast send` reverts with `FiatToken: caller is not a
minter`.

```bash
cd crates/bridge-prover-libraries
# BRIDGE_CONFIG_DIR must already be exported (./L1_config or ./L2_config)
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a

# Confirm which USDC the fresh deploy wired (defends against future
# USDC rotations — the address below assumes the current pin):
cast call $BRIDGE_ADDRESS 'usdc()(address)' --rpc-url $RPC_URL
# expect: 0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238

export USDC=0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238   # Circle canonical Sepolia USDC
export WALLET=$(cast wallet address --private-key $RELAYER_PRIVATE_KEY)
export AMOUNT=1000000    # cover at least one burn (1.000000 USDC)

# 1. Fund $WALLET from Circle's faucet.
#    Open https://faucet.circle.com, pick "Ethereum Sepolia",
#    paste $WALLET, request USDC (10 per call, throttled per addr/IP;
#    repeat for larger AMOUNT budgets).
cast call $USDC 'balanceOf(address)(uint256)' $WALLET --rpc-url $RPC_URL   # should show ≥ $AMOUNT

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

**If `deposit()` reverts `ERC20: transfer amount exceeds allowance`
despite a successful `approve`,** the wallet is holding balance of the
*wrong* USDC contract (e.g. the retired Pruvendo mock). Re-run the
`usdc()` check above and fund the correct token via Circle's faucet.

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

Everything lives under `crates/bridge-prover-libraries/`. Withdrawal E2E adds
three directories on top of the bundle-lane layout in the parent runbook:

```
crates/bridge-prover-libraries/
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



