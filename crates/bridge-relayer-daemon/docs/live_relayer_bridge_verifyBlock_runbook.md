# Live `verifyBlock` E2E Runbook — shellnet → Sepolia (bundle-only, Circuits 1A/B + 2)

Operational guide for running the `bridge-relayer-daemon daemon-live` binary
against a deployed `AckiNackiBridge` on Sepolia, driven by the shellnet
GraphQL endpoint. Covers first-time bootstrap, steady-state operation, and
recovery from the failure modes we have actually hit in production.

**Aim of this runbook.** Stand up and keep running the relayer daemon so
the on-chain bridge state (its global history — anchor chain, per-layer
history window, latest verified bundle) advances continuously in step with the
Acki Nacki chain. The daemon polls shellnet, proves key blocks with some specified cadence (not each key block!), and
posts `verifyBlock` to Sepolia; success here means the bridge's stored
history stays live and never falls behind. 

**Scope of this runbook.** Bundle-only path: Circuit 1A/1B (attestation) +
Circuit 2 (layer hashes), aggregated by SHPLONK, submitted via
`verifyBlock`. For now here **we do not deal with** BK-set updates. We
assume testing on shellnet, where by default the BK set is fixed from
genesis.

Recall that **Shellnet was restarted at this commit cf664666badf2f12bf0ecc20846ac14b8bcb4e9d**.

Live BK set (5 signers, fixed from genesis) is committed at
[`crates/an-bridge-prover/bk_set.shellnet.json`](../../an-bridge-prover/bk_set.shellnet.json).

- Poseidon commitment (matches on-chain `storedBkSetCommitment()` on any live
  shellnet bridge deploy, and the `GENESIS_BK_SET_COMMITMENT` line emitted by
  `compute_bridge_anchors --at-head`):
  `0x08eb0a1892e4f75a8b5c8cff69322f95bf0437c371903998c9365fbe293ca71c`
- SHA-256 of `bk_set.shellnet.json` (tamper-detection):
  `c77e3d6de5e6ea8ee96c6902f1b6ecb011bba2d631e76fa546e44ee67173898f`

Event-level withdrawal proofs (Circuit 4 / `withdrawByProof`) are **out of scope** — that path is
covered in [`live_withdrawByProof_runbook.md`](live_withdrawByProof_runbook.md).

> **Notation.** `seq_no` is the Acki Nacki block sequence number.
> A **key block** is a block at height `seq_no`, where `seq_no % W == 0` (producer-side,
> `W = 128` -- historical window size). Key block carries out essential AN historical data that will serve as an anchor in Ethereum bridge contract. A **bundle** is what the daemon actually proves; its
> stride depends on anchor mode — `W·P = 1024` under L1 (thinning `P = 4`),
> `W² = 16384` under L2 (no thinning).

**Why we do not prove every key block.** A single bundle proof
(Circuit 1A + Circuit 2, aggregated by SHPLONK) costs ~10 min
warm / ~14 min cold on dev hardware (see
[`daemon_live_performance.md`](daemon_live_performance.md)). Shellnet
emits a **key block** every `W` ( = 128) source blocks and runs fast — at the
observed ~3 b/s that is one key block every ~43 s — so a per-key-block
prover would fall ~15× behind the chain. Two levers stack to make the
system viable:

- **Thinning** (bridge-side, param `P`). Relay only every `P`-th key
  block, i.e. heights `SEQ_NO % (W*P) == 0`. Current setting
  `W = 128, P = 8` → bundle stride 1024 source blocks (~5 min
  chain-time). Even at `P = 8` the prover is still hot — thinning
  alone does not close the gap.
- **Anchor level** (on-chain, env `BRIDGE_ANCHOR_LEVEL`). Controls how
  often the bridge advances its covering layer-hash on Sepolia, and
  therefore how often a user can withdraw.
  - `L1` (stride `W·P = 1024`, ~5 min shellnet-time) — every bundle is
    an anchor. Convenient for one-fire-and-withdraw E2E tests and CI:
    a user's `withdrawByProof` becomes provable faster within one bundle cycle.
    Used for all dev/iteration work here. About 30-50 minutes per test.
  - `L2` (stride `W² = 16384`, ~91 min shellnet-time) — the production
    variant. Thinning does **not** apply here: the daemon proves one
    bundle per full `W²` window directly (single L2 hop, `chain_steps = 1`
    in Circuit 2), see `AnchorMode::stride` in
    `bridge-prover-lib/src/lib.rs`. A user waits up to ~2 h for their
    withdrawal to become provable — a *constant* cadence — in exchange
    for far fewer on-chain writes and lower gas.

Operational impact on `withdrawByProof` is detailed in
[`live_withdrawByProof_runbook.md`](live_withdrawByProof_runbook.md).

> **Runtime layout.** For a long-running L2 server, use the production
> [Docker Compose kit](../deploy/shellnet-l2/README.md): runtime state and
> heavyweight proving data stay in bind mounts, while the filled runtime env
> (including the signing key) stays outside Git. The direct-CLI sections below
> remain useful for development and recovery. Their in-repo `shellnet.common`
> and `L{1,2}_config/env` files are reproducible test fixtures, not a production
> secret/config store.

## Table of Contents

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
- [Case 7 — L2-anchored cold-start (`--anchor-level 2`)](#case-7--l2-anchored-cold-start--anchor-level-2)
- [Health checks (run any time)](#health-checks-run-any-time)
- [File & state reference](#file--state-reference)
- [Onboarding wrap-up — new operator on a fresh L2 server](#onboarding-wrap-up--new-operator-on-a-fresh-l2-server)

---

## Quick resume checklist (returning to a running system)

Run this **before touching anything** — it takes 30 seconds and tells you
exactly which case (below) applies.

```bash
cd crates/an-bridge-prover
export BRIDGE_CONFIG_DIR=./L1_config      # or ./L2_config — the mode this daemon runs in
export RELAYER_STATE_PATH="$BRIDGE_CONFIG_DIR/relayer-state.json"
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
export BRIDGE=$BRIDGE_ADDRESS
export RPC=$RPC_URL

# 1. Is the daemon alive?
pgrep -af 'relayer .*daemon-live' || echo "DAEMON NOT RUNNING"

# 2. Where is local state?
echo "local  last_processed = $(jq -r '.last_processed_seqno' "$RELAYER_STATE_PATH")"
echo "local  attempts       = $(jq -r '.attempts_since_progress' "$RELAYER_STATE_PATH")"
echo "local  observed_chain = $(jq -r '.last_observed_on_chain.last_seen_block_seq_no' "$RELAYER_STATE_PATH")"
echo "$BRIDGE_CONFIG_DIR/state/prover_state mtime: $(stat -c '%y' "$BRIDGE_CONFIG_DIR/state/prover_state.json" 2>/dev/null || echo MISSING)"

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
| running | yes | >0 | Usually healthy `NotYetAvailable` before the next boundary; confirm in logs |
| not running | yes | 0 | [Case 3](#case-3--clean-restart-no-state-loss) (clean restart) |
| not running | yes | >0 | Classify the final log/receipt first; [Case 4](#case-4--restart-after-rpc-induced-hard-abort) only for a real hard-abort |
| not running | **no** | any | [Case 5](#case-5--restart-after-on-chain-revert) or [Case 6b](#case-6b--state-loss--re-bootstrap-from-mid-chain) — do not restart blindly |
| running/not | `$BRIDGE_CONFIG_DIR/state/` missing | — | [Case 6a](#case-6a--chain-resurrect-advanced-contract--fresh-daemon) (auto-resurrect from chain) |

For a Compose deployment, use `sudo deploy/shellnet-l2/scripts/status.sh`
instead. It performs the same reconciliation and also reports container exit,
OOM/restart state, EOA nonce/balance, shellnet head and recent significant
logs without printing the private key.

## Production L2 server (Docker Compose)

The supported long-running server wrapper is
[`deploy/shellnet-l2/`](../deploy/shellnet-l2/README.md). It provides an
immutable non-root image, read-only preflight, bounded Docker logs, explicit
bind mounts, status tooling and example env files containing placeholders
only. The operator flow is:

1. deploy a fresh L2 bridge near shellnet head and retain its broadcast record;
2. provision and seal SRS/inner keys for K=17,19,20,21,22;
3. build target-host release binaries and the image from one pinned commit;
4. install the filled runtime env outside the repository with mode `0640`;
5. run `docker compose run --rm preflight`, then
   `docker compose up -d relayer`;
6. after the first confirmation, require local/on-chain cursor equality and
   perform one controlled stop/start to prove the `WarmResume` path.

`restart: unless-stopped` restores the service after an unexpected exit or a
Docker/host restart. The container reruns the full fail-closed preflight on
every start; a pending nonce, artifact drift or state/on-chain mismatch blocks
the daemon before it can send a transaction. Alert on repeated restarts and
stop the service for reconciliation after a persistent logical rejection.
Never start two daemons with the same bridge/EOA/state tuple. The remaining
sections document contract deployment, direct execution and recovery details
used by that wrapper.

## Binary + env prerequisites

Working directory: `crates/an-bridge-prover/`.

### Step 1 — Choose the anchor mode

`BRIDGE_CONFIG_DIR` is the single mode selector. Pick **one** of the
two lines below and export it in the shell you will use for all
subsequent commands:

```bash
cd crates/an-bridge-prover

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

`daemon-live` does **not** auto-download SRS on first launch — if
any required SRS is missing, proving or outer aggregation will fail. The full
live bundle path needs K=17,19,20,21,22; K=22 is specifically required by the
layer outer aggregator.

The `bootstrap_hermez_srs` binary lives in this same repo
(`gosh-sh/bridge`) under `crates/an-bridge-prover/bridge-prover-lib/src/bin/`.
Run both commands from `crates/an-bridge-prover/`:

```bash
# 3a. Manually fetch K=21 (~2.4 GB) and K=22 (~4.8 GB). They are not
#     auto-downloaded because the shared K=20 trust anchor cannot vouch for
#     either blob.
mkdir -p ~/.cache/halo2-kzg-srs
curl -L --fail --progress-bar \
  https://storage.googleapis.com/zkevm/ptau/powersOfTau28_hez_final_21.ptau \
  -o ~/.cache/halo2-kzg-srs/powersOfTau28_hez_final_21.ptau
curl -L --fail --progress-bar \
  https://storage.googleapis.com/zkevm/ptau/powersOfTau28_hez_final_22.ptau \
  -o ~/.cache/halo2-kzg-srs/powersOfTau28_hez_final_22.ptau

# 3b. Build + run. Auto-fetches the K=20 ptau (~1.2 GB) on cache miss,
#     then materializes all five SRS files (~960 MiB total).
cargo build --release -p bridge-prover-lib --bin bootstrap_hermez_srs
./target/release/bootstrap_hermez_srs \
  --k 17 --k 19 --k 20 --k 21 --k 22
```

Passing any `--k` replaces the program's default set, so list all five values;
`--k 22` alone would provision only K=22. Pin and verify the resulting artifact
manifest before starting a production container.

### Step 4 — Source the mode env file

```bash
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
```

The development env file first sources `../shellnet.common` for shared test
settings and then sets the three mode-specific vars
(`BRIDGE_ADDRESS`, `BRIDGE_BOOTSTRAP_SEQNO`, `BRIDGE_ANCHOR_LEVEL`).
`BRIDGE_CONFIG_DIR` stays as you exported it in Step 1.

For a server, do not edit either tracked file. Install a dedicated runtime env
outside the clone from
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

All ten variables must print `ok`. `BRIDGE_BOOTSTRAP_SEQNO` must equal
the contract's `storedLastSeenBlockSeqNo` at construction — verify with
`cast call $BRIDGE_ADDRESS 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC_URL`.

### Step 6 — Launch the daemon

```bash
./target/release/relayer --state "$RELAYER_STATE_PATH" daemon-live
```

All `daemon-live` CLI flags are exposed as `BRIDGE_*` env vars, so no
subcommand arguments are needed. The global `--state` is explicit so L1 and
L2 never share a relayer cursor. For a long-running server, use the Compose
section above rather than `nohup`.

---


## Deploy your own bridge bundle from scratch

**You may not need this section.** The repo ships pre-populated development
`shellnet.common`, `L1_config/env`, and `L2_config/env` pointing at a
live Sepolia deploy — each `L{1,2}_config/env` carries the on-chain
`BRIDGE_ADDRESS`, `BRIDGE_BOOTSTRAP_SEQNO`, and `BRIDGE_ANCHOR_LEVEL`;
`shellnet.common` carries the RPC + GQL endpoints and the in-repo paths
for the aggregator crate and Solidity verifier sources
(`BRIDGE_AGGREGATOR_DIR`, `BRIDGE_VERIFIERS_DIR` — same relative paths
in every clone, not deploy-specific). They may be used for disposable local
tests. A server operator must instead use a dedicated EOA and an external
runtime env, then skip to [Case 1](#case-1--first-time-bootstrap-from-a-fresh-deploy) — the
daemon can `Resurrect` off the on-chain state (see
[Case 6a](#case-6a--chain-resurrect-advanced-contract--fresh-daemon))
regardless of who deployed it.

Deploy your own bundle only if the shared deploy no longer suits (e.g.
different anchor level, custom W/P, isolated test lane, or you want to
own the key that owns the contract). `verifyBlock`
(`AckiNackiBridge.sol:650`) is permissionless — anyone with a valid
proof and a funded burner can submit against any deployed bridge; the
daemon reads its signer from `RELAYER_PRIVATE_KEY`
(`bridge-relayer-daemon/src/bin/relayer.rs:86`), no default.

**Network.** Target is Sepolia (chain 11155111). Point `RPC_URL` at a stable,
authenticated Sepolia RPC endpoint; public endpoints are suitable for smoke
tests but not unattended operation. The daemon does not
read a chain-id from env — the ethers provider fetches it via
`eth_chainId` on connect and every signed tx pins it there.

The four steps below take you from an empty machine to a wired-up
`L{1,2}_config/env`.

### 1. Create a fresh burner wallet

```bash
cast wallet new
#  Address:     0x...
#  Private key: 0x...
```

Keep the private key in a local file **outside** the repo. Never reuse a
wallet that holds real funds.

### 2. Fund it with Sepolia ETH

Two faucets that reliably deliver **without** an anti-Sybil mainnet-deposit
gate (verified 2026-08):

- **pk910 PoW** — https://sepolia-faucet.pk910.de/  (mine in-browser, ~5–15 min for the target amount)
- **Google Cloud Web3 faucet** — https://cloud.google.com/application/web3/faucet/ethereum/sepolia  (0.05 ETH/day, no PoW)

Faucets that require depositing ≥0.001 ETH on mainnet first (Alchemy /
Infura / QuickNode) are usable once you're funded; they hand out
0.05–0.5 ETH/day and are the practical top-up path after bootstrap.

**Budget.** The one-shot deploy creates six logical components
(`AckiNackiBridge`, four verifier lanes and `MockBlockHeaderOracle`) through
14 physical `CREATE` transactions (the four lanes each include adapter,
wrapper and Yul verifier). It cost **0.063 ETH** on 2026-08-13 (30M gas @
2.1 gwei). Add a running budget of ~0.001–0.003 ETH per `verifyBlock`
submit (one per bundle stride — 1024 blocks in L1 mode, 16384 in L2).
**Target ≥ 0.1 ETH before deploy**, ≥ 0.5 ETH for a multi-day E2E run.
The [Health checks](#health-checks-run-any-time) block includes a
wallet-balance line — refill from either faucet when it drops below
0.5 ETH.

### 3. Deploy + wire — one end-to-end script

> **The one thing to know.** `GENESIS_LAST_SEEN_BLOCK_SEQNO` is **not** a
> value you pick by hand. It is derived by
> **[`compute_bridge_anchors --at-head`](../../an-bridge-prover/bridge-prover-lib/src/bin/compute_bridge_anchors.rs)**,
> a Rust binary that:
>
> 1. Queries shellnet chain head over GraphQL (`query_latest_blocks(1)`,
>    endpoint from `$BRIDGE_GQL_ENDPOINT`) — no separate `curl` needed.
> 2. Picks the newest stride-aligned boundary ≤ head
>    (L1: `⌊head / 1024⌋ · 1024`; L2: `⌊head / 16384⌋ · 16384`).
> 3. Fetches that seed block and derives the on-chain anchor from its
>    `layer_hashes[level]`.
> 4. Prints five `KEY=value` lines to stdout, ready to `source`:
>    `GENESIS_BK_SET_COMMITMENT`, `GENESIS_PREV_MAX_LEVEL_LAYER_HASH`,
>    `GENESIS_SEED_SEQNO`, `GENESIS_SEED_HEIGHT`, `GENESIS_ANCHOR_LEVEL`.
>
> ⚠️ **Name mismatch to be aware of.** The tool emits `GENESIS_SEED_SEQNO`.
> The Solidity deploy script (`DeployShellnetE2EBridge.s.sol:138`) reads
> `GENESIS_LAST_SEEN_BLOCK_SEQNO` via `vm.envOr(..., 0)` — **silently
> defaulting to `0` if the var is missing**, which pins
> `storedLastSeenBlockSeqNo=0` into the constructor and permanently
> bricks the deploy. The script below aliases them explicitly. Do not
> skip that line.

The development helper below ships in-repo at
[`crates/an-bridge-prover/scripts/deploy_bridge_bundle.sh`](../../an-bridge-prover/scripts/deploy_bridge_bundle.sh)
and deploys a new bundle from scratch. This is an irreversible broadcast, not
an idempotent operation: every invocation spends a new nonce and deploys new
addresses. It also rewrites the selected development env and clears its local
state. Never blindly retry after a timeout or partial broadcast; first inspect
the Foundry broadcast JSON, account `latest`/`pending` nonces and receipts.
Archive any existing state and broadcast record before running it.

Load `PRIVATE_KEY`, `SEPOLIA_RPC_URL` and the withdrawal identity from a
root/operator-owned env outside the clone, then run:

```bash
cd crates/an-bridge-prover
CONFIRM_NEW_BRIDGE_DEPLOY=DEPLOY_NEW_CONTRACTS \
  LEVEL=1 PRIVATE_KEY=<sepolia burner from §1> ./scripts/deploy_bridge_bundle.sh
# or LEVEL=2 for L2 anchoring
```

`WITHDRAW_ACC_FR` identifies the expected Acki Nacki bridge account in
Circuit 4. The helper requires it explicitly. A deliberate dummy value is
acceptable only for a `verifyBlock`-only smoke deployment; it permanently
prevents meaningful withdrawal proofs. Use the authoritative account field
value for any bridge that will serve withdrawals and record all constructor
values in the private deployment handoff.

The essential script mechanics are shown below for review; the linked on-disk
copy is authoritative. Re-read the warning above before any rerun.

```bash
#!/usr/bin/env bash
# Deploy or redeploy the shellnet E2E bridge bundle end-to-end.
# NOT IDEMPOTENT: every run broadcasts new contracts and spends new nonces.
#
# Usage:
#   LEVEL=1 ./scripts/deploy_bridge_bundle.sh           # L1 anchor (stride  1024)
#   LEVEL=2 ./scripts/deploy_bridge_bundle.sh           # L2 anchor (stride 16384)
#
# Inputs expected in env (or in-line here — safer via env):
#   PRIVATE_KEY                Sepolia burner from §1 (fund via §2)
#   SEPOLIA_RPC_URL            default: https://ethereum-sepolia-rpc.publicnode.com
#   BRIDGE_GQL_ENDPOINT        default: https://shellnet.ackinacki.org/graphql
#   BRIDGE_BK_SET_CONFIG       default: ./bk_set.shellnet.json (relative to
#                              crates/an-bridge-prover)
set -euo pipefail
set +x
umask 077

: "${LEVEL:?set LEVEL=1 or LEVEL=2}"
: "${PRIVATE_KEY:?set PRIVATE_KEY=<sepolia burner>}"
: "${WITHDRAW_ACC_FR:?set authoritative WITHDRAW_ACC_FR (or an explicit dummy for verifyBlock-only smoke)}"
[[ "${CONFIRM_NEW_BRIDGE_DEPLOY:-}" == DEPLOY_NEW_CONTRACTS ]] || {
  echo "Refusing broadcast: set CONFIRM_NEW_BRIDGE_DEPLOY=DEPLOY_NEW_CONTRACTS" >&2
  exit 2
}
[[ "$LEVEL" == 1 || "$LEVEL" == 2 ]] || {
  echo "LEVEL must be 1 or 2" >&2
  exit 2
}
SEPOLIA_RPC_URL="${SEPOLIA_RPC_URL:-https://ethereum-sepolia-rpc.publicnode.com}"
BRIDGE_GQL_ENDPOINT="${BRIDGE_GQL_ENDPOINT:-https://shellnet.ackinacki.org/graphql}"
BRIDGE_BK_SET_CONFIG="${BRIDGE_BK_SET_CONFIG:-./bk_set.shellnet.json}"

# Resolve repo-root regardless of where the script sits.
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
PROVER_DIR="$REPO_ROOT/crates/an-bridge-prover"
CONTRACTS_DIR="$REPO_ROOT/contracts/ethereum"
GENESIS_ENV="$(mktemp -t genesis.XXXXXX.env)"
trap 'rm -f "$GENESIS_ENV"' EXIT

# ─── 1. Derive genesis anchors from live shellnet chain head ──────────────────
echo ">>> Computing genesis anchors (level=$LEVEL) against $BRIDGE_GQL_ENDPOINT"
cd "$PROVER_DIR/bridge-prover-lib"
BRIDGE_GQL_ENDPOINT="$BRIDGE_GQL_ENDPOINT" \
BRIDGE_BK_SET_CONFIG="$PROVER_DIR/${BRIDGE_BK_SET_CONFIG#./}" \
cargo run --release --quiet --bin compute_bridge_anchors -- \
  --at-head --level "$LEVEL" \
  > "$GENESIS_ENV"

echo ">>> Derived anchors:"
cat "$GENESIS_ENV"
set -a && source "$GENESIS_ENV" && set +a

# CRITICAL RENAME: compute_bridge_anchors emits GENESIS_SEED_SEQNO;
# DeployShellnetE2EBridge.s.sol reads GENESIS_LAST_SEEN_BLOCK_SEQNO via
# vm.envOr(..., 0). Without this alias the constructor silently pins 0.
export GENESIS_LAST_SEEN_BLOCK_SEQNO="$GENESIS_SEED_SEQNO"

# ─── 2. Deploy the bundle (14 CREATEs across 6 logical components) ────────────
cd "$CONTRACTS_DIR"
export PRIVATE_KEY
export WIRE_WITHDRAW_BY_PROOF=true
# Caller supplied this explicitly; use a dummy only for verifyBlock-only smoke.
export WITHDRAW_ACC_FR

echo ">>> forge script DeployShellnetE2EBridge (chain 11155111)"
forge script script/DeployShellnetE2EBridge.s.sol:DeployShellnetE2EBridge \
  --rpc-url "$SEPOLIA_RPC_URL" --broadcast --slow

# ─── 3. Extract BRIDGE_ADDRESS from the broadcast record ──────────────────────
BROADCAST="$CONTRACTS_DIR/broadcast/DeployShellnetE2EBridge.s.sol/11155111/run-latest.json"
BRIDGE_ADDRESS="$(jq -r '
  .transactions[]
  | select(.contractName=="AckiNackiBridge" and .transactionType=="CREATE")
  | .contractAddress
' "$BROADCAST" | tail -n 1)"
: "${BRIDGE_ADDRESS:?failed to extract AckiNackiBridge address from $BROADCAST}"
echo ">>> Deployed BRIDGE_ADDRESS = $BRIDGE_ADDRESS"

# ─── 4. Rewrite non-secret L${LEVEL}_config/env ───────────────────────────────
CFG_DIR="$PROVER_DIR/L${LEVEL}_config"
mkdir -p "$CFG_DIR/state" "$CFG_DIR/proofs" "$CFG_DIR/work_dir"

cat > "$CFG_DIR/env" <<EOF
# Generated by scripts/deploy_bridge_bundle.sh — do not hand-edit
# BRIDGE_BOOTSTRAP_SEQNO; re-run the script to regenerate anchors.
source "\$(dirname "\${BASH_SOURCE[0]}")/../shellnet.common"

BRIDGE_ADDRESS=$BRIDGE_ADDRESS
BRIDGE_BOOTSTRAP_SEQNO=$GENESIS_SEED_SEQNO
BRIDGE_ANCHOR_LEVEL=$LEVEL
EOF
echo ">>> Wrote $CFG_DIR/env"

# ─── 5. Archive stale state so the new bridge cold-start remains recoverable ─
STATE_ARCHIVE="$CFG_DIR/state.pre_deploy_$(date -u +%Y%m%dT%H%M%SZ)"
if compgen -G "$CFG_DIR/state/*.json" >/dev/null; then
  mkdir -p "$STATE_ARCHIVE"
  mv -- "$CFG_DIR/state"/*.json "$STATE_ARCHIVE/"
  echo ">>> Archived prior state in $STATE_ARCHIVE"
else
  echo ">>> No prior state JSON to archive"
fi

echo
echo ">>> Done. Next:"
echo "    cd $PROVER_DIR"
echo "    export BRIDGE_CONFIG_DIR=./L${LEVEL}_config"
echo "    set -a && source \"\$BRIDGE_CONFIG_DIR/env\" && set +a"
echo "    ./target/release/relayer --state \"\$BRIDGE_CONFIG_DIR/relayer-state.json\" daemon-live"
```

**Why `--at-head` matters (danger).** The daemon's cold-start policy
(`SeedPolicy::Explicit(N)`) does **no** chain-head comparison at
startup — only the alignment check
(`live_driver::LiveProverDriver::new`, live_driver/mod.rs:634-645). If
you hand-pick a boundary *ahead* of chain head, the daemon does not
error: it enters an **indefinite polling loop**, emitting
`Bootstrapping { seed_seqno=N, chain_head_seqno=<current head> }` on
every tick until chain catches up
(`live_driver::advance_bootstrap`, mod.rs:926-986). No timeout.
`--at-head` guarantees `N ≤ head` so the seed block is already
finalized and bootstrap proceeds immediately.

Now jump to [Binary + env prerequisites](#binary--env-prerequisites)
and continue with [Case 1](#case-1--first-time-bootstrap-from-a-fresh-deploy).

---

## Case 1 — First-time bootstrap from a fresh deploy

**When to use.** Contract just deployed; no `$BRIDGE_CONFIG_DIR/state/prover_state.json`
yet (where `$BRIDGE_CONFIG_DIR` is either `./L1_config` or `./L2_config`,
chosen in [Prereqs Step 1](#step-1--choose-the-anchor-mode)). One case,
one code path — L2 differs only in bootstrap seqno alignment (`W²` vs
`W·P`), covering-bundle wait time, and one log field.

**Prerequisites.** Complete [Binary + env prerequisites](#binary--env-prerequisites)
Steps 1–5 first (`BRIDGE_CONFIG_DIR` export + build + SRS + source env
+ sanity check). All commands below assume `$BRIDGE_CONFIG_DIR` is
exported and `$BRIDGE_ADDRESS`, `$RPC_URL`, etc. are populated.
L1 uses `W·P=1024`-block stride (~5.7 min per covering bundle);
L2 uses `W²=16384` (~91 min per covering bundle).

**Pre-flight (contract sanity).** Level-opaque — the same three storage
slots are verified regardless of `$BRIDGE_CONFIG_DIR`. `storedPrevMaxLevelLayerHash`
holds whichever fold you baked into the deploy (`compute_bridge_anchors
--level 1` or `--level 2`).

```bash
export BRIDGE=$BRIDGE_ADDRESS
export RPC=$RPC_URL

cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)'      --rpc-url $RPC   # == $BRIDGE_BOOTSTRAP_SEQNO
cast call $BRIDGE 'expectedPrevAnchor(uint8)(uint256)' $BRIDGE_ANCHOR_LEVEL --rpc-url $RPC   # matches GENESIS_PREV_MAX_LEVEL_LAYER_HASH
cast call $BRIDGE 'storedBkSetCommitment()(uint256)'        --rpc-url $RPC   # matches GENESIS_BK_SET_COMMITMENT
```

If any of these don't match your deploy's genesis values (from the
`compute_bridge_anchors` output you pinned at deploy time), **stop** —
the deploy is broken. Do not launch the daemon.

**Archive stale artifacts (cold-start hygiene).** The development helper moves
existing `state/*.json` into a timestamped `state.pre_deploy_*` directory;
still archive the complete prior config before deploying against a new bridge.
Older event proofs, aggregation
scratch and submission dumps are harmless to the daemon but valuable for
nonce/receipt forensics. Prefer a timestamped move over deletion:

```bash
TS=$(date +%Y%m%d_%H%M%S)
mkdir -p "runtime-archive/$TS"
for path in "$BRIDGE_CONFIG_DIR"/state.pre_deploy*_* \
            "$BRIDGE_CONFIG_DIR"/proofs.pre_deploy*_* \
            "$BRIDGE_CONFIG_DIR"/work_dir submissions; do
  [ -e "$path" ] && mv -- "$path" "runtime-archive/$TS/"
done
```

Skip all cleanup while handling Cases 4/5/6b. Never remove the only copy of a
state file or broadcast record during an incident.

**Cold-start launch:**

```bash
mkdir -p "$BRIDGE_CONFIG_DIR/state" "$BRIDGE_CONFIG_DIR/proofs" "$BRIDGE_CONFIG_DIR/work_dir" logs
if compgen -G "$BRIDGE_CONFIG_DIR/state/*.json" >/dev/null; then
  echo "Refusing cold start: archive existing state JSON first" >&2
  exit 1
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
INFO bridge_prover_lib::bk_set_bootstrap:  chain-config check OK: ./bk_set.shellnet.json matches prover_bk_set.commitment
INFO relayer: LiveProverDriver seed policy seed_policy=Explicit(<BRIDGE_BOOTSTRAP_SEQNO>)   ← cold-start signature
INFO bridge_prover_lib::live_driver: live_driver: bootstrap seed applied — seq_no=<N>, height=<N>, layers=<1 for L1 | 2 for L2>
```

`seed_policy=Explicit(N)` means the daemon is seeding from
`BRIDGE_BOOTSTRAP_SEQNO`. `seed_policy=Resume` would mean it found existing
`$BRIDGE_CONFIG_DIR/state/prover_state.json` and is resuming — wrong for cold start.
The `layers=` field is the anchor-level ground truth: `layers=1` for
`./L1_config`, `layers=2` for `./L2_config`. If they disagree with
`$BRIDGE_CONFIG_DIR`,
either the env didn't reach the process or you have stale state from a
prior run at the wrong level (see startup drift check below).

**Cold-start seed timing.** The daemon does **not** compare
`BRIDGE_BOOTSTRAP_SEQNO` against shellnet head at startup — only
stride-alignment (L1: `%1024`, L2: `%16384`). Two runtime outcomes:

- **`BRIDGE_BOOTSTRAP_SEQNO ≤ shellnet head`** (normal case — always
  true when you took the value from
  `compute_bridge_anchors --at-head`): the daemon fetches the seed
  block from GQL immediately and the `bootstrap seed applied` line
  appears within seconds. First `verifyBlock confirmed` follows in
  ~15 min worst-case (L1) or **~56 min average, up to ~101 min
  worst-case** (L2) — see **First cycle timing** below. The seed
  itself is *not* proven; the first ZK-proven bundle is the next
  stride-boundary above the seed, so the wait is (chain time to that
  boundary) + ~10 min prover.
- **`BRIDGE_BOOTSTRAP_SEQNO > shellnet head`** (footgun — you
  hand-picked a future boundary): the daemon enters an **indefinite
  polling loop** emitting `Bootstrapping { seed_seqno=N,
  chain_head_seqno=<current head> }` on every tick until head reaches
  N. There is no timeout. L1 head advances by ~1024 blocks every
  ~5 min; L2 boundaries land every ~91 min. If you deployed against
  a chain that will not grow to N, the daemon hangs forever. Kill it,
  redeploy with `compute_bridge_anchors --at-head` so `N ≤ head` by
  construction.

**Expected startup arm: `Cold`.** Fresh contracts + no local state ⇒
`startup_decide::decide()` picks the **Cold** arm and derives
`seed_policy = Explicit(BRIDGE_BOOTSTRAP_SEQNO)`. The log MUST show:

```
INFO relayer: startup: Cold — contract at genesis, bootstrapping
INFO relayer: LiveProverDriver seed policy seed_policy=Explicit(<BOOTSTRAP_SEQNO>)
```

If instead you see `startup: Resurrect …` or `startup: WarmResume …`,
you are not in Case 1 — the contract already has history or the state
dir wasn't wiped. Jump to
[Case 6a](#case-6a--chain-resurrect-advanced-contract--fresh-daemon)
for the full startup-routing truth table and the auto-resurrect path.

**First cycle timing** — GQL fetch of seed block → real-chain-builder
Merkle root → Circuit 2 proof (~2 min) → aggregation (~7 min at `layer PK`
load) → submit → wait for Sepolia confirmation → `ack_last_bundle`
→ `$BRIDGE_CONFIG_DIR/state/prover_state.json` written for the first time.

- **L1 (`BRIDGE_CONFIG_DIR=./L1_config`)** — first `verifyBlock confirmed` in **~15 min**
  (covering bundle lands at seed + `W·P = 1024`).
- **L2 (`BRIDGE_CONFIG_DIR=./L2_config`)** — first `verifyBlock confirmed` in
  **~56 min average, up to ~101 min worst-case**. The seed is written into
  the contract as the genesis anchor (`GENESIS_LAST_SEEN_BLOCK_SEQNO`) and
  trusted as-is — **no ZK proof is computed for the bootstrap seq_no**. The
  first proven bundle is the *next* W²-aligned boundary above the seed
  (`(k+1)·W²`), which by construction sits above chain head, so time-to-
  first-verify = (chain time to reach that boundary, uniform in
  `[0, W²/rate)` ≈ 0–91 min at ~3 b/s) + ~10 min prover. L2 has **no
  thinning and no sub-bundles**: the daemon proves exactly one bundle per
  W² window with `chain_steps = 1` (single L2 hop; see `AnchorMode::stride`
  in `bridge-prover-lib/src/lib.rs`). During the wait it polls GQL for the
  next W²-aligned key block to finalize — no intermediate proofs are
  produced, so no `dumped verifyBlock submission` lines appear until the
  covering bundle is ready to submit.

L2 progress watch:

```bash
tail -f logs/live_cold_${BRIDGE_CONFIG_DIR##*/}_*.log | grep -E '(=== Processing|layers=|dumped verifyBlock|confirmed)'
```

**Startup drift check (cross-level guard).** The daemon refuses to boot
if `prover_state.anchor_level != BRIDGE_ANCHOR_LEVEL`
(`bridge-prover-daemon/src/main.rs:115`). If you're switching a running
config from L1 to L2 (or the reverse), rename the offending state
dir first — never auto-migrate anchor levels on a live bridge:

```bash
mv "$BRIDGE_CONFIG_DIR/state" "$BRIDGE_CONFIG_DIR/state.pre_L${OLD_LEVEL}_$(date +%s)"
mkdir -p "$BRIDGE_CONFIG_DIR/state"
```

Similarly, if `$BRIDGE_CONFIG_DIR/relayer-state.json` carries a stale
`last_observed_on_chain` from a prior deploy, the daemon aborts with
`startup on-chain drift vs last_observed_on_chain`. Snapshot the file
(`mv "$BRIDGE_CONFIG_DIR/relayer-state.json" "$BRIDGE_CONFIG_DIR/relayer-state.pre_deploy<N>_<ts>.json"`) and
restart — the daemon writes a fresh one on first observation cycle.

---

## Case 2 — Steady-state operation

Once bootstrapped, cadence is **per bundle**, not per key-block, and depends
on anchor mode:

- **L1** — bundle every `W·P = 1024` seq_nos (~5 min chain-time). Prover is
  the limiter (~10–14 min per bundle for Circuit 2 + SHPLONK aggregation),
  so effective cadence is **~13 min per bundle**.
- **L2** — bundle every `W² = 16384` seq_nos (~91 min chain-time). Chain is
  the limiter (prover finishes in ~10 min and idles until the next W²
  boundary finalizes), so effective cadence is **~91 min per bundle**.

**Where things get written per successful cycle:**

| Path | Written by | Trigger |
|---|---|---|
| `submissions/verifyBlock_seq<N>_fin<T>_<ts>.json` | `bridge.rs:679-724` (env-gated by `BRIDGE_DUMP_SUBMISSIONS_DIR`) | Every submit attempt (before tx send) |
| `$BRIDGE_CONFIG_DIR/relayer-state.json` | `relayer.rs` | Every on-chain observation cycle (~every block) |
| `$BRIDGE_CONFIG_DIR/state/prover_state.json` + `$BRIDGE_CONFIG_DIR/state/prover_bk_set.json` | `live_source.rs:141-157` (`persist_driver`) | Every successful `ack_last_bundle` after on-chain confirmation |

**Steady-state watch commands:**

```bash
# Live daemon log
tail -f crates/an-bridge-prover/logs/live_*.log

# Latest submissions (cadence check)
watch -n 30 'ls -lt crates/an-bridge-prover/submissions/ | head -6'

# On-chain progress
watch -n 60 "cast call $BRIDGE_ADDRESS 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC_URL"
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
kill $(pgrep -f 'relayer .*daemon-live')
sleep 5
pkill -9 -f 'aggregate-proof' 2>/dev/null   # clean up any orphan child

# 2. Rebuild if needed
cargo build --release -p bridge-relayer-daemon --bin relayer

# 3. Relaunch (state files preserved → file-first guard triggers Resume)
# BRIDGE_CONFIG_DIR must already be exported (./L1_config or ./L2_config)
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
TS=$(date +%Y%m%d_%H%M%S)
nohup ./target/release/relayer --state "$BRIDGE_CONFIG_DIR/relayer-state.json" daemon-live \
  > logs/live_restart_${BRIDGE_CONFIG_DIR##*/}_${TS}.log 2>&1 &
```

**Verify Resume:** log must contain `LiveProverDriver seed policy
seed_policy=Resume`, NOT `Explicit(...)`. If you see `Explicit(...)` after a
restart, `$BRIDGE_CONFIG_DIR/state/prover_state.json` is missing — go to [Case 6a](#case-6a--chain-resurrect-advanced-contract--fresh-daemon)
(daemon will auto-resurrect from chain; no operator action needed).

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
# BRIDGE_CONFIG_DIR must already be exported (./L1_config or ./L2_config)
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
RELAYER_ADDR=$(cast wallet address --private-key $RELAYER_PRIVATE_KEY)
cast nonce $RELAYER_ADDR --rpc-url $RPC_URL
cast nonce $RELAYER_ADDR --rpc-url $RPC_URL --block pending
```

If `latest == pending`, no txs in mempool. If `pending > latest`, wait 1-2 min
for pending to clear before restart (otherwise nonce collision).

### 4b. Drift check — local state vs on-chain

```bash
cd crates/an-bridge-prover

L_SEQ=$(jq -r '.last_observed_on_chain.last_seen_block_seq_no' "$BRIDGE_CONFIG_DIR/relayer-state.json")
L_BK=$(python3 -c 'import sys; print(f"0x{int(sys.argv[1], 0):064x}")' \
  "$(jq -r '.last_observed_on_chain.bk_set_commitment' "$BRIDGE_CONFIG_DIR/relayer-state.json")")
L_PREV=$(python3 -c 'import sys; print(f"0x{int(sys.argv[1], 0):064x}")' \
  "$(jq -r '.last_observed_on_chain.prev_max_level_layer_hash' "$BRIDGE_CONFIG_DIR/relayer-state.json")")

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
  | .bk_update_attempts_since_progress = 0' \
  "$BRIDGE_CONFIG_DIR/relayer-state.json" > "$BRIDGE_CONFIG_DIR/relayer-state.json.tmp" \
  && mv "$BRIDGE_CONFIG_DIR/relayer-state.json.tmp" "$BRIDGE_CONFIG_DIR/relayer-state.json"
jq . "$BRIDGE_CONFIG_DIR/relayer-state.json"
```

Without this, the daemon inherits `attempts_since_progress=3` from disk and
hard-aborts on the very next transport blip.

### 4d. Relaunch

Same as [Case 3](#case-3--clean-restart-no-state-loss). Expect
`seed_policy=Resume`. First cycle regenerates the proof from scratch
(prior proof was in RAM, lost on exit) — ~13 min to next submit.

**If it dies again from RPC:** switch `RPC_URL` in the external runtime env to
a stable authenticated endpoint and repeat the nonce/cursor/state checks.
Public-node RPCs are rate-limited and can drop long-lived receipt polls. For a
Compose instance, rerun the one-shot preflight before starting the container.

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
| `0x87bf1c06` | `AttestationProofRejected()` | Adapter equality check on a public input failed. **Bug class: BN254 Fr canonicalization** — if this fires on `blockId`, the client fix in `bridge-relayer-daemon/src/types.rs:83` (`U256::from_be_bytes(b.block_id_be) % BN254_FR_MODULUS`) is missing/reverted. See [`changelog.md` — 2026-08-03 BN254 Fr canonicalization client fix](changelog.md#2026-08-03--bn254-fr-canonicalization-client-fix). |
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

## Case 6a — Chain-resurrect (advanced contract + fresh daemon)

**When it fires.** Any startup where the on-chain contract has history
(`storedLastSeenBlockSeqNo > 0`) but the local `$BRIDGE_CONFIG_DIR/state/prover_state.json`
is either absent, `initialized=false`, or has `stored_last_seen_block_seq_no`
strictly less than chain. This is the operating case for fresh checkouts
on a new machine that need to catch up with an already-advanced deploy.

**No manual bootstrap needed** — the daemon does it automatically. On
startup:

1. Reads the full on-chain state via `getLayerWindow(1..=10)` +
   `storedLastSeenBlockSeqNo` + `storedBkSetCommitment` +
   `storedLastBkSetUpdateSeqNo`.
2. Compares against local state via `startup_decide::decide()`.
3. On the `Resurrect` arm: rebuilds `BridgeState` byte-for-byte from
   the contract snapshot via `BridgeState::from_contract`, atomically
   persists it to `$BRIDGE_CONFIG_DIR/state/prover_state.json`, then drives
   `LiveProverDriver` with `SeedPolicy::Resume`. `BRIDGE_BOOTSTRAP_SEQNO`
   is **ignored** — the seed comes from chain.
4. Continues to Case 2 (steady state) on the next key block after chain
   head.

**What the operator does.**

```bash
cd crates/an-bridge-prover
# BRIDGE_CONFIG_DIR must already be exported (./L1_config or ./L2_config)
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a       # RPC, BRIDGE, private key
TS=$(date +%Y%m%d_%H%M%S)
nohup ./target/release/relayer --state "$BRIDGE_CONFIG_DIR/relayer-state.json" daemon-live \
  > logs/live_${BRIDGE_CONFIG_DIR##*/}_${TS}.log 2>&1 &
```

That's it. No env edits, no `mv $BRIDGE_CONFIG_DIR/state $BRIDGE_CONFIG_DIR/state.stale_*`, no
`compute_bridge_anchors` re-run. `BRIDGE_BOOTSTRAP_SEQNO` may be left
stale — it is only consulted on the `Cold` arm (contract at genesis).

**Expected log signature:**

```
INFO relayer: startup: read on-chain state for routing chain_last_seen=... local_last_seen=0 local_initialized=false
INFO relayer: startup: Resurrect — rebuilding BridgeState from on-chain snapshot chain_last_seen=...
INFO relayer: LiveProverDriver seed policy seed_policy=Resume
```

**When the daemon refuses to auto-resurrect (`Stop` arm).** `decide()`
bails with an explicit reason for any of these:

- `local_last_seen > chain_last_seen` — you have local state ahead of the
  contract; investigate (redeploy? state file from wrong environment?)
  before re-launching.
- Cursors match but `bk_set_commitment` or `layer_windows` diverge from
  chain — state file is from a different contract or a diverged fork.
- `HISTORY_WINDOW_SIZE` mismatch between daemon binary and contract W —
  rebuild against the deployed W.
- `state.anchor_level != BRIDGE_ANCHOR_LEVEL` — state file was written
  under a different anchor level; either restore the matching env or go
  to [Case 6b](#case-6b--state-loss--re-bootstrap-from-mid-chain).
- L1 daemon against contract with `_layerWindows[L>=2].data_len > 0` —
  contract is running higher-stride; either switch to `BRIDGE_ANCHOR_LEVEL=L`
  or point at a different bridge.

If a `Stop` bail was legitimate (e.g. state file really is unrecoverable),
proceed to [Case 6b](#case-6b--state-loss--re-bootstrap-from-mid-chain)
for the manual destructive path.

---

## Case 6b — State loss / re-bootstrap from mid-chain

**When to use.** `$BRIDGE_CONFIG_DIR/state/prover_state.json` deleted / corrupted, OR contract
redeployed at a different `storedLastSeenBlockSeqNo`, AND [Case 6a](#case-6a--chain-resurrect-advanced-contract--fresh-daemon)
auto-resurrect refused (`Stop` arm) with a reason you understand and
accept.

**This is destructive.** Only proceed if you've confirmed the contract is at
a known seed and no in-flight state is worth preserving.

```bash
cd crates/an-bridge-prover
# BRIDGE_CONFIG_DIR must already be exported (./L1_config or ./L2_config)

# 1. Read the contract's CURRENT last_seen from chain
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
CURRENT=$(cast call $BRIDGE_ADDRESS 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC_URL --json | jq -r '.[0]')
echo "chain last_seen = $CURRENT"

# 2. Update BRIDGE_BOOTSTRAP_SEQNO in $BRIDGE_CONFIG_DIR/env to match
#    (must equal what compute_bridge_anchors would produce for that seed)
grep '^BRIDGE_BOOTSTRAP_SEQNO=' "$BRIDGE_CONFIG_DIR/env"
# Manually edit if needed — MUST equal $CURRENT and be aligned to the
# level's stride (L1: W*P=1024; L2: W²=16384)

# 3. Archive any existing state
TS=$(date +%Y%m%d_%H%M%S)
[ -d "$BRIDGE_CONFIG_DIR/state" ] && mv "$BRIDGE_CONFIG_DIR/state" "$BRIDGE_CONFIG_DIR/state.stale_${TS}"
[ -f "$BRIDGE_CONFIG_DIR/relayer-state.json" ] && \
  mv "$BRIDGE_CONFIG_DIR/relayer-state.json" "$BRIDGE_CONFIG_DIR/relayer-state.json.stale_${TS}"
mkdir -p "$BRIDGE_CONFIG_DIR/state"

# 4. Regenerate genesis anchors from the seed block (sanity)
cd ../bridge-prover-lib
cargo run --release --bin compute_bridge_anchors -- \
  --seed-seqno $CURRENT \
  --level $BRIDGE_ANCHOR_LEVEL \
  --gql-endpoint https://shellnet.ackinacki.org/graphql
# Compare its output to `expectedPrevAnchor($BRIDGE_ANCHOR_LEVEL)` and
# `storedBkSetCommitment()` on-chain. All three must match. If not:
# env / contract are out of sync — fix the contract deploy before proceeding.
cd ../an-bridge-prover

# 5. Cold-start launch (identical to Case 1)
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
nohup ./target/release/relayer --state "$BRIDGE_CONFIG_DIR/relayer-state.json" daemon-live \
  > logs/live_rebootstrap_${BRIDGE_CONFIG_DIR##*/}_${TS}.log 2>&1 &
```

**Expect `seed_policy=Explicit($CURRENT)`** in the log. Not `Resume`.

**Why re-bootstrap is dangerous.** If `BRIDGE_BOOTSTRAP_SEQNO` doesn't match
`storedLastSeenBlockSeqNo` on-chain, the first submit reverts with
`BlockSeqNoNotMonotonic` (if you're behind) or `PrevAnchorMismatch` (if
you're ahead of chain but chain expects a different prev-anchor).
Historical note: the `storedLastSeenBlockSeqNo=0` chicken-and-egg motivated
adding `genesisLastSeenBlockSeqNo` to the contract constructor.

---

## Case 7 — L2-anchored cold-start (`--anchor-level 2`)

Merged into [Case 1](#case-1--first-time-bootstrap-from-a-fresh-deploy) —
the three L2 deltas (`W²=16384`-aligned bootstrap seqno, up to ~101 min
first-verify wait, `layers=2` startup-log field) are covered in-line
there. Cases 2–6 are anchor-level opaque; substitute `./L1_config` ↔
`./L2_config` in any command block.

For the L2 E2E withdrawal (burn → `withdraw-e2e` submit), see the
withdraw runbook's
[Case 8](./live_withdrawByProof_runbook.md#case-8--fresh-l2-deploy-first-e2e-withdrawal).

---

## Health checks (run any time)

**On-chain state snapshot:**

```bash
# BRIDGE_CONFIG_DIR must already be exported (./L1_config or ./L2_config)
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
export BRIDGE=$BRIDGE_ADDRESS
export RPC=$RPC_URL
echo "last_seen:         $(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC --json | jq -r '.[0]')"
# num_layers derived from getLatestPerLayer() — highest index with a nonzero hash.
# (Since storage-v2 / commit f8c5ba0, storedNumLayers()/storedLayerHashes(uint256)/getStoredLayerHashes() are gone.)
echo "latest_per_layer:  $(cast call $BRIDGE 'getLatestPerLayer()(uint256[10])' --rpc-url $RPC)"
echo "bk_last_update:    $(cast call $BRIDGE 'storedLastBkSetUpdateSeqNo()(uint64)' --rpc-url $RPC --json | jq -r '.[0]')"
echo "bk_commitment:     $(cast call $BRIDGE 'storedBkSetCommitment()(uint256)' --rpc-url $RPC)"
echo "expectedPrev($BRIDGE_ANCHOR_LEVEL): $(cast call $BRIDGE 'expectedPrevAnchor(uint8)(uint256)' $BRIDGE_ANCHOR_LEVEL --rpc-url $RPC)"
echo "storedPrev(genesis): $(cast call $BRIDGE 'storedPrevMaxLevelLayerHash()(uint256)' --rpc-url $RPC)"
# storedPrevMaxLevelLayerHash is `immutable` (constructor-set) — it holds
# the genesis seed forever, not the last-block max-level. The dynamic
# per-block anchor lives in _layerWindows and is folded via
# expectedPrevAnchor(numLayers) at submit time.
```

**Daemon liveness:**

```bash
pgrep -a -f 'relayer .*daemon-live'                # parent PID + cmdline
pgrep -a -f 'aggregate-proof'                      # active aggregator subprocess (should exist mid-cycle)
```

**Recent activity:**

```bash
# BRIDGE_CONFIG_DIR must already be exported (./L1_config or ./L2_config)

# Last successful ack (mtime of $BRIDGE_CONFIG_DIR/state/prover_state.json)
stat -f "%Sm  %N" "crates/an-bridge-prover/$BRIDGE_CONFIG_DIR/state/prover_state.json"

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
# Refill from the faucets listed in
# [Deploy your own bridge bundle from scratch §2](#2-fund-it-with-sepolia-eth)
# if <0.5 ETH.
```

---

## File & state reference

Everything lives under `crates/an-bridge-prover/` (the daemon's working
directory). The sibling `crates/bridge-relayer-daemon/` directory is
**source code only** — no runtime data lands there.

```
crates/an-bridge-prover/
├── shellnet.common                  ← tracked development fixture; not a server secret store
├── L1_config/                       ← L1-anchor mode config dir (BRIDGE_CONFIG_DIR=./L1_config)
│   ├── env                          ← sources ../shellnet.common + 4 L1 overrides
│   ├── relayer-state.json           ← verifier cursor + on-chain observation cache
│   ├── state/
│   │   ├── prover_state.json        ← LiveProverDriver snapshot (~574 KB) — auth. resume point
│   │   └── prover_bk_set.json       ← active BK-set snapshot (updated only via applyBkSetUpdate)
│   ├── proofs/                      ← per-mode SHPLONK-wrapped Circuit 4 output (event proofs)
│   └── work_dir/                    ← per-mode enriched witnesses + shplonk-snark scratch
├── L2_config/                       ← same layout as L1_config; BRIDGE_CONFIG_DIR=./L2_config (W²-stride mode)
│   ├── env
│   ├── relayer-state.json
│   ├── state/
│   ├── proofs/
│   └── work_dir/
├── bk_set.shellnet.json             ← bootstrap BK-set (5 signers) — shared across L1/L2
├── submissions/                     ← calldata dumps (env-gated by BRIDGE_DUMP_SUBMISSIONS_DIR)
├── logs/                            ← daemon stdout+stderr
├── params/                          ← SRS + circuit VKs/PKs (~17 GB, DO NOT WIPE)
└── target/release/relayer           ← the binary
```

**Path resolution (`bridge-prover-lib::paths`):** the daemon reads
`BRIDGE_CONFIG_DIR` (exported by the caller before sourcing the per-mode
env file — see [Prereqs Step 1](#step-1--choose-the-anchor-mode)) and
derives `paths::state_dir()` → `$BRIDGE_CONFIG_DIR/state`,
`paths::proofs_dir()` → `$BRIDGE_CONFIG_DIR/proofs`,
`paths::bootstrap_seed_file()` → `$BRIDGE_CONFIG_DIR/state/bootstrap_seed.json`.
`BRIDGE_STATE_DIR` / `BRIDGE_PROOFS_DIR` are honored as narrower
overrides if set explicitly.

**Persistence triggers (see [`../../bridge-relayer-daemon/src/live_source.rs:115,132,210,259`](../../bridge-relayer-daemon/src/live_source.rs)):**

- `$BRIDGE_CONFIG_DIR/state/prover_state.json` + `$BRIDGE_CONFIG_DIR/state/prover_bk_set.json` are
  written together by `persist_driver` on every successful
  `ack_last_bundle` / `ack_bk_update`. Not on every submit — only on
  confirmed ACK.
- `$BRIDGE_CONFIG_DIR/relayer-state.json` is written on every on-chain observation refresh
  (~every block).

**Cleanup rules:**

- `submissions/` — safe to prune anytime; purely diagnostic.
- `logs/` — safe to prune, but keep the most recent for post-mortem.
- `$BRIDGE_CONFIG_DIR/state/`, `$BRIDGE_CONFIG_DIR/relayer-state.json` — **NEVER** delete a running
  daemon's active state. To reset, archive to `$BRIDGE_CONFIG_DIR/state.stale_<ts>/`
  + `$BRIDGE_CONFIG_DIR/relayer-state.json.stale_<ts>` first (see [Case 6b](#case-6b--state-loss--re-bootstrap-from-mid-chain)).
- `$BRIDGE_CONFIG_DIR/work_dir/` — safe to prune between events; each `withdraw-e2e`
  run repopulates its own subdir.
- `params/` — never delete; keygen takes ~7 min per circuit.

---

## Onboarding wrap-up — new operator on a fresh L2 server

Written for a colleague picking up this runbook cold on a new n14-class
machine. Concrete task: **deploy a fresh bundle near chain head, run the L2
daemon through Docker Compose, and leave a reproducible private deployment
record plus observable persistent state.**

Scope of *this* deployment (dismisses several open questions upfront):

- **Single relayer instance** — no parallel provers, no catch-up workers.
- **L2 anchor mode only** — `BRIDGE_CONFIG_DIR=./L2_config`, stride `W² = 16384`.
- **Shellnet → Sepolia only** — mainnet is not in scope for this instance.
- **BK-set fixed from shellnet genesis** — no rotation, no replay.

### Do this, in order

1. **Pin + build** — check out one reviewed commit and build both release
   binaries with `--locked`. Record the commit and binary SHA-256 values.
2. **Wallet + secrets** — generate a dedicated Sepolia EOA, fund it and keep
   its key/RPC credentials only in root-owned env files outside the clone.
   Never reuse the tracked development burner for a server.
3. **SRS + toolchain** — provision K=17,19,20,21,22, generate the inner
   PK/VK/config set, install `solc 0.8.19`, then seal and hash the static
   artifact manifest. Keep the outer `pk_cache` writable on a bind mount.
4. **Deploy the bundle** — load deployment values from the external env and
   run the helper once from `crates/an-bridge-prover/`:
   ```bash
   CONFIRM_NEW_BRIDGE_DEPLOY=DEPLOY_NEW_CONTRACTS \
     LEVEL=2 PRIVATE_KEY=<burner> ./scripts/deploy_bridge_bundle.sh
   ```
   Retain the full Foundry broadcast record and all constructor/anchor values.
   A retry deploys new addresses; audit nonce and receipts first. Detail:
   [Deploy your own bridge bundle from scratch](#deploy-your-own-bridge-bundle-from-scratch).
5. **Preflight + cold start** — fill the external runtime env, then follow the
   [Compose kit](../deploy/shellnet-l2/README.md). Expect the first confirmation
   after the next W² boundary plus proof time: roughly 0–91 min chain wait plus
   about 10 min proving on the measured host.
6. **Acceptance** — require a Sepolia receipt with `status=1`, equal local and
   on-chain cursors, no pending nonce, and matching verifier bytecode. Perform
   one controlled stop/start and require `WarmResume` without a duplicate tx.
7. **Long-run watch** — run `scripts/status.sh` and retain submission JSONs.
   L2 normally advances once per 16,384 seq_nos (~91 min at the observed
   shellnet rate); `NotYetAvailable` between boundaries is healthy.

### Answers to the PR #31 review questions

| Question | Verdict | Where |
|---|---|---|
| Shellnet backlog — new bridge near head, or catch-up / parallel provers? | **New bridge near head.** No catch-up model exists on this branch; `compute_bridge_anchors --at-head` pins the newest stride-aligned boundary ≤ chain head at deploy time. | [§3 "The one thing to know"](#deploy-your-own-bridge-bundle-from-scratch) |
| Mainnet destination chain + production RPC | **N/A for this instance.** Sepolia (chain 11155111) only; use an authenticated runtime RPC. | intro |
| Bridge address / deploy tx / bootstrap height / initial cursors | **Mandatory private handoff data.** Record the bridge address, every deployment tx/receipt, genesis seed/height/anchor/BK commitment and initial local/on-chain cursor. | [§3](#deploy-your-own-bridge-bundle-from-scratch) |
| "Exact verifier stack version" | Pin source/binaries/params by hash. Preflight walks each adapter → wrapper → Yul verifier and byte-compares deployed runtime code with the packaged `.bin`. | [Compose kit](../deploy/shellnet-l2/README.md) |
| Authoritative genesis BK set + checksum | `bk_set.shellnet.json` in-repo. Poseidon commitment `0x08eb0a…ca71c`, SHA-256 `c77e3d…898f`. Fixed on shellnet from genesis; no rotation. | intro (top of doc) |
| Who fixes BK-cursor init / replays rotations from genesis? | **N/A on shellnet** (rotation off — confirmed with Sehor 2026-07-08). Mainnet concern only. | intro |
| Proof artifacts from Alina with block ranges / params / checksums | Historical proofs are not required for a fresh near-head bridge. SRS/PK/VK artifacts are required and must be locally verified against a retained checksum manifest. | [Prereqs Step 3](#step-3--provision-the-hermez-kzg-srs-one-time-large-download--cpu) |
| Funded relayer EOA / secrets transfer / monitoring owner | Dedicated EOA and external root-owned secret env. One named operator owns restart/nonce reconciliation; Compose status/logs provide the checks. | [Compose kit](../deploy/shellnet-l2/README.md) |
| n14 sizing / core scaling read | n14 is adequate for the measured L2 cadence. Stages run sequentially, but individual Halo2 stages are multi-core; extra cores improve those stages rather than multiplying independent bundles. Live acceptance peaked near 23 GiB RAM and produced an outer cache around 17 GiB. | [daemon_live_performance.md](daemon_live_performance.md) |
