# Live `daemon-live` E2E Runbook — shellnet → Sepolia (`verifyBlock` + BK-set rotation)

Operational guide for running the `bridge-relayer-daemon daemon-live` binary
against a deployed `AckiNackiBridge` on Sepolia, driven by the shellnet
GraphQL endpoint. Covers first-time bootstrap, steady-state operation,
BK-set rotation handling, and recovery from the failure modes we have
actually hit in production.

**Aim of this runbook.** Stand up and keep running the relayer daemon so
the on-chain bridge state (its global history — anchor chain, per-layer
history window, latest verified bundle, latest BK-set commitment)
advances continuously in step with the Acki Nacki chain. The daemon
polls shellnet, proves key blocks with some specified cadence (not each
key block!), and posts `verifyBlock` to Sepolia; when the chain rotates
its BK set, the daemon detects the rotation off-chain, submits
`applyBkSetUpdate`, and keeps proving against the right (incoming or
outgoing) BK set per the ETH-36 two-slot model.

**Scope of this runbook.** Bundle-only path:

- Circuit 1A/1B (attestation) + Circuit 2 (layer hashes), aggregated by
  SHPLONK, submitted via `verifyBlock`.
- BK-set rotation: Circuit 3 (BkSetUpdateChecker) proof submitted via
  `applyBkSetUpdate`, with ETH-36 two-slot semantics (an `applyBkSetUpdate`
  may land ahead of the layer cursor; `verifyBlock` accepts
  `storedPrevBkSetCommitment` for `seqNo <= N` where `N =
  storedLastBkSetUpdateSeqNo`; the prover-side ack is deferred until the
  next bundle target passes `N`).

Event-level withdrawal proofs (Circuit 4 / `withdrawByProof`) are **out
of scope** — see the withdraw-side runbook.

> **Notation.** `seq_no` is the Acki Nacki block sequence number.
> A **key block** is a block at height `seq_no` where `seq_no % W == 0`
> (producer-side, `W = 128` — the historical window size). A key block
> carries essential AN historical data that will serve as an anchor in
> the Ethereum bridge contract. A **bundle** is what the daemon actually
> proves; its stride depends on anchor mode — `W·P = 1024` under L1
> (thinning `P = 8`), `W² = 16384` under L2 (no thinning).

**Why we do not prove every key block.** A single bundle proof
(Circuit 1A + Circuit 2, aggregated by SHPLONK) costs ~10 min
warm / ~14 min cold on dev hardware. Shellnet emits a key block every
`W` = 128 source blocks and runs fast — at the observed ~3 b/s that is
one key block every ~43 s — so a per-key-block prover would fall ~15×
behind. Two levers stack to make the system viable:

- **Thinning** (bridge-side, param `P`). Relay only every `P`-th key
  block, i.e. heights `SEQ_NO % (W*P) == 0`. Current setting
  `W = 128, P = 8` → bundle stride 1024 source blocks (~5 min
  chain-time).
- **Anchor level** (on-chain, env `BRIDGE_ANCHOR_LEVEL`). Controls how
  often the bridge advances its covering layer-hash on Sepolia, and
  therefore how often a user can withdraw.
  - `L1` (stride `W·P = 1024`, ~5 min shellnet-time) — every bundle is
    an anchor. Convenient for one-fire-and-withdraw E2E tests and CI;
    about 30–50 minutes per test.
  - `L2` (stride `W² = 16384`, ~91 min shellnet-time) — the production
    variant. Thinning does **not** apply here: the daemon proves one
    bundle per full `W²` window directly (single L2 hop, `chain_steps =
    1` in Circuit 2). A user waits up to ~2 h for their withdrawal to
    become provable — a *constant* cadence — in exchange for far fewer
    on-chain writes and lower gas.

> **Runtime layout.** For a long-running L2 server, use the production
> [Docker Compose kit](../deploy/shellnet-l2/README.md): runtime state
> and heavyweight proving data stay in bind mounts, while the filled
> runtime env (including the signing key) stays outside Git. The
> direct-CLI sections below remain useful for development and recovery.

---

## BK-set rotation on shellnet — what to know before you start

Shellnet's rotation is governed by the `minBlockKeepers` field of the
`BlockKeeperContractRoot` TVM contract (address `0:7777…7777`, dapp 0).
With 5 active BKs the gate is `activeBK - 1 >= minBK`:

| `minBK` | effect on 5-BK shellnet | relayer sees |
|---:|:---|:---|
| **5** | A BK cannot leave the epoch — gate fails. No rotations ever fire. | `storedLastBkSetUpdateSeqNo` stays 0 forever. |
| **4** | After each epoch (~660 s + cliff/wait) one BK may rotate out; a candidate (if any is queued) rotates in. | Shellnet publishes a BK-set update; the daemon detects it, proves Circuit 3, submits `applyBkSetUpdate`. |

`minBK` is set by calling `setConfig` on `BlockKeeperContractRoot` with
the owner key (`config/BlockKeeperContractRoot.keys.json`). `setConfig`
overwrites **all six** config fields in one shot:

```
setConfig(uint64 epochDuration,
          uint128 minBlockKeepers,
          bool    isNeedNumberOfActiveBlockKeepers,
          uint128 needNumberOfActiveBlockKeepers,
          uint8   walletTouch,
          uint128 nlinit)
```

Only `epochDuration` has a getter (`getConfig`); the other five fields
must be reconstructed from the deploy env. Current shellnet defaults
(from `contracts/scripts/generate_zerostate.py`): `epochDuration=660`,
`isNeed=false`, `needNumber=0`, `walletTouch=200`, `nlinit=5000`.

**Operational consequence.** `bk_set.shellnet.json` in-repo is a
**bootstrap snapshot** — correct when it was captured, stale as soon as
the first rotation lands. After rotation starts firing, the daemon
updates its on-disk BK set from `applyBkSetUpdate` ACKs
(`$BRIDGE_CONFIG_DIR/state/prover_bk_set.json`); never hand-edit
`bk_set.shellnet.json` to track live rotations — resynchronize via a
fresh bootstrap or let `Resurrect` rebuild from chain.

**Capturing the state of rotation at runtime** (two getters, both
level-opaque):

```bash
cast call $BRIDGE 'storedLastBkSetUpdateSeqNo()(uint64)'  --rpc-url $RPC
# 0  → no rotation has ever been applied (minBK=5 regime, or fresh deploy)
# N  → latest applied rotation seq_no; verifyBlock accepts the previous
#      commitment for every seqNo <= N

cast call $BRIDGE 'storedBkSetCommitment()(uint256)'      --rpc-url $RPC
cast call $BRIDGE 'storedPrevBkSetCommitment()(uint256)'  --rpc-url $RPC
# Two-slot model: `stored` is incoming, `storedPrev` is outgoing.
# After the first rotation, both are non-zero and differ.
```

The daemon's view of the same state (local observation cache):

```bash
jq '{
  seen:        .last_observed_on_chain.last_seen_block_seq_no,
  last_bk:     .last_observed_on_chain.last_bk_set_update_seq_no,
  bk_commit:   .last_observed_on_chain.bk_set_commitment,
  prev_bk:     .last_observed_on_chain.prev_bk_set_commitment,
  bk_attempts: .bk_update_attempts_since_progress
}' "$BRIDGE_CONFIG_DIR/relayer-state.json"
```

Equivalence after every observation cycle:
`local.last_bk == chain.storedLastBkSetUpdateSeqNo`.
Any drift is a Case 5 revert waiting to happen.

---

## Table of Contents

- [BK-set rotation on shellnet — what to know before you start](#bk-set-rotation-on-shellnet--what-to-know-before-you-start)
- [Quick resume checklist (returning to a running system)](#quick-resume-checklist-returning-to-a-running-system)
- [Production L2 server (Docker Compose)](#production-l2-server-docker-compose)
- [Binary + env prerequisites](#binary--env-prerequisites)
- [Deploy your own bridge bundle from scratch](#deploy-your-own-bridge-bundle-from-scratch)
- [Case 1 — First-time bootstrap from a fresh deploy](#case-1--first-time-bootstrap-from-a-fresh-deploy)
- [Case 2 — Steady-state operation](#case-2--steady-state-operation)
- [Case 3 — Clean restart (no state loss)](#case-3--clean-restart-no-state-loss)
- [Case 4 — Restart after RPC-induced hard-abort](#case-4--restart-after-rpc-induced-hard-abort)
- [Case 5 — Restart after on-chain revert](#case-5--restart-after-on-chain-revert)
- [Case 6a — Chain-resurrect (advanced contract + fresh daemon)](#case-6a--chain-resurrect-advanced-contract--fresh-daemon)
- [Case 6b — State loss / re-bootstrap from mid-chain](#case-6b--state-loss--re-bootstrap-from-mid-chain)
- [Case 7 — L2-anchored cold-start (`BRIDGE_CONFIG_DIR=./L2_config`)](#case-7--l2-anchored-cold-start-bridge_config_dirl2_config)
- [Case 8 — Observed BK-set rotation (routine, no action required)](#case-8--observed-bk-set-rotation-routine-no-action-required)
- [Case 9 — Phase 1 Stuck: two rotations with no bundle target between them](#case-9--phase-1-stuck-two-rotations-with-no-bundle-target-between-them)
- [Case 10 — `bk_set.shellnet.json` has gone stale after the first rotation](#case-10--bk_setshellnetjson-has-gone-stale-after-the-first-rotation)
- [Health checks (run any time)](#health-checks-run-any-time)
- [File & state reference](#file--state-reference)
- [Onboarding wrap-up — new operator on a fresh L2 server](#onboarding-wrap-up--new-operator-on-a-fresh-l2-server)

---

## Quick resume checklist (returning to a running system)

Run this **before touching anything** — it takes 30 seconds and tells
you exactly which case (below) applies, including whether a rotation is
pending or in-flight.

```bash
cd crates/bridge-prover-libraries
export BRIDGE_CONFIG_DIR=./L1_config      # or ./L2_config
export RELAYER_STATE_PATH="$BRIDGE_CONFIG_DIR/relayer-state.json"
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
export BRIDGE=$BRIDGE_ADDRESS
export RPC=$RPC_URL

# 1. Is the daemon alive?
pgrep -af 'relayer .*daemon-live' || echo "DAEMON NOT RUNNING"

# 2. Where is local state?
echo "local  last_processed = $(jq -r '.last_processed_seqno' "$RELAYER_STATE_PATH")"
echo "local  attempts       = $(jq -r '.attempts_since_progress' "$RELAYER_STATE_PATH")"
echo "local  bk_attempts    = $(jq -r '.bk_update_attempts_since_progress' "$RELAYER_STATE_PATH")"
echo "local  observed_seen  = $(jq -r '.last_observed_on_chain.last_seen_block_seq_no' "$RELAYER_STATE_PATH")"
echo "local  observed_bkN   = $(jq -r '.last_observed_on_chain.last_bk_set_update_seq_no' "$RELAYER_STATE_PATH")"
echo "$BRIDGE_CONFIG_DIR/state/prover_state mtime:  $(stat -c '%y' "$BRIDGE_CONFIG_DIR/state/prover_state.json" 2>/dev/null || echo MISSING)"
echo "$BRIDGE_CONFIG_DIR/state/prover_bk_set mtime: $(stat -c '%y' "$BRIDGE_CONFIG_DIR/state/prover_bk_set.json" 2>/dev/null || echo MISSING)"

# 3. Where is on-chain?
echo "chain  last_seen  = $(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)'      --rpc-url $RPC --json | jq -r '.[0]')"
echo "chain  last_bkN   = $(cast call $BRIDGE 'storedLastBkSetUpdateSeqNo()(uint64)'    --rpc-url $RPC --json | jq -r '.[0]')"
echo "chain  bk_commit  = $(cast call $BRIDGE 'storedBkSetCommitment()(uint256)'        --rpc-url $RPC)"
echo "chain  bk_prev    = $(cast call $BRIDGE 'storedPrevBkSetCommitment()(uint256)'    --rpc-url $RPC)"

# 4. Latest log tail
LOG=$(ls -t logs/live_*.log 2>/dev/null | head -1)
echo "latest log: $LOG"
tail -30 "$LOG" 2>/dev/null | grep -E '(ERROR|WARN|verifyBlock|applyBkSetUpdate|bk-set|rotation|seed policy|stuck|Explicit|Resume)'
```

**Decision matrix:**

| pgrep | local == chain (seen, bkN) | attempts / bk_attempts | Go to |
|---|---|---|---|
| running | all equal | both 0 | Nothing — [Case 2](#case-2--steady-state-operation) |
| running | all equal | attempts > 0 | Usually healthy `NotYetAvailable` before next boundary — confirm in logs |
| running | `local.last_bk < chain.last_bk` | bk_attempts rising | Daemon is actively trying to apply a rotation — see [Case 8](#case-8--observed-bk-set-rotation-routine-no-action-required) |
| not running | all equal | 0 | [Case 3](#case-3--clean-restart-no-state-loss) |
| not running | all equal | > 0 | Classify final log first; [Case 4](#case-4--restart-after-rpc-induced-hard-abort) only for real hard-abort |
| not running | **any drift** | any | [Case 5](#case-5--restart-after-on-chain-revert) or [Case 6b](#case-6b--state-loss--re-bootstrap-from-mid-chain) — do not restart blindly |
| not running | last_bkN very old vs chain | any | [Case 10](#case-10--bk_setshellnetjson-has-gone-stale-after-the-first-rotation) if `prover_bk_set.json` missing/mismatched |
| any | `$BRIDGE_CONFIG_DIR/state/` missing | — | [Case 6a](#case-6a--chain-resurrect-advanced-contract--fresh-daemon) (auto-resurrect from chain) |

Container deployment: use `sudo deploy/shellnet-l2/scripts/status.sh`
instead. It performs the same reconciliation (including BK-set
reconciliation) and also reports container exit, OOM/restart state, EOA
nonce/balance, shellnet head and recent significant logs without
printing the private key.

---

## Production L2 server (Docker Compose)

The supported long-running server wrapper is
[`deploy/shellnet-l2/`](../deploy/shellnet-l2/README.md). It provides
an immutable non-root image, read-only preflight, bounded Docker logs,
explicit bind mounts, status tooling and example env files containing
placeholders only. The operator flow is:

1. deploy a fresh L2 bridge near shellnet head and retain its broadcast record;
2. provision and seal SRS/inner keys for K=17,19,20,21,22;
3. build target-host release binaries and the image from one pinned commit;
4. install the filled runtime env outside the repository with mode `0640`;
5. run `docker compose run --rm preflight`, then
   `docker compose up -d relayer`;
6. after the first confirmation, require local/on-chain cursor equality
   (both `last_seen` **and** `last_bk_set_update_seq_no`) and perform
   one controlled stop/start to prove the `WarmResume` path.

`restart: unless-stopped` restores the service after an unexpected exit
or a Docker/host restart. The container reruns the full fail-closed
preflight on every start; a pending nonce, artifact drift or
state/on-chain mismatch (including BK-set drift) blocks the daemon
before it can send a transaction. Alert on repeated restarts and stop
the service for reconciliation after a persistent logical rejection.
Never start two daemons with the same bridge/EOA/state tuple.

---

## Binary + env prerequisites

Working directory: `crates/bridge-prover-libraries/`.

### Step 1 — Choose the anchor mode

`BRIDGE_CONFIG_DIR` is the single mode selector. Pick **one** of the
two lines below and export it in the shell you will use for all
subsequent commands:

```bash
cd crates/bridge-prover-libraries

export BRIDGE_CONFIG_DIR=./L2_config   # production on a server (W² = 16384 stride)
# — OR —
export BRIDGE_CONFIG_DIR=./L1_config   # one-off withdraw test  (W·P = 1024 stride)
export RELAYER_STATE_PATH="$BRIDGE_CONFIG_DIR/relayer-state.json"
```

Everything downstream (`state/`, `proofs/`, `work_dir/`, log names)
resolves against this one variable via `bridge-prover-lib::paths`.

### Step 2 — Build binaries (one-time, per fresh clone)

```bash
# 2a. Relayer daemon binary
cargo build --release --locked -p bridge-relayer-daemon --bin relayer
#     -> ./target/release/relayer

# 2b. Aggregator subprocess (mandatory for daemon-live)
( cd ../bridge-evm-aggregator && cargo build --release --locked --bin aggregate-proof )
```

### Step 3 — Provision the Hermez KZG SRS (one-time, large download + CPU)

`daemon-live` does **not** auto-download SRS on first launch. The full
live bundle path needs K=17,19,20,21,22; K=22 is specifically required
by the layer outer aggregator; K=21 is required by Circuit 3
(BkSetUpdateChecker) wrap+aggregate when a rotation lands.

```bash
# 3a. Manually fetch K=21 (~2.4 GB). Hermez trusted-setup mirror.
mkdir -p ~/.cache/halo2-kzg-srs
curl -L --fail --progress-bar \
  https://storage.googleapis.com/aptos-circuit-testing-setups/ptau/powersOfTau28_hez_final_21.ptau \
  -o ~/.cache/halo2-kzg-srs/powersOfTau28_hez_final_21.ptau

# 3b. Materialise all five SRS files from the shared K=20 trust anchor.
cargo build --release -p bridge-prover-lib --bin bootstrap_hermez_srs
./target/release/bootstrap_hermez_srs --k 17 --k 19 --k 20 --k 21 --k 22
```

Pin and verify the resulting artifact manifest before starting a
production container.

### Step 4 — Source the mode env file

```bash
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
```

The development env file first sources `../shellnet.common` for shared
test settings and then sets the three mode-specific vars
(`BRIDGE_ADDRESS`, `BRIDGE_BOOTSTRAP_SEQNO`, `BRIDGE_ANCHOR_LEVEL`).

For a server, do not edit either tracked file. Install a dedicated
runtime env outside the clone from
[`deploy/shellnet-l2/runtime.env.example`](../deploy/shellnet-l2/runtime.env.example)
and let Compose mount it read-only.

### Step 5 — Sanity-check the resulting environment

```bash
for v in RPC_URL BRIDGE_ADDRESS RELAYER_PRIVATE_KEY \
         BRIDGE_GQL_ENDPOINT BRIDGE_AGGREGATOR_DIR BRIDGE_VERIFIERS_DIR \
         BRIDGE_PARAMS_DIR BRIDGE_CONFIG_DIR BRIDGE_BK_SET_CONFIG \
         BRIDGE_BOOTSTRAP_SEQNO; do
  [ -n "${!v:-}" ] && echo "  ok  $v" || echo "  FAIL $v"
done
```

All ten variables must print `ok`. Optional:
`BRIDGE_GQL_FAILOVER_ENDPOINTS` (comma-separated GraphQL endpoints
tried after `BRIDGE_GQL_ENDPOINT` exhausts its three retries) and
`RELAYER_METRICS_ADDR` (Prometheus `/metrics` bind address).
`BRIDGE_BOOTSTRAP_SEQNO` must equal the contract's
`storedLastSeenBlockSeqNo` at construction.

**BK-set-specific sanity check.** `BRIDGE_BK_SET_CONFIG` points at the
bootstrap BK-set file. For shellnet the default `./bk_set.shellnet.json`
is correct **only until the first rotation**; after that, the daemon
state (`$BRIDGE_CONFIG_DIR/state/prover_bk_set.json`) is authoritative.
A **fresh** cold-start must still see
`SHA-256($BRIDGE_BK_SET_CONFIG) == c77e3d6de5e6ea8ee96c6902f1b6ecb011bba2d631e76fa546e44ee67173898f`
only if the shellnet in question is still at its genesis BK set. For a
shellnet that has already rotated, use the chain-resurrect path
([Case 6a](#case-6a--chain-resurrect-advanced-contract--fresh-daemon))
instead and let the daemon rebuild from the on-chain commitment.

### Step 6 — Launch the daemon

```bash
./target/release/relayer --state "$RELAYER_STATE_PATH" daemon-live
```

All `daemon-live` CLI flags are exposed as `BRIDGE_*` env vars, so no
subcommand arguments are needed.

---

## Deploy your own bridge bundle from scratch

**You may not need this section.** The repo ships pre-populated
development `shellnet.common`, `L1_config/env`, and `L2_config/env`
pointing at a live Sepolia deploy. A server operator must instead use a
dedicated EOA and an external runtime env, then skip to
[Case 1](#case-1--first-time-bootstrap-from-a-fresh-deploy) — the
daemon can `Resurrect` off the on-chain state (see
[Case 6a](#case-6a--chain-resurrect-advanced-contract--fresh-daemon))
regardless of who deployed it.

Deploy your own bundle only if the shared deploy no longer suits (e.g.
different anchor level, custom W/P, isolated test lane, or you want to
own the key that owns the contract). `verifyBlock` and
`applyBkSetUpdate` are both permissionless on `AckiNackiBridge`.

**Network.** Target is Sepolia (chain 11155111). Point `RPC_URL` at a
stable, authenticated Sepolia RPC endpoint; public endpoints are
suitable for smoke tests but not unattended operation.

### 1. Create a fresh burner wallet

```bash
cast wallet new
#  Address:     0x...
#  Private key: 0x...
```

Keep the private key in a local file **outside** the repo. Never reuse
a wallet that holds real funds.

### 2. Fund it with Sepolia ETH

Faucets without an anti-Sybil mainnet-deposit gate:

- **pk910 PoW** — https://sepolia-faucet.pk910.de/
- **Google Cloud Web3 faucet** — https://cloud.google.com/application/web3/faucet/ethereum/sepolia

**Budget.** The one-shot deploy creates six logical components through
14 physical `CREATE` transactions. ~0.063 ETH on 2026-08-13 (30M gas @
2.1 gwei). Add ~0.001–0.003 ETH per `verifyBlock` submit, ~similar per
`applyBkSetUpdate` submit (less frequent). **Target ≥ 0.1 ETH before
deploy**, ≥ 0.5 ETH for a multi-day E2E run with rotations ON.

### 3. Deploy + wire — one end-to-end script

> **The one thing to know.** `GENESIS_LAST_SEEN_BLOCK_SEQNO` is **not**
> a value you pick by hand. It is derived by
> **[`compute_bridge_anchors --at-head`](../../bridge-prover-libraries/bridge-prover-lib/src/bin/compute_bridge_anchors.rs)**,
> a Rust binary that:
>
> 1. Queries shellnet chain head over GraphQL — no separate `curl`
>    needed.
> 2. Picks the newest stride-aligned boundary ≤ head
>    (L1: `⌊head / 1024⌋ · 1024`; L2: `⌊head / 16384⌋ · 16384`).
> 3. Fetches that seed block and derives the on-chain anchor from its
>    `layer_hashes[level]` and `bk_set` commitment.
> 4. Prints `KEY=value` lines to stdout, ready to `source`:
>    `GENESIS_BK_SET_COMMITMENT`, `GENESIS_PREV_MAX_LEVEL_LAYER_HASH`,
>    `GENESIS_SEED_SEQNO`, `GENESIS_SEED_HEIGHT`, `GENESIS_ANCHOR_LEVEL`.
>
> ⚠️ **Name mismatch to be aware of.** The tool emits
> `GENESIS_SEED_SEQNO`. The Solidity deploy script
> (`DeployShellnetE2EBridge.s.sol:138`) reads
> `GENESIS_LAST_SEEN_BLOCK_SEQNO` via `vm.envOr(..., 0)` — **silently
> defaulting to `0` if the var is missing**, which pins
> `storedLastSeenBlockSeqNo=0` into the constructor and permanently
> bricks the deploy. The in-repo helper script aliases them
> explicitly; do not skip that line.

The development helper ships in-repo at
[`crates/bridge-prover-libraries/scripts/deploy_bridge_bundle.sh`](../../bridge-prover-libraries/scripts/deploy_bridge_bundle.sh)
and deploys a new bundle from scratch. This is an irreversible
broadcast, not an idempotent operation: every invocation spends a new
nonce and deploys new addresses. It also rewrites the selected
development env and clears its local state. Never blindly retry after a
timeout or partial broadcast; first inspect the Foundry broadcast JSON,
account `latest`/`pending` nonces and receipts.

```bash
cd crates/bridge-prover-libraries
CONFIRM_NEW_BRIDGE_DEPLOY=DEPLOY_NEW_CONTRACTS \
  LEVEL=1 PRIVATE_KEY=<sepolia burner from §1> \
  WITHDRAW_ACC_FR=<authoritative AN bridge account field element> \
  ./scripts/deploy_bridge_bundle.sh
# or LEVEL=2 for L2 anchoring
```

`WITHDRAW_ACC_FR` identifies the expected Acki Nacki bridge account in
Circuit 4. A deliberate dummy value is acceptable only for a
`verifyBlock`-only smoke deployment; it permanently prevents meaningful
withdrawal proofs. Use the authoritative account field value for any
bridge that will serve withdrawals.

**BK-set anchor at deploy time.** The constructor receives
`GENESIS_BK_SET_COMMITMENT` and sets `storedBkSetCommitment` to it;
`storedPrevBkSetCommitment` is initialised to zero and
`storedLastBkSetUpdateSeqNo` to 0. The first `applyBkSetUpdate` moves
the current commitment into `storedPrevBkSetCommitment` and installs
the new one into `storedBkSetCommitment`.

**Why `--at-head` matters (danger).** The daemon's cold-start policy
does **no** chain-head comparison at startup — only the alignment
check. If you hand-pick a boundary *ahead* of chain head, the daemon
enters an **indefinite polling loop**, emitting `Bootstrapping {
seed_seqno=N, chain_head_seqno=<current head> }` on every tick until
chain catches up. No timeout. `--at-head` guarantees `N ≤ head`.

Now jump to [Binary + env prerequisites](#binary--env-prerequisites)
and continue with [Case 1](#case-1--first-time-bootstrap-from-a-fresh-deploy).

---

## Case 1 — First-time bootstrap from a fresh deploy

**When to use.** Contract just deployed; no
`$BRIDGE_CONFIG_DIR/state/prover_state.json` yet. One case, one code
path — L2 differs only in bootstrap seqno alignment (`W²` vs `W·P`),
covering-bundle wait time, and one log field.

**Prerequisites.** Complete [Binary + env prerequisites](#binary--env-prerequisites)
Steps 1–5 first. All commands below assume `$BRIDGE_CONFIG_DIR` is
exported and `$BRIDGE_ADDRESS`, `$RPC_URL`, etc. are populated.
L1 uses `W·P=1024`-block stride (~5.7 min per covering bundle); L2
uses `W²=16384` (~91 min per covering bundle).

**Pre-flight (contract sanity).** Level-opaque — the same storage
slots are verified regardless of `$BRIDGE_CONFIG_DIR`.

```bash
export BRIDGE=$BRIDGE_ADDRESS
export RPC=$RPC_URL

cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)'    --rpc-url $RPC   # == $BRIDGE_BOOTSTRAP_SEQNO
cast call $BRIDGE 'expectedPrevAnchor(uint8)(uint256)' $BRIDGE_ANCHOR_LEVEL --rpc-url $RPC   # == GENESIS_PREV_MAX_LEVEL_LAYER_HASH
cast call $BRIDGE 'storedBkSetCommitment()(uint256)'      --rpc-url $RPC   # == GENESIS_BK_SET_COMMITMENT
cast call $BRIDGE 'storedPrevBkSetCommitment()(uint256)'  --rpc-url $RPC   # == 0 (no rotation applied yet)
cast call $BRIDGE 'storedLastBkSetUpdateSeqNo()(uint64)'  --rpc-url $RPC   # == 0
```

If any of these don't match your deploy's genesis values, **stop** —
the deploy is broken. Do not launch the daemon.

**Handle stale artifacts before cold-start.** Two classes of stale
state must be dealt with before a fresh-deploy launch, or the daemon
hard-aborts:

1. **`relayer-state.json` in cwd (`crates/bridge-prover-libraries/`) —
   MANDATORY snapshot after every redeploy.** This file is the
   *daemon-level* observation cache (independent of
   `$BRIDGE_CONFIG_DIR/state/`, which is the *prover-lib-level*
   state). It carries `last_observed_on_chain` from the previous
   contract; on relaunch the daemon compares it against the live
   contract state and refuses to boot if they disagree —
   `startup on-chain drift vs last_observed_on_chain`. Note that
   `last_observed_on_chain` now carries BK-set state too, so a drift
   on `bk_set_commitment` or `last_bk_set_update_seq_no` also triggers
   this refusal.

   ```bash
   if [ -f relayer-state.json ]; then
     LOCAL_SEEN=$(jq -r '.last_observed_on_chain.last_seen_block_seq_no' relayer-state.json)
     if [ "$LOCAL_SEEN" != "$BRIDGE_BOOTSTRAP_SEQNO" ]; then
       mv relayer-state.json "relayer-state.pre_deploy_${BRIDGE_ADDRESS: -4}_$(date +%s).json"
     fi
   fi
   ```

2. **Prior-deploy artifacts under `$BRIDGE_CONFIG_DIR/` — archive,
   don't delete.** The development helper moves existing `state/*.json`
   into a timestamped `state.pre_deploy_*` directory; still archive the
   complete prior config before deploying against a new bridge.

   ```bash
   TS=$(date +%Y%m%d_%H%M%S)
   mkdir -p "runtime-archive/$TS"
   for path in "$BRIDGE_CONFIG_DIR"/state.pre_deploy*_* \
               "$BRIDGE_CONFIG_DIR"/proofs.pre_deploy*_* \
               "$BRIDGE_CONFIG_DIR"/work_dir submissions; do
     [ -e "$path" ] && mv -- "$path" "runtime-archive/$TS/"
   done
   ```

**Cold-start launch:**

```bash
mkdir -p "$BRIDGE_CONFIG_DIR/state" "$BRIDGE_CONFIG_DIR/proofs" "$BRIDGE_CONFIG_DIR/work_dir" logs
if compgen -G "$BRIDGE_CONFIG_DIR/state/*.json" >/dev/null; then
  echo "Refusing cold start: archive existing state JSON first" >&2
  exit 1
fi

if [ -f relayer-state.json ] && \
   [ "$(jq -r '.last_observed_on_chain.last_seen_block_seq_no' relayer-state.json)" != "$BRIDGE_BOOTSTRAP_SEQNO" ]; then
  mv relayer-state.json "relayer-state.pre_deploy_${BRIDGE_ADDRESS: -4}_$(date +%s).json"
fi

TS=$(date +%Y%m%d_%H%M%S)
nohup ./target/release/relayer --state "$BRIDGE_CONFIG_DIR/relayer-state.json" daemon-live \
  > logs/live_cold_${BRIDGE_CONFIG_DIR##*/}_${TS}.log 2>&1 &
echo "PID=$!"
```

**Expected log signature (first 30s):**

```
INFO bridge_prover_lib::bk_set_bootstrap:  BK-set bootstrap: mode=file, loaded 5 signers from ./bk_set.shellnet.json
INFO bridge_prover_lib::keys::primary:     loaded primary VK from cache
INFO bridge_prover_lib::keys::fallback:    loaded fallback VK from cache
INFO bridge_prover_lib::keys::layer:       loaded layer VK from cache
INFO bridge_prover_lib::keys::event:       loaded event VK from cache
INFO bridge_prover_lib::keys::bk_update:   loaded BkSetUpdateChecker VK from cache
INFO bridge_prover_lib::bk_set_bootstrap:  chain-config check OK: ./bk_set.shellnet.json matches prover_bk_set.commitment
INFO relayer: LiveProverDriver seed policy seed_policy=Explicit(<BRIDGE_BOOTSTRAP_SEQNO>)
INFO bridge_prover_lib::live_driver: live_driver: bootstrap seed applied — seq_no=<N>, height=<N>, layers=<1 for L1 | 2 for L2>
```

`seed_policy=Explicit(N)` means the daemon is seeding from
`BRIDGE_BOOTSTRAP_SEQNO`. `seed_policy=Resume` would mean it found
existing `$BRIDGE_CONFIG_DIR/state/prover_state.json` and is resuming
— wrong for cold start.

**Cold-start seed timing.** The daemon does **not** compare
`BRIDGE_BOOTSTRAP_SEQNO` against shellnet head at startup — only
stride-alignment (L1: `%1024`, L2: `%16384`).

- `BRIDGE_BOOTSTRAP_SEQNO ≤ shellnet head` (normal): seed block fetched
  immediately; first `verifyBlock confirmed` follows in ~15 min
  worst-case (L1) or ~56 min average, up to ~101 min worst-case (L2).
- `BRIDGE_BOOTSTRAP_SEQNO > shellnet head` (footgun): daemon enters an
  **indefinite polling loop**. Kill, redeploy with `--at-head`.

**Expected startup arm: `Cold`.** Fresh contracts + no local state ⇒
`startup_decide::decide()` picks the **Cold** arm. If instead you see
`Resurrect` or `WarmResume`, you are not in Case 1 — go to
[Case 6a](#case-6a--chain-resurrect-advanced-contract--fresh-daemon).

**First cycle timing** —
- **L1** — first `verifyBlock confirmed` in **~15 min**.
- **L2** — first `verifyBlock confirmed` in **~56 min avg, up to ~101
  min**. The seed is written into the contract as the genesis anchor
  and trusted as-is — **no ZK proof is computed for the bootstrap
  seq_no**. The first proven bundle is the *next* W²-aligned boundary.

**Startup drift check (cross-level guard).** The daemon refuses to
boot if `prover_state.anchor_level != BRIDGE_ANCHOR_LEVEL`. If
switching a running config from L1 to L2 (or the reverse), rename the
offending state dir first:

```bash
mv "$BRIDGE_CONFIG_DIR/state" "$BRIDGE_CONFIG_DIR/state.pre_L${OLD_LEVEL}_$(date +%s)"
mkdir -p "$BRIDGE_CONFIG_DIR/state"
```

### Recovery: `startup on-chain drift vs last_observed_on_chain` (first launch after redeploy)

**When it hits.** First `daemon-live` launch against a fresh
`BRIDGE_ADDRESS` (or any time the on-chain contract's `storedLastSeen…`
/ `storedBkSetCommitment` / `storedLastBkSetUpdateSeqNo` does not
match the local `relayer-state.json.last_observed_on_chain`).

**Error signature:**

```
ERROR relayer: daemon-live failed
   e=startup on-chain drift vs last_observed_on_chain:
     startup_on_chain_drift:
       expected=BridgeOnChainState { last_seen_block_seq_no: <OLD>, ..., last_bk_set_update_seq_no: <OLD_BK>, ... }
       actual  =BridgeOnChainState { last_seen_block_seq_no: <NEW>, ..., last_bk_set_update_seq_no: <NEW_BK>, ... }
     — operator must reconcile
```

**Fix — snapshot stale local file, then relaunch:**

```bash
cd crates/bridge-prover-libraries

# 1. Confirm the drift
L_SEEN=$(jq -r '.last_observed_on_chain.last_seen_block_seq_no' relayer-state.json)
L_BKN=$(jq -r '.last_observed_on_chain.last_bk_set_update_seq_no' relayer-state.json)
C_SEEN=$(cast call $BRIDGE_ADDRESS 'storedLastSeenBlockSeqNo()(uint64)'   --rpc-url $RPC_URL --json | jq -r '.[0]')
C_BKN=$(cast  call $BRIDGE_ADDRESS 'storedLastBkSetUpdateSeqNo()(uint64)' --rpc-url $RPC_URL --json | jq -r '.[0]')
echo "seen: local=$L_SEEN chain=$C_SEEN   bkN: local=$L_BKN chain=$C_BKN"

# 2. Snapshot
mv relayer-state.json "relayer-state.pre_deploy_${BRIDGE_ADDRESS: -4}_$(date +%s).json"

# 3. Relaunch
TS=$(date +%Y%m%d_%H%M%S)
nohup ./target/release/relayer daemon-live \
  > logs/live_cold_${BRIDGE_CONFIG_DIR##*/}_${TS}.log 2>&1 &
echo "PID=$!"
```

**Do not** hand-edit `relayer-state.json` to silence the guard — that
leaves attempt counters / bk-update bookkeeping from the previous
deploy in place.

---

## Case 2 — Steady-state operation

Once bootstrapped, cadence is **per bundle**, not per key-block:

- **L1** — bundle every `W·P = 1024` seq_nos (~5 min chain-time).
  Prover is the limiter (~10–14 min per bundle), effective cadence
  **~13 min per bundle**.
- **L2** — bundle every `W² = 16384` seq_nos (~91 min chain-time).
  Chain is the limiter; effective cadence **~91 min per bundle**.

Overlapping with that, independently and asynchronously, the Phase 1
rotation lane fires whenever shellnet publishes a new BK-set update
AND the previous rotation (if any) is already covered by `verifyBlock`
(see [Case 8](#case-8--observed-bk-set-rotation-routine-no-action-required)
for the signatures and the deferred-ack mechanics).

**Where things get written per successful cycle:**

| Path | Written by | Trigger |
|---|---|---|
| `submissions/verifyBlock_seq<N>_fin<T>_<ts>.json` | `bridge.rs:679-724` (env-gated by `BRIDGE_DUMP_SUBMISSIONS_DIR`) | Every `verifyBlock` submit attempt |
| `submissions/applyBkSetUpdate_seq<N>_<ts>.json` | same path, same env gate | Every `applyBkSetUpdate` submit attempt |
| `$BRIDGE_CONFIG_DIR/relayer-state.json` | `relayer.rs` | Every on-chain observation cycle (~every block) |
| `$BRIDGE_CONFIG_DIR/state/prover_state.json` + `$BRIDGE_CONFIG_DIR/state/prover_bk_set.json` | `live_source.rs:141-157` (`persist_driver`) | Every successful `ack_last_bundle` or `ack_bk_update` after on-chain confirmation |

**Steady-state watch commands:**

```bash
# Live daemon log
tail -f crates/bridge-prover-libraries/logs/live_*.log

# Latest submissions (both lanes)
watch -n 30 'ls -lt crates/bridge-prover-libraries/submissions/ | head -10'

# On-chain progress (both counters)
watch -n 60 "cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)'   --rpc-url $RPC; \
             cast call $BRIDGE 'storedLastBkSetUpdateSeqNo()(uint64)' --rpc-url $RPC"
```

**Progress signature (per successful `verifyBlock` cycle):**

```
INFO bridge_prover_lib::live_driver::bundle: key block <N>: Circuit 2 proof generated in <ms>
INFO bridge_relayer_daemon::aggregated_source: aggregated_source: wrap+aggregate attestation + layer
INFO bridge_relayer_daemon::bridge: dumped verifyBlock submission to submissions/verifyBlock_seq<N>_fin0_<ts>.json
INFO bridge_relayer_daemon::relayer: verifyBlock confirmed  seq_no=<N>  block=<sepolia_block>
```

**Progress signature (per successful `applyBkSetUpdate` cycle):**

```
INFO bridge_prover_lib::live_driver::bk_update: BkSetUpdateChecker proof generated in <ms>
INFO bridge_relayer_daemon::aggregated_source: aggregated_source: wrap+aggregate bk_update
INFO bridge_relayer_daemon::bridge: dumped applyBkSetUpdate submission to submissions/applyBkSetUpdate_seq<N>_<ts>.json
INFO bridge_relayer_daemon::relayer: bk-set update applied seq_no=<N> tx=<hash>
```

Absence of the last line for >5 min after a `dumped ...` line =
transport / receipt lag — go to
[Case 4](#case-4--restart-after-rpc-induced-hard-abort).

---

## Case 3 — Clean restart (no state loss)

**When to use.** Daemon is running, no failures — you just want to
restart (e.g. after `git pull` + rebuild).

```bash
cd crates/bridge-prover-libraries

# 1. Send SIGTERM, wait for graceful exit
kill $(pgrep -f 'relayer .*daemon-live')
sleep 5
pkill -9 -f 'aggregate-proof' 2>/dev/null   # clean up any orphan child

# 2. Rebuild if needed
cargo build --release -p bridge-relayer-daemon --bin relayer

# 3. Relaunch (state files preserved → file-first guard triggers Resume)
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
TS=$(date +%Y%m%d_%H%M%S)
nohup ./target/release/relayer --state "$BRIDGE_CONFIG_DIR/relayer-state.json" daemon-live \
  > logs/live_restart_${BRIDGE_CONFIG_DIR##*/}_${TS}.log 2>&1 &
```

**Verify Resume:** log must contain `LiveProverDriver seed policy
seed_policy=Resume`, NOT `Explicit(...)`. If you see `Explicit(...)`
after a restart, `$BRIDGE_CONFIG_DIR/state/prover_state.json` is
missing — go to [Case 6a](#case-6a--chain-resurrect-advanced-contract--fresh-daemon).

Also expect, within the first minute, a log line resembling
`startup: BK-set state read from prover_bk_set.json n=<N> commit=…`,
confirming the daemon picked up the correct BK-set snapshot (not the
bootstrap file).

---

## Case 4 — Restart after RPC-induced hard-abort

**Symptoms.** Daemon exited by itself. Last log lines:

```
WARN bridge_relayer_daemon::relayer: bridge reverted attempts=3
   reason=verifyBlock send failed: error sending request for url (...)
   OR
   reason=applyBkSetUpdate send failed: error sending request for url (...)
   OR
   reason=tx confirmation error: transaction was not confirmed within the timeout
ERROR bridge_relayer_daemon::daemon: daemon: stuck — hard aborting
   error=Stuck { seq_no: <N>, attempts: 3, reason: "..." }
Error: relayer stuck on seqNo=<N> after 3 rejects: ...
```

The hard-abort logic currently counts **transport failures as
rejections** and kills the daemon after N=3 attempts in either lane
(`attempts_since_progress` for `verifyBlock`,
`bk_update_attempts_since_progress` for `applyBkSetUpdate`). This is a
known false-positive for transport-only failures.

**Recovery — always run these three checks before relaunch:**

### 4a. Verify no txs actually landed (nonce check)

```bash
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
RELAYER_ADDR=$(cast wallet address --private-key $RELAYER_PRIVATE_KEY)
cast nonce $RELAYER_ADDR --rpc-url $RPC_URL
cast nonce $RELAYER_ADDR --rpc-url $RPC_URL --block pending
```

If `latest == pending`, no txs in mempool. If `pending > latest`, wait
1–2 min for pending to clear before restart (otherwise nonce collision).

### 4b. Drift check — local state vs on-chain (both lanes)

```bash
cd crates/bridge-prover-libraries

L_SEQ=$(jq -r '.last_observed_on_chain.last_seen_block_seq_no'      "$BRIDGE_CONFIG_DIR/relayer-state.json")
L_BKN=$(jq -r '.last_observed_on_chain.last_bk_set_update_seq_no'   "$BRIDGE_CONFIG_DIR/relayer-state.json")
L_BK=$(python3 -c 'import sys; print(f"0x{int(sys.argv[1], 0):064x}")' \
  "$(jq -r '.last_observed_on_chain.bk_set_commitment'              "$BRIDGE_CONFIG_DIR/relayer-state.json")")
L_PBK=$(python3 -c 'import sys; print(f"0x{int(sys.argv[1], 0):064x}")' \
  "$(jq -r '.last_observed_on_chain.prev_bk_set_commitment'         "$BRIDGE_CONFIG_DIR/relayer-state.json")")
L_PREV=$(python3 -c 'import sys; print(f"0x{int(sys.argv[1], 0):064x}")' \
  "$(jq -r '.last_observed_on_chain.prev_max_level_layer_hash'      "$BRIDGE_CONFIG_DIR/relayer-state.json")")

C_SEQ=$(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)'    --rpc-url $RPC --json | jq -r '.[0]')
C_BKN=$(cast call $BRIDGE 'storedLastBkSetUpdateSeqNo()(uint64)'  --rpc-url $RPC --json | jq -r '.[0]')
C_BK=0x$(python3 -c "print(f'{$(cast call $BRIDGE storedBkSetCommitment\(\)\(uint256\)        --rpc-url $RPC --json | jq -r '.[0]'):064x}')")
C_PBK=0x$(python3 -c "print(f'{$(cast call $BRIDGE storedPrevBkSetCommitment\(\)\(uint256\)   --rpc-url $RPC --json | jq -r '.[0]'):064x}')")
C_PREV=0x$(python3 -c "print(f'{$(cast call $BRIDGE storedPrevMaxLevelLayerHash\(\)\(uint256\) --rpc-url $RPC --json | jq -r '.[0]'):064x}')")

[ "$L_SEQ"  = "$C_SEQ" ]  && echo "seq OK"    || echo "SEQ DRIFT    local=$L_SEQ  chain=$C_SEQ"
[ "$L_BKN"  = "$C_BKN" ]  && echo "bkN OK"    || echo "BKN DRIFT    local=$L_BKN  chain=$C_BKN"
[ "$L_BK"   = "$C_BK" ]   && echo "bk OK"     || echo "BK DRIFT     local=$L_BK   chain=$C_BK"
[ "$L_PBK"  = "$C_PBK" ]  && echo "prev-bk OK"|| echo "PREV-BK DRIFT local=$L_PBK chain=$C_PBK"
[ "$L_PREV" = "$C_PREV" ] && echo "prev OK"   || echo "PREV DRIFT   local=$L_PREV chain=$C_PREV"
```

- **Zero drift → proceed to 4c.**
- **Any drift → STOP.** Investigate what advanced on-chain (another
  relayer? direct `applyBkSetUpdate` from a maintainer?) before
  touching anything. Never restart through drift — you'll instantly
  hit `PrevAnchorMismatch` / `BkSetCommitmentMismatch` /
  `BlockSeqNoNotMonotonic` / `BkUpdateSeqNoNotMonotonic` and the abort
  loop kicks in again.

### 4c. Reset both attempt counters

```bash
jq '.last_attempt_seqno = .last_processed_seqno
  | .attempts_since_progress = 0
  | .bk_update_attempts_since_progress = 0' \
  "$BRIDGE_CONFIG_DIR/relayer-state.json" > "$BRIDGE_CONFIG_DIR/relayer-state.json.tmp" \
  && mv "$BRIDGE_CONFIG_DIR/relayer-state.json.tmp" "$BRIDGE_CONFIG_DIR/relayer-state.json"
jq . "$BRIDGE_CONFIG_DIR/relayer-state.json"
```

Without this, the daemon inherits a nonzero counter from disk and
hard-aborts on the next transport blip in either lane.

### 4d. Relaunch

Same as [Case 3](#case-3--clean-restart-no-state-loss). Expect
`seed_policy=Resume`. First cycle regenerates the pending proof from
scratch (prior proof was in RAM, lost on exit) — ~13 min to next
`verifyBlock` submit; a pending rotation re-runs Circuit 3 (~3–5 min
at K=21) before the next `applyBkSetUpdate` submit.

**If it dies again from RPC:** switch `RPC_URL` to a stable
authenticated endpoint and repeat the nonce/cursor/state checks.

---

## Case 5 — Restart after on-chain revert

**Symptoms.** Daemon exited via hard-abort, log shows:

```
WARN bridge_relayer_daemon::relayer: bridge reverted attempts=3
   reason=verifyBlock send failed: server returned an error response:
   error code 3: execution reverted, data: "0x<selector>"
   -- OR applyBkSetUpdate send failed with the same shape --
```

**Decode the revert selector.** Any of these is a real proof/state
problem, not a transport blip:

| Selector | Error | Lane | Root cause pattern |
|---|---|---|---|
| `0x87bf1c06` | `AttestationProofRejected()` | verifyBlock | Adapter equality check on a public input failed. **Bug class: BN254 Fr canonicalization** — if this fires on `blockId`, the client fix in `bridge-relayer-daemon/src/types.rs:83` (`U256::from_be_bytes(b.block_id_be) % BN254_FR_MODULUS`) is missing/reverted. |
| `0x...PrevAnchorMismatch` | `PrevAnchorMismatch(supplied, stored)` | verifyBlock | Local prev-anchor state diverged from on-chain `expectedPrevAnchor(numLayers)`. |
| `0x...BkSetCommitmentMismatch` | `BkSetCommitmentMismatch(supplied, stored)` | verifyBlock | Proof baked against the wrong BK-set slot. Common causes: (a) `prover_bk_set.json` stale after a rotation we missed; (b) prover was acked onto the incoming set too early (deferred-ack bug); (c) another actor applied a rotation we don't know about. See [Case 10](#case-10--bk_setshellnetjson-has-gone-stale-after-the-first-rotation). |
| `0x...BlockSeqNoNotMonotonic` | `BlockSeqNoNotMonotonic(supplied, stored)` | verifyBlock | Submitting `seq_no ≤ storedLastSeenBlockSeqNo`. Almost always: state loss + wrong `BRIDGE_BOOTSTRAP_SEQNO`. |
| `0x...BkUpdateSeqNoNotMonotonic` | `BkUpdateSeqNoNotMonotonic(supplied, stored)` | applyBkSetUpdate | Submitting a rotation at `block_seq_no ≤ storedLastBkSetUpdateSeqNo`. Means another actor applied this rotation already, or the off-chain `BkUpdateSource` returned a stale `block_seq_no`. |
| `0x...BkUpdatePrevUncovered` (name may differ — contract line AckiNackiBridge.sol:979-981) | `BkUpdate second rotation while previous uncovered` | applyBkSetUpdate | Attempt to apply a second rotation while `storedLastBkSetUpdateSeqNo > storedLastSeenBlockSeqNo`. **Should never leave Phase 1** — the relayer defers in-process (see [Case 9](#case-9--phase-1-stuck-two-rotations-with-no-bundle-target-between-them)). If this fires on-chain, another actor is bypassing the deferral. |
| `0x...BkUpdateProofRejected` | adapter equality check failed on Circuit 3 PI | applyBkSetUpdate | Proof baked against wrong `old_commitment` / `new_commitment` / `block_seq_no`. Compare the dumped submission to the chain state. |

Decode via `cast 4byte $SELECTOR` or dry-run the failing submission:

```bash
# verifyBlock dry-run
LATEST=$(ls -t crates/bridge-prover-libraries/submissions/verifyBlock_seq*.json | head -1)
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

# applyBkSetUpdate dry-run (one Circuit 3 proof + 4 public inputs)
LATEST=$(ls -t crates/bridge-prover-libraries/submissions/applyBkSetUpdate_seq*.json | head -1)
cast call $BRIDGE \
  "applyBkSetUpdate(bytes,uint256,uint256,uint64)" \
  $(jq -r '.bk_update_proof_hex'       "$LATEST") \
  $(jq -r '.old_commitment_uint256'    "$LATEST") \
  $(jq -r '.new_commitment_uint256'    "$LATEST") \
  $(jq -r '.block_seq_no'              "$LATEST") \
  --rpc-url $RPC
```

**Do NOT** just reset the counter and restart — the proof itself is
broken. Fix the root cause first, then follow [Case 4c → 4d](#4c-reset-both-attempt-counters).

---

## Case 6a — Chain-resurrect (advanced contract + fresh daemon)

**When it fires.** Any startup where the on-chain contract has history
(`storedLastSeenBlockSeqNo > 0` **or**
`storedLastBkSetUpdateSeqNo > 0`) but the local
`$BRIDGE_CONFIG_DIR/state/prover_state.json` is either absent,
`initialized=false`, or has `stored_last_seen_block_seq_no` /
`stored_last_bk_set_update_seq_no` strictly less than chain. This is
the operating case for fresh checkouts on a new machine that need to
catch up with an already-advanced deploy — **especially after
rotations have started firing on shellnet**, where this becomes the
canonical way to resynchronize without depending on the possibly-stale
`bk_set.shellnet.json`.

**No manual bootstrap needed** — the daemon does it automatically. On
startup:

1. Reads the full on-chain state via `getLayerWindow(1..=10)` +
   `storedLastSeenBlockSeqNo` + `storedBkSetCommitment` +
   `storedPrevBkSetCommitment` + `storedLastBkSetUpdateSeqNo`.
2. Compares against local state via `startup_decide::decide()`.
3. On the `Resurrect` arm: rebuilds `BridgeState` byte-for-byte from
   the contract snapshot via `BridgeState::from_contract`, atomically
   persists it to `$BRIDGE_CONFIG_DIR/state/prover_state.json`, then
   drives `LiveProverDriver` with `SeedPolicy::Resume`.
   `BRIDGE_BOOTSTRAP_SEQNO` is **ignored**.
4. The BK-set snapshot (`prover_bk_set.json`) is reconstructed from the
   on-chain commitment: the daemon queries the AN node's `/v2/bk_set`
   endpoint (served by any AN node on port 8600; **note:
   `shellnet.ackinacki.org` returns 404 for this path** — point at a
   node you control, or seed from `$BRIDGE_BK_SET_CONFIG` and validate
   its commitment against `storedBkSetCommitment` before accepting).

**What the operator does.**

```bash
cd crates/bridge-prover-libraries
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
TS=$(date +%Y%m%d_%H%M%S)
nohup ./target/release/relayer --state "$BRIDGE_CONFIG_DIR/relayer-state.json" daemon-live \
  > logs/live_${BRIDGE_CONFIG_DIR##*/}_${TS}.log 2>&1 &
```

**Expected log signature:**

```
INFO relayer: startup: read on-chain state for routing chain_last_seen=... chain_last_bk=... local_last_seen=0 local_initialized=false
INFO relayer: startup: Resurrect — rebuilding BridgeState from on-chain snapshot chain_last_seen=... chain_last_bk=...
INFO relayer: LiveProverDriver seed policy seed_policy=Resume
```

**When the daemon refuses to auto-resurrect (`Stop` arm):**

- `local_last_seen > chain_last_seen` — local state ahead of contract;
  investigate.
- `local_last_bk_set_update_seq_no > chain_last_bk_set_update_seq_no`
  — same, in the BK-set lane.
- Cursors match but `bk_set_commitment`, `prev_bk_set_commitment` or
  `layer_windows` diverge from chain — state file from a different
  contract.
- `HISTORY_WINDOW_SIZE` mismatch between daemon binary and contract W.
- `state.anchor_level != BRIDGE_ANCHOR_LEVEL`.
- L1 daemon against contract with `_layerWindows[L>=2].data_len > 0`.

If a `Stop` bail was legitimate, proceed to
[Case 6b](#case-6b--state-loss--re-bootstrap-from-mid-chain).

---

## Case 6b — State loss / re-bootstrap from mid-chain

**When to use.** `$BRIDGE_CONFIG_DIR/state/prover_state.json`
deleted / corrupted, OR contract redeployed at a different
`storedLastSeenBlockSeqNo`, AND
[Case 6a](#case-6a--chain-resurrect-advanced-contract--fresh-daemon)
auto-resurrect refused (`Stop` arm) with a reason you understand and
accept.

**This is destructive.** Only proceed if you've confirmed the contract
is at a known seed and no in-flight state is worth preserving.

```bash
cd crates/bridge-prover-libraries

# 1. Read chain state (both lanes)
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
CURRENT_SEEN=$(cast call $BRIDGE_ADDRESS 'storedLastSeenBlockSeqNo()(uint64)'   --rpc-url $RPC_URL --json | jq -r '.[0]')
CURRENT_BKN=$(cast  call $BRIDGE_ADDRESS 'storedLastBkSetUpdateSeqNo()(uint64)' --rpc-url $RPC_URL --json | jq -r '.[0]')
echo "chain last_seen = $CURRENT_SEEN   last_bk = $CURRENT_BKN"

# 2. Update BRIDGE_BOOTSTRAP_SEQNO in $BRIDGE_CONFIG_DIR/env to match
#    (must equal $CURRENT_SEEN and be aligned to the level's stride)
grep '^BRIDGE_BOOTSTRAP_SEQNO=' "$BRIDGE_CONFIG_DIR/env"

# 3. Archive existing state
TS=$(date +%Y%m%d_%H%M%S)
[ -d "$BRIDGE_CONFIG_DIR/state" ] && mv "$BRIDGE_CONFIG_DIR/state" "$BRIDGE_CONFIG_DIR/state.stale_${TS}"
[ -f "$BRIDGE_CONFIG_DIR/relayer-state.json" ] && \
  mv "$BRIDGE_CONFIG_DIR/relayer-state.json" "$BRIDGE_CONFIG_DIR/relayer-state.json.stale_${TS}"
mkdir -p "$BRIDGE_CONFIG_DIR/state"

# 4. Regenerate genesis anchors from the seed block
cd ../bridge-prover-lib
cargo run --release --bin compute_bridge_anchors -- \
  --seed-seqno $CURRENT_SEEN \
  --level $BRIDGE_ANCHOR_LEVEL \
  --gql-endpoint https://shellnet.ackinacki.org/graphql
# Compare its output to `expectedPrevAnchor(level)` and
# `storedBkSetCommitment()` on-chain. All three must match.
# If `storedBkSetCommitment` on-chain != GENESIS_BK_SET_COMMITMENT in
# the output, the chain's BK set has rotated since; you MUST re-seed
# $BRIDGE_BK_SET_CONFIG from a node that reflects the live set
# (`curl http://<an-node>:8600/v2/bk_set | jq`) — see Case 10.
cd ../bridge-prover-libraries

# 5. Cold-start launch (identical to Case 1)
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
nohup ./target/release/relayer --state "$BRIDGE_CONFIG_DIR/relayer-state.json" daemon-live \
  > logs/live_rebootstrap_${BRIDGE_CONFIG_DIR##*/}_${TS}.log 2>&1 &
```

**Expect `seed_policy=Explicit($CURRENT_SEEN)`** in the log. Not
`Resume`.

**Why re-bootstrap is dangerous.** If `BRIDGE_BOOTSTRAP_SEQNO` doesn't
match `storedLastSeenBlockSeqNo` on-chain, the first submit reverts
with `BlockSeqNoNotMonotonic` or `PrevAnchorMismatch`. And if the BK
set file doesn't match `storedBkSetCommitment`, the first `verifyBlock`
reverts with `BkSetCommitmentMismatch` even if the layer-chain is
sound.

---

## Case 7 — L2-anchored cold-start (`BRIDGE_CONFIG_DIR=./L2_config`)

Merged into [Case 1](#case-1--first-time-bootstrap-from-a-fresh-deploy)
— the three L2 deltas (`W²=16384`-aligned bootstrap seqno, up to ~101
min first-verify wait, `layers=2` startup-log field) are covered
in-line there. Cases 2–6 are anchor-level opaque; substitute
`./L1_config` ↔ `./L2_config` in any command block. BK-set rotation is
anchor-level opaque: `applyBkSetUpdate` cadence depends only on the
chain's rotation schedule, not on `BRIDGE_ANCHOR_LEVEL`.

For the L2 E2E withdrawal (burn → `withdraw-e2e` submit), see the
withdraw runbook.

---

## Case 8 — Observed BK-set rotation (routine, no action required)

**When it fires.** Shellnet has `minBK=4` set on
`BlockKeeperContractRoot` and completes an epoch — a BK leaves, a
candidate (or no one) takes its slot. The node publishes a BK-set
update over its REST + GQL surfaces; the daemon's `BkUpdateSource`
picks it up on the next tick.

**No operator action.** This case documents what the daemon does
unaided, so you can tell "routine rotation in progress" apart from
"stuck rotation" ([Case 9](#case-9--phase-1-stuck-two-rotations-with-no-bundle-target-between-them))
and "stale BK-set file"
([Case 10](#case-10--bk_setshellnetjson-has-gone-stale-after-the-first-rotation)).

**Phase 1 + Phase 2 interleaving (relayer `tick`).** Per
`crates/bridge-relayer-daemon/src/relayer.rs:204-302`:

1. **Pre-check.** `bk_target = on_chain.last_bk_set_update_seq_no + 1`.
   Ask `BkUpdateSource::fetch_bk_update(bk_target)` whether a candidate
   rotation exists at or after that seq_no.
2. **Deferral guard.** If `on_chain.last_bk_set_update_seq_no >
   on_chain.last_seen_block_seq_no` (previous rotation not yet covered
   by `verifyBlock`), the daemon computes the next bundle target
   `next = next_bundle_boundary(last_seen, bundle_stride)`:
   - `upd.block_seq_no >= next` → **log + defer** (`"deferring
     applyBkSetUpdate until verifyBlock covers the previous rotation"`),
     fall through to Phase 2.
   - `upd.block_seq_no < next` → **Stuck** (see
     [Case 9](#case-9--phase-1-stuck-two-rotations-with-no-bundle-target-between-them)).
3. **Cold-start sanity.** `upd.old_commitment_l2 ==
   on_chain.bk_set_commitment` is checked locally. Mismatch → emit
   `TickOutcome::BkUpdateReverted` without broadcasting; bump
   `bk_update_attempts_since_progress`.
4. **Submit.** `bridge.submit_bk_set_update(&upd)`. On
   `Applied { new_state, tx_hash }`:
   - Call `prover_rotation_may_ack(&new_state, bundle_stride)`.
     If true (next bundle target would be `> N`), ack the live driver
     immediately via `ack_last_bk_update(upd.block_seq_no)` and run the
     after-ack consistency check.
     If false, **defer** the ack: keep the prover on the outgoing set
     so Phase 2 can still prove blocks `<= N` against
     `storedPrevBkSetCommitment`.
   - Persist `relayer-state.json` with
     `record_bk_update_progress(upd.block_seq_no)` and the fresh
     `BridgeOnChainState`.
5. **Fall through to Phase 2** (`verifyBlock` lane): the deferred-ack
   mechanics mean the prover continues to produce proofs against the
   outgoing BK set as long as `target_bundle_seqno <= N`, then
   switches to the incoming set.

**Expected log signature (routine rotation, both lanes shown):**

```
INFO relayer: Phase 1: fetched bk_update bk_target=<N+1> upd_block_seq_no=<N'>
INFO relayer: submit_bk_set_update dispatched block_seq_no=<N'>
INFO relayer: dumped applyBkSetUpdate submission to submissions/applyBkSetUpdate_seq<N'>_<ts>.json
INFO relayer: bk-set update applied seq_no=<N'> tx=0x…
INFO relayer: deferring prover ack until next bundle target passes <N'>
...
INFO relayer: Phase 2: fetching block target=<seqno>
INFO bridge_prover_lib::live_driver: proving against outgoing BK set (seq_no=<T> <= N=<N'>, using storedPrevBkSetCommitment)
INFO relayer: verifyBlock confirmed seq_no=<T> block=<sepolia_block>
...
(when next bundle target > N)
INFO relayer: prover rotation ack: next bundle target <T'> > N <N'>, acking BK update
INFO bridge_prover_lib::live_driver: switched to incoming BK set after ack
```

**What you check in the Quick resume view.** After a routine rotation
has fully landed:

- `chain.storedLastBkSetUpdateSeqNo = N'` (new value)
- `local.last_observed_on_chain.last_bk_set_update_seq_no = N'`
- `chain.storedPrevBkSetCommitment` = the pre-rotation commitment
- `chain.storedBkSetCommitment` = the post-rotation commitment
- `$BRIDGE_CONFIG_DIR/state/prover_bk_set.json` mtime refreshed
- `bk_update_attempts_since_progress = 0`

If `last_bk_set_update_seq_no` on chain is non-zero but on the local
side the daemon has not seen it (local counter lags by more than one
rotation cycle) for more than ~1 tick duration, suspect
[Case 10](#case-10--bk_setshellnetjson-has-gone-stale-after-the-first-rotation).

**Observability shortcut.** You can also watch the Phase 1 lane
directly from Prometheus if `RELAYER_METRICS_ADDR` is set:

```
curl -s http://localhost:9464/metrics | grep -E '^(bk_update_|bridge_last_bk|bridge_bk_)' | head
```

Expected counters move: `bk_update_submits_total`,
`bk_update_confirms_total`, `bk_update_deferred_total`,
`bridge_last_bk_set_update_seq_no` (gauge).

---

## Case 9 — Phase 1 Stuck: two rotations with no bundle target between them

**Symptoms.** Daemon exits via hard-abort immediately after a Phase 1
pre-check — before any submit — with:

```
ERROR bridge_relayer_daemon::daemon: daemon: stuck — hard aborting
   error=Stuck {
     seq_no: <N2>,
     attempts: 0,
     reason: "two rotations with no bundle target in [<N1>, <N2>]: next target <next> is after N2; verifyBlock cannot move and this stall is permanent"
   }
```

Here `N1 = storedLastBkSetUpdateSeqNo` on chain (the already-applied
but still-uncovered rotation) and `N2 = upd.block_seq_no` from
`BkUpdateSource` (the next pending rotation). The relayer computed
`next_bundle_boundary(last_seen, bundle_stride) = next`, found that
`next > N2`, and concluded that `verifyBlock` has no way to advance
past `N1` before Phase 1 is forced to apply a second (not permitted)
rotation.

**Why it's "permanent".** The ETH-36 two-slot model only holds one
outgoing commitment. Applying `N2` while `N1 > last_seen` would delete
the pre-`N1` commitment that `verifyBlock` still needs to verify
blocks in `(last_seen, N1]`. The relayer refuses rather than causing
an on-chain `BkUpdatePrevUncovered` revert later.

**Can this happen on shellnet?** Only if the chain publishes two
rotations within the same bundle stride **and** `last_seen` is
positioned such that neither bundle target lands in `[N1, N2]`. With
`BUNDLE_STRIDE_L1 = 1024` (~5 min chain-time) and the shellnet rotation
cadence of one per epoch (660 s baseline + cliff/wait + candidate
availability), this requires either:
- A very narrow bundle alignment window where `next < N1` and `next +
  stride > N2`, i.e. one bundle target just before `N1` and the next
  jumping over both rotations, **or**
- The daemon falling behind the chain so far that two rotations have
  already piled up by the time it looks (restart after long outage).

L2 (`stride = 16384`) is more vulnerable to this because the stride is
wider than any realistic rotation gap.

**Recovery.**

1. **Confirm the stall is real.**
   ```bash
   N1=$(cast call $BRIDGE 'storedLastBkSetUpdateSeqNo()(uint64)' --rpc-url $RPC --json | jq -r '.[0]')
   LAST_SEEN=$(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC --json | jq -r '.[0]')
   # The upd.block_seq_no (N2) comes from the daemon log — grep for `Stuck`.
   echo "N1=$N1  last_seen=$LAST_SEEN  stride=$BUNDLE_STRIDE"
   # If LAST_SEEN >= N1, the deferral is no longer needed; the stall
   # has cleared itself — go to step 4 directly.
   ```
2. **Wait for `verifyBlock` to advance `last_seen >= N1`.** The
   relayer is already producing a `verifyBlock` proof covering the
   next bundle target `next`. Confirm by `tail -f logs/live_*.log` and
   watch for `verifyBlock confirmed seq_no=<T>` with `T >= N1`.
3. **Once covered, the "permanent" classification is reversed** — the
   daemon can now apply `N2` normally. The hard-abort just forced
   operator attention.
4. **Reset Phase 1 counters and relaunch** (identical to
   [Case 4c + 4d](#4c-reset-both-attempt-counters)).

**Prevention.**

- Keep `last_seen` within one bundle of chain head. A long outage that
  misses two rotations is the easiest way to construct this stall.
- If you deliberately stop the daemon through a rotation (e.g. for a
  rebuild), prefer stopping it *after* `last_seen >= N1` so no
  `N2`-pile-up can race the restart.
- Do **not** reduce `BUNDLE_STRIDE` to avoid this — stride is a
  circuit parameter.

---

## Case 10 — `bk_set.shellnet.json` has gone stale after the first rotation

**Symptoms.** One of:

- Fresh clone attempting to cold-start a daemon against a shellnet
  that has already rotated at least once. On launch, the chain-config
  sanity check fails:
  ```
  ERROR bridge_prover_lib::bk_set_bootstrap: chain-config check FAILED:
    ./bk_set.shellnet.json commitment 0x08eb0a… != on-chain
    storedBkSetCommitment 0xabcd12…
  Error: bootstrap BK-set commitment mismatch — refuse to start
  ```
- OR an existing daemon that lost its `prover_bk_set.json` falls back
  to the bootstrap file and immediately hits
  `BkSetCommitmentMismatch` on the first `verifyBlock` after resume.

**Root cause.** `bk_set.shellnet.json` is a **snapshot** of the
shellnet BK set at the time the file was committed. Shellnet with
`minBK=4` rotates after each completed epoch (~660 s + cliff/wait).
Once the first rotation lands, the file's commitment no longer matches
the chain's `storedBkSetCommitment`. The daemon's cold-start path
refuses to proceed because any proof baked against the stale set would
be rejected on-chain.

Note: when the daemon runs in `Resurrect` mode and successfully reads
the live BK set from the AN node's `/v2/bk_set` REST endpoint, this
case does not fire — `prover_bk_set.json` is rebuilt from chain. This
case is specifically about the `Cold` arm where the file is still
authoritative.

**Fix path 1 — prefer `Resurrect` instead of `Cold`.** If any
`$BRIDGE_CONFIG_DIR/state/prover_state.json` from a previous session
is available, let the daemon rebuild from chain:

```bash
# Confirm resurrectable: chain non-genesis AND you have a reachable
# AN node REST endpoint (NOT the public GQL — it returns 404 for /v2/*).
cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)'   --rpc-url $RPC   # > 0
cast call $BRIDGE 'storedLastBkSetUpdateSeqNo()(uint64)' --rpc-url $RPC   # > 0
curl -s http://<an-node-host>:8600/v2/bk_set | jq '.pub_keys | length'  # ~5
```

Then follow [Case 6a](#case-6a--chain-resurrect-advanced-contract--fresh-daemon)
— no edits to `bk_set.shellnet.json` required.

**Fix path 2 — refresh `bk_set.shellnet.json` from a live AN node.**
Only if you must cold-start, or you need the committed file to reflect
the current chain:

```bash
# 1. Pull the live BK set from an AN node you control (NOT the public
#    GQL endpoint).
AN_NODE=http://<an-node-host>:8600
curl -s "$AN_NODE/v2/bk_set" | jq . > /tmp/bk_set.live.json

# 2. Convert the REST payload to the file format the prover expects
#    (same JSON shape as bk_set.shellnet.json — array of entries, each
#    with `pub_key` BLS G1 compressed hex). The exact conversion is a
#    one-line jq; the committed file is the ground-truth template:
jq '{bk_set: [.pub_keys[] | {pub_key: .}]}' /tmp/bk_set.live.json \
  > crates/bridge-prover-libraries/bk_set.shellnet.json.candidate

# 3. Verify the Poseidon commitment matches on-chain BEFORE moving the
#    file into place. The prover's `bk_set_bootstrap` dry-run tool is
#    the authoritative check:
cd crates/bridge-prover-libraries
cargo run --release -p bridge-prover-lib --bin verify_bk_set_commitment -- \
  --file bk_set.shellnet.json.candidate \
  --expected $(cast call $BRIDGE 'storedBkSetCommitment()(uint256)' --rpc-url $RPC --json | jq -r '.[0]')
# Must print: "OK: file commitment matches expected"
# If it prints "MISMATCH", the AN node is on a different fork or the
# conversion is wrong — do NOT replace the file.

# 4. Atomic swap + checksum update
mv bk_set.shellnet.json bk_set.shellnet.pre_rotation_$(date +%s).json
mv bk_set.shellnet.json.candidate bk_set.shellnet.json
sha256sum bk_set.shellnet.json   # record this value in private handoff
```

**Fix path 3 — operator policy.** On any shellnet instance that will
have `minBK=4` for an extended period, treat `bk_set.shellnet.json`
as a bootstrap-only artifact. Do not update it on each rotation; the
runtime authority is `prover_bk_set.json` in the state dir, which is
updated by every successful `applyBkSetUpdate` ACK. Only refresh the
committed file when you need to onboard a fresh clone to a long-lived
rotated chain, and record the SHA-256 in the private handoff each
time.

**What this is not.** This is not the same as a chain-side
inconsistency (`storedBkSetCommitment` does not match any legitimate
set). If `verify_bk_set_commitment` fails against every reasonable
source, suspect either (a) the AN node is on a different fork, or (b)
an unauthorized `applyBkSetUpdate` landed from a non-relayer account.
Both are incident-response territory; do not patch the file.

---

## Health checks (run any time)

**On-chain state snapshot (both lanes):**

```bash
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
export BRIDGE=$BRIDGE_ADDRESS
export RPC=$RPC_URL
echo "last_seen:         $(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)'      --rpc-url $RPC --json | jq -r '.[0]')"
echo "last_bk_update:    $(cast call $BRIDGE 'storedLastBkSetUpdateSeqNo()(uint64)'    --rpc-url $RPC --json | jq -r '.[0]')"
echo "bk_commitment:     $(cast call $BRIDGE 'storedBkSetCommitment()(uint256)'        --rpc-url $RPC)"
echo "bk_prev:           $(cast call $BRIDGE 'storedPrevBkSetCommitment()(uint256)'    --rpc-url $RPC)"
echo "latest_per_layer:  $(cast call $BRIDGE 'getLatestPerLayer()(uint256[10])'        --rpc-url $RPC)"
echo "expectedPrev($BRIDGE_ANCHOR_LEVEL): $(cast call $BRIDGE 'expectedPrevAnchor(uint8)(uint256)' $BRIDGE_ANCHOR_LEVEL --rpc-url $RPC)"
echo "storedPrev(genesis): $(cast call $BRIDGE 'storedPrevMaxLevelLayerHash()(uint256)' --rpc-url $RPC)"
# storedPrevMaxLevelLayerHash is `immutable` (constructor-set) — it
# holds the genesis seed forever, not the last-block max-level. The
# dynamic per-block anchor lives in _layerWindows and is folded via
# expectedPrevAnchor(numLayers) at submit time.
```

**BK-set snapshot (on-chain vs daemon vs file):**

```bash
# On-chain
C_BK=$(cast call $BRIDGE 'storedBkSetCommitment()(uint256)' --rpc-url $RPC)

# Daemon runtime (authoritative when the daemon has been through at
# least one ACK)
D_BK=$(jq -r '.commitment' "$BRIDGE_CONFIG_DIR/state/prover_bk_set.json" 2>/dev/null || echo MISSING)

# Bootstrap file (should match on-chain ONLY if no rotation has ever
# been applied)
F_BK=$(jq -r '.commitment' "$BRIDGE_BK_SET_CONFIG" 2>/dev/null || echo MISSING)

echo "chain:  $C_BK"
echo "state:  $D_BK   (authoritative at runtime)"
echo "file:   $F_BK   (bootstrap-only; stale after first rotation)"
```

Expected relationships after any rotation has fired:
- `chain == state` (daemon in sync)
- `file  != chain` is normal and NOT an error
- `chain != state` → daemon is lagging; see
  [Case 10](#case-10--bk_setshellnetjson-has-gone-stale-after-the-first-rotation)
  or [Case 5](#case-5--restart-after-on-chain-revert)

**Daemon liveness:**

```bash
pgrep -a -f 'relayer .*daemon-live'                # parent PID + cmdline
pgrep -a -f 'aggregate-proof'                      # active aggregator subprocess (should exist mid-cycle)
```

**Recent activity:**

```bash
# Last successful ack (mtime of prover_state.json / prover_bk_set.json)
stat -f "%Sm  %N" "crates/bridge-prover-libraries/$BRIDGE_CONFIG_DIR/state/prover_state.json"
stat -f "%Sm  %N" "crates/bridge-prover-libraries/$BRIDGE_CONFIG_DIR/state/prover_bk_set.json"

# Last submitted block (verifyBlock lane)
ls -t crates/bridge-prover-libraries/submissions/verifyBlock_seq*.json 2>/dev/null | head -1 \
  | xargs -I {} sh -c 'jq -r "\"verifyBlock seq_no=\" + (.block_seq_no|tostring)" {}'

# Last submitted BK update (Phase 1 lane)
ls -t crates/bridge-prover-libraries/submissions/applyBkSetUpdate_seq*.json 2>/dev/null | head -1 \
  | xargs -I {} sh -c 'jq -r "\"applyBkSetUpdate seq_no=\" + (.block_seq_no|tostring)" {}'

# Cadence — deltas between latest 5 submissions in each lane
ls -lt crates/bridge-prover-libraries/submissions/verifyBlock_seq*.json 2>/dev/null | head -5
ls -lt crates/bridge-prover-libraries/submissions/applyBkSetUpdate_seq*.json 2>/dev/null | head -5
```

**Wallet balance:**

```bash
RELAYER_ADDR=$(cast wallet address --private-key $RELAYER_PRIVATE_KEY)
cast balance $RELAYER_ADDR --rpc-url $RPC --ether
# verifyBlock ~0.001-0.003 ETH; applyBkSetUpdate ~similar.
# Refill if <0.5 ETH.
```

**Rotation cadence sanity (shellnet-side, needs AN node access):**

```bash
# Approximate — count rotations observed on-chain in the last hour
cast logs --address $BRIDGE --from-block $(($(cast block-number --rpc-url $RPC) - 300)) \
  'BkSetUpdateApplied(uint64,uint256,uint256)' --rpc-url $RPC | grep -c '^transactionHash' || true
# Expected on shellnet@minBK=4: ~5-6 per hour (one per epoch); 0 means
# either rotation is off (minBK=5) or the chain is paused.
```

---

## File & state reference

Everything lives under `crates/bridge-prover-libraries/` (the daemon's
working directory). The sibling `crates/bridge-relayer-daemon/`
directory is **source code only** — no runtime data lands there.

```
crates/bridge-prover-libraries/
├── shellnet.common                  ← tracked development fixture; not a server secret store
├── L1_config/                       ← L1-anchor mode config dir (BRIDGE_CONFIG_DIR=./L1_config)
│   ├── env                          ← sources ../shellnet.common + L1 overrides
│   ├── relayer-state.json           ← verifier cursor + on-chain observation cache (both lanes)
│   ├── state/
│   │   ├── prover_state.json        ← LiveProverDriver snapshot (~574 KB) — auth. resume point
│   │   └── prover_bk_set.json       ← active BK-set snapshot (authoritative after first ACK)
│   ├── proofs/                      ← per-mode SHPLONK-wrapped Circuit 4 output (event proofs)
│   └── work_dir/                    ← per-mode enriched witnesses + shplonk-snark scratch
├── L2_config/                       ← same layout as L1_config
├── bk_set.shellnet.json             ← bootstrap-only BK-set (5 signers at genesis); stale after first rotation
├── submissions/                     ← calldata dumps for verifyBlock AND applyBkSetUpdate
├── logs/                            ← daemon stdout+stderr
├── params/                          ← SRS + circuit VKs/PKs (~17 GB, DO NOT WIPE)
└── target/release/relayer           ← the binary
```

**Path resolution (`bridge-prover-lib::paths`):** the daemon reads
`BRIDGE_CONFIG_DIR` and derives `paths::state_dir()` →
`$BRIDGE_CONFIG_DIR/state`, `paths::proofs_dir()` →
`$BRIDGE_CONFIG_DIR/proofs`, `paths::bootstrap_seed_file()` →
`$BRIDGE_CONFIG_DIR/state/bootstrap_seed.json`.
`BRIDGE_STATE_DIR` / `BRIDGE_PROOFS_DIR` are honored as narrower
overrides.

**Persistence triggers (`crates/bridge-relayer-daemon/src/live_source.rs`):**

- `$BRIDGE_CONFIG_DIR/state/prover_state.json` +
  `$BRIDGE_CONFIG_DIR/state/prover_bk_set.json` are written together
  by `persist_driver` on every successful `ack_last_bundle` **or**
  `ack_last_bk_update`. Not on every submit — only on confirmed ACK.
  Note: because the Phase 1 ack is **deferred** when the prover is
  still on the outgoing BK set, `prover_bk_set.json` may lag
  `storedLastBkSetUpdateSeqNo` by up to one bundle stride.
- `$BRIDGE_CONFIG_DIR/relayer-state.json` is written on every on-chain
  observation refresh (~every block). Carries the full
  `BridgeOnChainState` including `last_bk_set_update_seq_no` and both
  BK-set commitments.

**BK-set lifecycle (two files, two roles):**

| File | Role | Written by | Read by |
|---|---|---|---|
| `bk_set.shellnet.json` (`BRIDGE_BK_SET_CONFIG`) | Bootstrap snapshot | **Operator** (committed in repo or captured from a live AN node — see Case 10) | Daemon on `Cold` arm only, with on-chain commitment check |
| `$BRIDGE_CONFIG_DIR/state/prover_bk_set.json` | Runtime authority | Daemon, on every ACK (both `ack_last_bundle` and `ack_last_bk_update` emit via `persist_driver`) | Daemon on `Resume` / `Resurrect` arms; never read by human tools |

**Cleanup rules:**

- `submissions/` — safe to prune anytime; purely diagnostic.
- `logs/` — safe to prune, but keep the most recent for post-mortem.
- `$BRIDGE_CONFIG_DIR/state/`, `$BRIDGE_CONFIG_DIR/relayer-state.json`
  — **NEVER** delete a running daemon's active state. To reset,
  archive to `$BRIDGE_CONFIG_DIR/state.stale_<ts>/` +
  `$BRIDGE_CONFIG_DIR/relayer-state.json.stale_<ts>` first (see
  [Case 6b](#case-6b--state-loss--re-bootstrap-from-mid-chain)).
- `bk_set.shellnet.json` — never delete; if it goes stale, follow
  [Case 10](#case-10--bk_setshellnetjson-has-gone-stale-after-the-first-rotation)
  (archive the old copy, don't overwrite in place).
- `$BRIDGE_CONFIG_DIR/work_dir/` — safe to prune between events.
- `params/` — never delete; keygen takes ~7 min per circuit.

---

## Onboarding wrap-up — new operator on a fresh L2 server

Written for a colleague picking up this runbook cold on a new n14-class
machine. Concrete task: **deploy a fresh bundle near chain head, run
the L2 daemon through Docker Compose, and leave a reproducible private
deployment record plus observable persistent state.**

Scope of *this* deployment:

- **Single relayer instance** — no parallel provers, no catch-up workers.
- **L2 anchor mode only** — `BRIDGE_CONFIG_DIR=./L2_config`, stride
  `W² = 16384`.
- **Shellnet → Sepolia only** — mainnet is not in scope.
- **BK-set rotation state at onboarding time: check first.** Query
  `cast call $BRIDGE 'storedLastBkSetUpdateSeqNo()(uint64)'`. If `> 0`,
  shellnet has `minBK=4` and rotations are live — prefer the
  `Resurrect` path over cold-starting with the committed bootstrap
  file (which may be stale, see
  [Case 10](#case-10--bk_setshellnetjson-has-gone-stale-after-the-first-rotation)).
  Record the on-chain `storedBkSetCommitment` and
  `storedLastBkSetUpdateSeqNo` in the private handoff.

### Do this, in order

1. **Pin + build** — check out one reviewed commit and build both
   release binaries with `--locked`. Record the commit and binary
   SHA-256 values.
2. **Wallet + secrets** — generate a dedicated Sepolia EOA, fund it
   and keep its key/RPC credentials only in root-owned env files
   outside the clone. Never reuse the tracked development burner.
3. **SRS + toolchain** — provision K=17,19,20,21,22, generate the
   inner PK/VK/config set (including the Circuit 3 BkSetUpdateChecker
   PK/VK), then seal and hash the static artifact manifest. Keep the
   outer `pk_cache` writable on a bind mount.
4. **BK-set bootstrap decision.** Before deploy:
   - `storedLastBkSetUpdateSeqNo == 0` on the shellnet (either `minBK=5`
     or `minBK=4` but no rotation has landed yet) → commit the
     `bk_set.shellnet.json` from a live AN node snapshot taken within
     the last epoch; record its SHA-256.
   - `storedLastBkSetUpdateSeqNo > 0` → still capture a current
     snapshot (for emergencies), but plan to onboard via `Resurrect`
     after the first run acknowledges a rotation.
5. **Deploy the bundle** — load deployment values from the external
   env and run the helper once from `crates/bridge-prover-libraries/`:
   ```bash
   CONFIRM_NEW_BRIDGE_DEPLOY=DEPLOY_NEW_CONTRACTS \
     LEVEL=2 PRIVATE_KEY=<burner> WITHDRAW_ACC_FR=<auth value> \
     ./scripts/deploy_bridge_bundle.sh
   ```
   Retain the full Foundry broadcast record and all
   constructor/anchor values. A retry deploys new addresses; audit
   nonce and receipts first.
6. **Preflight + cold start** — fill the external runtime env, then
   follow the [Compose kit](../deploy/shellnet-l2/README.md). Expect
   the first confirmation after the next W² boundary plus proof time:
   roughly 0–91 min chain wait plus about 10 min proving.
7. **Acceptance** — require:
   - a Sepolia receipt with `status=1` on the first `verifyBlock`,
   - equal local and on-chain cursors (both `last_seen` **and**
     `last_bk_set_update_seq_no`),
   - no pending nonce,
   - matching verifier bytecode,
   - a controlled stop/start cycle that logs `WarmResume` without a
     duplicate tx.
8. **Long-run watch** — run `scripts/status.sh` and retain submission
   JSONs. L2 normally advances once per 16,384 seq_nos (~91 min at the
   observed shellnet rate); `NotYetAvailable` between boundaries is
   healthy. If shellnet rotates during the run, expect
   `applyBkSetUpdate` cycles interleaved with `verifyBlock` cycles —
   this is routine ([Case 8](#case-8--observed-bk-set-rotation-routine-no-action-required)).

### Private handoff — what to record

Mandatory per deployment:

- Bridge address, every deployment tx/receipt.
- Genesis seed/height/anchor/BK commitment and initial local/on-chain
  cursors (both lanes).
- Current `storedBkSetCommitment` and `storedLastBkSetUpdateSeqNo` at
  handoff time (not just at deploy time — these may differ if any time
  has elapsed).
- SHA-256 of `bk_set.shellnet.json` and the date it was captured.
- The specific `BlockKeeperContractRoot` `minBK` policy for this
  shellnet instance (4 → rotations on; 5 → rotations off), and when it
  was last changed.

n14 sizing / core scaling: n14 is adequate for the measured L2 cadence.
Stages run sequentially, but individual Halo2 stages are multi-core;
extra cores improve those stages rather than multiplying independent
bundles. Live acceptance peaked near 23 GiB RAM (`verifyBlock` lane)
and produced an outer cache around 17 GiB. Add ~2–3 GiB headroom for
the Circuit 3 wrap+aggregate path when the BK-set lane fires in
parallel with layer-hashes proving.
