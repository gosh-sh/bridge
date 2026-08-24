# Live `verifyBlock` E2E Runbook — shellnet → Sepolia (bundle-only, Circuits 1A + 2)

Operational guide for running the `bridge-relayer-daemon daemon-live` binary
against a deployed `AckiNackiBridge` on Sepolia, driven by the shellnet
GraphQL endpoint. Covers first-time bootstrap, steady-state operation, and
recovery from the failure modes we have actually hit in production.

**Scope of this runbook.** Bundle-only path: Circuit 1A/1B (attestation) +
Circuit 2 (layer hashes), aggregated by R15 SHPLONK, submitted via
`verifyBlock`. **No** BK-set updates (Circuit 3 / `applyBkSetUpdate`) and
**no** event proofs (Circuit 4 / `withdrawByProof`). For the full E2E
including Circuit 4 see [`../TECHNICAL_README.md`](../../../../../crates/an-bridge-prover/TECHNICAL_README.md).

> **Notation:** `seq_no` is Acki Nacki block sequence number.
> "Key block" = every `SEQ_NO % (W*P) == 0` block; only key blocks trigger a
> bundle proof (currently `W=128, P=4`, so stride 1024).
>
> **Anchor level.** All L1-only cases below assume `BRIDGE_ANCHOR_LEVEL=1`
> (default) — bundle-covering cadence 1024. For L2-anchored deploys
> (`BRIDGE_ANCHOR_LEVEL=2`), the daemon still processes every W·P bundle
> (Circuit 1a/1b/2 cadence unchanged), but the covering layer-hash
> boundary the on-chain verifier accepts advances every `W² = 16384`
> seq_nos (~91 min chain-time on shellnet). See [Case 7](#case-7--l2-anchored-cold-start--anchor-level-2)
> for the L2 cold-start delta. All other cases (2-6) apply verbatim — the
> daemon's anchor mode is opaque to steady-state recovery.
>
> **L2 maturity status: SMOKE-PENDING.** Shellnet Deploy #12 (2026-08-18)
> verified cold-start + first covering bundle end-to-end; no multi-day
> continuous-production run yet on record. L1 is the exercised default
> (dev, CI, one-fire-and-withdraw E2E per iteration). Both daemons emit a
> `warn!` on startup when `BRIDGE_ANCHOR_LEVEL=2` is selected — that is
> expected, not a fault. Circuit 2 itself is level-parametric (same VK,
> same on-chain verifier); the gap is operational coverage, not the ZK
> stack.

---

## Table of Contents

- [Quick resume checklist (returning to a running system)](#quick-resume-checklist-returning-to-a-running-system)
- [Reference addresses (current deploy)](#reference-addresses-current-deploy)
- [Binary + env prerequisites](#binary--env-prerequisites)
- [Fund the relayer wallet with Sepolia ETH](#fund-the-relayer-wallet-with-sepolia-eth)
- [Shared test deploy (open — for quick onboarding)](#shared-test-deploy-open--for-quick-onboarding)
- [Deploy your own bridge bundle (external users)](#deploy-your-own-bridge-bundle-external-users)
- [Case 1 — First-time bootstrap from a fresh deploy](#case-1--first-time-bootstrap-from-a-fresh-deploy)
- [Case 2 — Steady-state operation](#case-2--steady-state-operation)
- [Case 3 — Clean restart (no state loss)](#case-3--clean-restart-no-state-loss)
- [Case 4 — Restart after RPC-induced hard-abort](#case-4--restart-after-rpc-induced-hard-abort)
- [Case 5 — Restart after on-chain revert](#case-5--restart-after-on-chain-revert)
- [Case 6a — Chain-resurrect (shared contract + fresh daemon)](#case-6a--chain-resurrect-shared-contract--fresh-daemon)
- [Case 6b — State loss / re-bootstrap from mid-chain](#case-6b--state-loss--re-bootstrap-from-mid-chain)
- [Case 7 — L2-anchored cold-start (`--anchor-level 2`)](#case-7--l2-anchored-cold-start--anchor-level-2)
- [Health checks (run any time)](#health-checks-run-any-time)
- [File & state reference](#file--state-reference)
- [Change log / known incidents](#change-log--known-incidents)

---

## Quick resume checklist (returning to a running system)

Run this **before touching anything** — it takes 30 seconds and tells you
exactly which case (below) applies.

```bash
cd crates/an-bridge-prover
set -a && source .env.shellnet && set +a
export BRIDGE=$BRIDGE_ADDRESS
export RPC=$RPC_URL

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
| not running | **no** | any | [Case 5](#case-5--restart-after-on-chain-revert) or [Case 6b](#case-6b--state-loss--re-bootstrap-from-mid-chain) — do not restart blindly |
| running/not | state/ missing | — | [Case 6a](#case-6a--chain-resurrect-shared-contract--fresh-daemon) (auto-resurrect from chain) |

**Authoritative branch (as of 2026-08-04):** `refactoring_and_review_bridge_relayer_demon`
in `bridge-relayer-daemon/`. The BN254 Fr client fix (`types.rs:83`) is
committed on this branch — see [Change log](#change-log--known-incidents).

---

## Reference addresses (current deploy)

Source of truth: [`../../../bridge-deployer.txt`](../../../bridge-deployer.txt) —
read the latest `Deploy #N` section for current addresses. Values below are
Deploy #4 (2026-08-03, active).

| Item | Value |
|---|---|
| Network | Sepolia (chain 11155111) |
| RPC | `https://ethereum-sepolia-rpc.publicnode.com` |
| `AckiNackiBridge` | `0x36272c871d9389E77d0b95F931BA0B0f74d43818` |
| `PrimaryAggregatorVerifier` | `0x653ceeAaC0b77f6A9c5E6cAB648a434c707fa6eD` |
| `FallbackAggregatorVerifier` | `0x4DA9474C7b9a6c45cC96DDdc9fB8679846076B1e` |
| `LayerHashesAggregatorVerifier` | `0xA6f0bc033485751a3a8C301441b672E1d07CeD79` |
| `MockBlockHeaderOracle` | `0x05eE6Ee62696efA7B6B8dA1Ce520f16CC3bC2f2f` |
| Bootstrap seed seq_no | `4887552` |
| Deployer / relayer wallet | `0x841709B6842233d8474aeA1d773e8d0F7c7c0B9f` |
| Genesis `bk_set_commitment` | `0x08eb0a1892e4f75a8b5c8cff69322f95bf0437c371903998c9365fbe293ca71c` |
| Genesis `prev_max_level_layer_hash` | `0x268e7b0af653733a850d2fd7ee2cff346145bf5e9c4559f90e024829a7869158` |

Any redeploy invalidates all seven — regenerate `.env.shellnet` from the
new `bridge-deployer.txt` section (see [Case 6b](#case-6b--state-loss--re-bootstrap-from-mid-chain)).

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
contract's `storedLastSeenBlockSeqNo` at construction (visible in
`bridge-deployer.txt` and via `cast call ... storedLastSeenBlockSeqNo()`).

---

## Fund the relayer wallet with Sepolia ETH

Two faucets that reliably deliver **without** an anti-Sybil mainnet-deposit
gate (verified 2026-08):

- **pk910 PoW** — https://sepolia-faucet.pk910.de/  (mine in-browser, ~5–15 min for the target amount)
- **Google Cloud Web3 faucet** — https://cloud.google.com/application/web3/faucet/ethereum/sepolia  (0.05 ETH/day, no PoW)

Faucets that require depositing ≥0.001 ETH on mainnet first (Alchemy /
Infura / QuickNode) are usable once you're funded; they hand out 0.05–0.5
ETH/day and are the practical top-up path after bootstrap.

**Budget.** The one-shot deploy of the 6-contract bundle
(`DeployShellnetE2EBridge.s.sol`: `AckiNackiBridge` + 4 SHPLONK verifiers +
`MockBlockHeaderOracle`) cost **0.063 ETH** on 2026-08-13 (30M gas @
2.1 gwei). Add a running budget of ~0.001–0.003 ETH per `verifyBlock`
submit (one per bundle stride — 512 blocks in L1 mode, 4096 in L2).
**Target ≥ 0.1 ETH before deploy**, ≥ 0.5 ETH for a multi-day E2E run.

The [Health checks](#health-checks-run-any-time) block at the end of this
runbook includes a wallet-balance line — refill from either faucet above
when it drops below 0.5 ETH.

---

## Shared test deploy (open — for quick onboarding)

The team maintains a **deliberately public** Sepolia burner + bridge bundle,
deployed so a third-party developer can `git clone` and run
`./target/release/relayer daemon-live` end-to-end without deploying anything
or funding a wallet.

**Where to find it.** Current shared-deploy credentials (burner address,
burner private key, contract addresses, seed seq_no, genesis anchors) live
in [`../../../bridge-deployer.txt`](../../../bridge-deployer.txt) under the
latest `Deploy #N (shared)` section. The [Reference addresses](#reference-addresses-current-deploy)
table above pins one specific deploy for stability of the runbook; check
`bridge-deployer.txt` for the current shared bundle if the pinned addresses
have been superseded.

**When to use it.** One-shot smoke test — confirm the daemon builds, picks
a startup arm, generates a proof, and submits a real `verifyBlock` tx that
lands on Sepolia.

**When NOT to use it.**

- Continuous / multi-day operation.
- Any workflow where two developers run against it concurrently. The
  burner is a single key; concurrent submits from different machines
  collide on nonce and one side reverts.
- Anything you care about not being drained. The burner key is committed
  to `bridge-deployer.txt` — Sepolia key-scraper bots harvest it within
  minutes. Treat any balance as ephemeral.

For serious work, use [Deploy your own bridge bundle](#deploy-your-own-bridge-bundle-external-users)
to spin up a private burner + private contract bundle.

### Recommended server-side path (headless Linux)

**Best strategy: shared *contract*, your own *key*.** `verifyBlock`
(`AckiNackiBridge.sol:650`) is permissionless (`external nonReentrant`, no
role gate) — anyone with a funded Sepolia burner and a valid proof can
submit against the shared bridge address. Using your own key avoids the
two problems of the shared burner: nonce collisions when several devs run
concurrently, and getting drained by faucet-scraper bots. Reserve the
in-repo burner for a one-shot "does my clone build and land a tx" smoke
test.

```bash
# 1. Clone + build (~15 min)
git clone https://github.com/gosh-sh/bridge.git && cd bridge
cd crates/an-bridge-prover
cargo build --release -p bridge-relayer-daemon --bin relayer
(cd ../bridge-evm-aggregator && cargo build --release)

# 2. Provision params/ (~30 min one-time). See TECHNICAL_README.md
#    "KZG SRS provisioning"; or rsync ~17 GB from a teammate.

# 3. Wire env — shared contract, YOUR key (fund via API-key faucet:
#    Alchemy / Infura / QuickNode all work headlessly). Copy the shared
#    contract addresses + seed seq_no from bridge-deployer.txt.
cat > .env.shellnet <<'EOF'
RPC_URL=https://ethereum-sepolia-rpc.publicnode.com
BRIDGE_ADDRESS=<AckiNackiBridge from bridge-deployer.txt>
RELAYER_PRIVATE_KEY=<your funded Sepolia burner>
BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql
BRIDGE_BOOTSTRAP_SEQNO=<seed seq_no from bridge-deployer.txt>
BRIDGE_BK_SET_CONFIG=./bk_set.shellnet.json
BRIDGE_PARAMS_DIR=./params
BRIDGE_STATE_DIR=./state
BRIDGE_AGGREGATOR_DIR=../bridge-evm-aggregator
BRIDGE_VERIFIERS_DIR=../../contracts/ethereum/verifiers
EOF

# 4. Launch under tmux/screen for server persistence
mkdir -p state logs
tmux new -d -s bridge \
  "set -a && source .env.shellnet && set +a && \
   ./target/release/relayer daemon-live 2>&1 | tee logs/live_$(date +%s).log"

# 5. Confirm the startup arm within 30 s
tmux capture-pane -t bridge -p | grep -E 'startup:|seed_policy='
```

**Expected first-launch signature against a shared contract with history:**
the daemon will pick `Resurrect` (not `Cold`) — see [Case 6a](#case-6a--chain-resurrect-shared-contract--fresh-daemon).

**Two gotchas on a shared contract:**

- `publicnode.com` drops long receipt polls under load — if logs show
  `WARN … transport failure` on the receipt wait, swap to an
  Alchemy / Infura / dRPC endpoint (`RPC_URL=` in `.env.shellnet`).
- Multiple daemons racing the same cursor: only one wins per key-block
  stride; losers hit `BlockSeqNoNotMonotonic` (Case 5). Fine for
  testing the revert-recovery path, but coordinate on team chat if
  you need every submit to land. For fully-isolated work, jump to
  [Deploy your own bridge bundle](#deploy-your-own-bridge-bundle-external-users).

---

## Deploy your own bridge bundle (external users)

Running this E2E requires an on-chain `AckiNackiBridge` **that you own and
fund**. `verifyBlock` (`AckiNackiBridge.sol:650`) is a state-mutating
`external nonReentrant` function; every relayer submit is a signed Sepolia
transaction. The daemon reads its signer from the `RELAYER_PRIVATE_KEY`
env var (`bridge-relayer-daemon/src/bin/relayer.rs:86`); there is no
default. The [shared test deploy](#shared-test-deploy-open--for-quick-onboarding)
above ships a burner + contract for one-shot onboarding, but it is
single-key and gets drained by faucet bots — for continuous work, spin
up your own burner + contract bundle here.

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

See [Fund the relayer wallet with Sepolia ETH](#fund-the-relayer-wallet-with-sepolia-eth)
above.

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
# --at-head picks the newest boundary ≤ head aligned to the target
# anchor level:
#   L1: newest W*P = 1024-block key-block boundary  (--level 1, default)
#   L2: newest W² = 16384-block covering boundary   (--level 2)
# Outputs:
#   GENESIS_LAST_SEEN_BLOCK_SEQNO      = that boundary (the seed key block)
#   GENESIS_PREV_MAX_LEVEL_LAYER_HASH  = anchor at the target level
#                                        (L1: layer-1 root; L2: L2 fold)
#   GENESIS_BK_SET_COMMITMENT          = Poseidon commitment of bk_set.shellnet.json
#                                        (stable while shellnet BK rotation is off)
cd ../../crates/an-bridge-prover/bridge-prover-lib
cargo run --release --bin compute_bridge_anchors -- \
  --at-head \
  --gql-endpoint https://shellnet.ackinacki.org/graphql
# For L2: append `--level 2`

cd ../../contracts/ethereum
set -a && source .env && set +a
export GENESIS_BK_SET_COMMITMENT=0x...
export GENESIS_PREV_MAX_LEVEL_LAYER_HASH=0x...
export GENESIS_LAST_SEEN_BLOCK_SEQNO=<from compute_bridge_anchors>
export WIRE_WITHDRAW_BY_PROOF=true            # required by constructor since 7a645e5
export WITHDRAW_ACC_FR=0x1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a  # placeholder OK for verifyBlock-only

forge script script/DeployShellnetE2EBridge.s.sol:DeployShellnetE2EBridge \
  --rpc-url $SEPOLIA_RPC_URL --broadcast --slow
```

The current `AckiNackiBridge.sol` has no Pausable inheritance — user
entrypoints are live the moment the deploy tx confirms. No post-deploy
unpause step needed.

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
# For L2 anchoring add: BRIDGE_ANCHOR_LEVEL=2  (see Case 7)
```

Now jump to [Binary + env prerequisites](#binary--env-prerequisites) and
[Case 1 — First-time bootstrap from a fresh deploy](#case-1--first-time-bootstrap-from-a-fresh-deploy)
(L1) or [Case 7](#case-7--l2-anchored-cold-start--anchor-level-2) (L2).

*Maintainer-only note:* Alina keeps her personal burner + per-deploy
change history + genesis-anchor paper trail in
`~/HALO2_TVM_EXPERIMENTS/bridge-deployer.txt` — off-tree, not shipped.

---

## Case 1 — First-time bootstrap from a fresh deploy

**When to use.** Contract just deployed; no `state/prover_state.json` yet.

**Pre-flight (contract sanity):**

```bash
set -a && source .env.shellnet && set +a
export BRIDGE=$BRIDGE_ADDRESS
export RPC=$RPC_URL

cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)'      --rpc-url $RPC   # == $BRIDGE_BOOTSTRAP_SEQNO
cast call $BRIDGE 'expectedPrevAnchor(uint8)(uint256)' 1    --rpc-url $RPC   # matches env GENESIS_PREV_MAX_LEVEL_LAYER_HASH
cast call $BRIDGE 'storedBkSetCommitment()(uint256)'        --rpc-url $RPC   # matches env GENESIS_BK_SET_COMMITMENT
```

If any of these don't match your deploy's genesis values (from the
`compute_bridge_anchors` output you pinned at deploy time, or the
`bridge-deployer.txt` section for the shared deploy), **stop** — the
deploy is broken. Do not launch the daemon.

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
INFO relayer: LiveProverDriver seed policy seed_policy=Explicit(<BRIDGE_BOOTSTRAP_SEQNO>)   ← cold-start signature
```

`seed_policy=Explicit(N)` means the daemon is seeding from
`BRIDGE_BOOTSTRAP_SEQNO`. `seed_policy=Resume` would mean it found existing
`state/prover_state.json` and is resuming — wrong for cold start.

**How the daemon picks its startup path.** On startup the daemon reads the
*full* on-chain state (four scalars + all 10 `_layerWindows` via
`getLayerWindow(uint8)`) and routes through one of four arms via
`startup_decide::decide()` (`bridge-relayer-daemon/src/startup_decide.rs`).
Inputs: local `state/prover_state.json` + on-chain
`EthBridgeContractState` + `BRIDGE_BOOTSTRAP_SEQNO` +
`BRIDGE_ANCHOR_LEVEL`. `seed_policy` (`Resume` / `Explicit(N)` / `Auto`)
is derived — not directly chosen — as a consequence of the arm picked.

| local state file                       | on-chain contract                                | arm            | `seed_policy` |
|---|---|---|---|
| absent / `initialized=false`           | genesis (`last_seen == 0`)                       | **Cold**       | `Explicit(N)` if `$BRIDGE_BOOTSTRAP_SEQNO` set else `Auto` |
| absent / `initialized=false`           | advanced (`last_seen > 0`)                       | **Resurrect**  | `Resume` (state rebuilt from chain first) |
| present, cursor / commitment / windows byte-match chain | same                                    | **WarmResume** | `Resume` |
| present, `local_last_seen < chain_last_seen` (co-tester advanced it) | advanced                    | **Resurrect**  | `Resume` (state overwritten from chain) |
| present, `local_last_seen > chain_last_seen`                        | genesis or older            | **Stop**       | (daemon bails) |
| present, cursors match but bk-commit / windows diverge              | same cursor                 | **Stop**       | (daemon bails) |
| window-size mismatch (contract W ≠ daemon `HISTORY_WINDOW_SIZE`)    | (any)                       | **Stop**       | (daemon bails) |
| anchor-level mismatch (`state.anchor_level != BRIDGE_ANCHOR_LEVEL`) | (any)                       | **Stop**       | (daemon bails; see Case 7) |
| L1 daemon against contract with `_layerWindows[L>=2].data_len > 0`   | L≥2-advanced               | **Stop**       | (daemon bails; contract is running higher-stride) |

`state.initialized` lives in `state/prover_state.json` and is flipped to
`true` by `persist_driver` (`live_source.rs:141-157`) after the first
successful on-chain ACK, **or** by `Resurrect` when it rebuilds the mirror
from `getLayerWindow` and saves it before driver construction.
`Explicit(N)` also requires `N > 0 && N % (W*P) == 0` — bad values abort
in `LiveProverDriver::new`.

**"Byte-match" is not literal on layer windows.** `startup_decide::decide()`
normalizes endianness before comparing:

- `bk_set_commitment` — chain returns `uint256` (BE); local is
  `Fr::to_repr()` (LE). `to_lib_full_state` converts chain to LE.
- `_layerWindows[i].data[j]` — each slot is a BE `uint256`; local is LE.
  `to_lib_layer_window` reverses per slot.
- Cold-start genesis prepend — `BootstrapSeed::apply()` pushes the seed's
  per-layer `history_proofs` into `layer_windows[i].data[0]` for every
  layer in the seed's max_level (see `bootstrap.rs:17-18`); chain only
  materializes layer 1's anchor as `storedPrevMaxLevelLayerHash` and
  leaves `_layerWindows[k>=2]` empty until a verifyBlock promotes them.
  `windows_match` therefore walks each ring oldest→newest and accepts a
  `local.len == chain.len + 1` offset: layer 1's genesis slot is
  byte-verified against `storedPrevMaxLevelLayerHash` (BE→LE); layers
  2..10 accept the +1 offset without per-slot verification, and the
  runtime ack-time top-hash check (`history_consistency.rs:38-47`) is
  the authoritative gate. `last_height` is not compared standalone —
  the chronological tuple compare covers heights for populated slots
  (chain's empty-layer `last_height=0` would otherwise false-reject a
  local layer holding only a genesis prepend).

For cold start (this Case), the log MUST show
`startup: Cold — contract at genesis, bootstrapping` followed by
`LiveProverDriver seed policy seed_policy=Explicit(<BOOTSTRAP_SEQNO>)`.
If instead you see `startup: Resurrect …` or `startup: WarmResume …`,
the contract already has history — that's expected in the shared-test
multi-tester scenario (see [Case 6a](#case-6a--chain-resurrect-shared-contract--fresh-daemon))
and no operator intervention is needed.

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
restart, `state/prover_state.json` is missing — go to [Case 6a](#case-6a--chain-resurrect-shared-contract--fresh-daemon)
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
set -a && source .env.shellnet && set +a
RELAYER_ADDR=$(cast wallet address --private-key $RELAYER_PRIVATE_KEY)
cast nonce $RELAYER_ADDR --rpc-url $RPC_URL
cast nonce $RELAYER_ADDR --rpc-url $RPC_URL --block pending
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

## Case 6a — Chain-resurrect (shared contract + fresh daemon)

**When it fires.** Any startup where the on-chain contract has history
(`storedLastSeenBlockSeqNo > 0`) but the local `state/prover_state.json`
is either absent, `initialized=false`, or has `stored_last_seen_block_seq_no`
strictly less than chain. This is the **default** operating case for
multi-tester development against a shared bridge, and for fresh checkouts
on a new machine that need to catch up with an already-advanced deploy.

**No manual bootstrap needed** — the daemon does it automatically. On
startup:

1. Reads the full on-chain state via `getLayerWindow(1..=10)` +
   `storedLastSeenBlockSeqNo` + `storedBkSetCommitment` +
   `storedLastBkSetUpdateSeqNo`.
2. Compares against local state via `startup_decide::decide()`.
3. On the `Resurrect` arm: rebuilds `BridgeState` byte-for-byte from
   the contract snapshot via `BridgeState::from_contract`, atomically
   persists it to `state/prover_state.json`, then drives
   `LiveProverDriver` with `SeedPolicy::Resume`. `BRIDGE_BOOTSTRAP_SEQNO`
   is **ignored** — the seed comes from chain.
4. Continues to Case 2 (steady state) on the next key block after chain
   head.

**What the operator does.**

```bash
cd crates/an-bridge-prover
set -a && source .env.shellnet && set +a       # RPC, BRIDGE, private key
TS=$(date +%Y%m%d_%H%M%S)
nohup ./target/release/relayer daemon-live > logs/live_${TS}.log 2>&1 &
```

That's it. No env edits, no `mv state state.stale_*`, no
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

**When to use.** `state/prover_state.json` deleted / corrupted, OR contract
redeployed at a different `storedLastSeenBlockSeqNo`, AND [Case 6a](#case-6a--chain-resurrect-shared-contract--fresh-daemon)
auto-resurrect refused (`Stop` arm) with a reason you understand and
accept.

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
you're ahead of chain but chain expects a different prev-anchor). The
[Deploy #4 log in `bridge-deployer.txt`](../../../bridge-deployer.txt) has a
detailed post-mortem of the `storedLastSeenBlockSeqNo=0` chicken-and-egg
that motivated adding `genesisLastSeenBlockSeqNo` to the contract
constructor.

---

## Case 7 — L2-anchored cold-start (`--anchor-level 2`)

**When to use.** Fresh deploy where you want the on-chain verifier to
accept only L2 (`W²`-stride) covering bundles — analogous to Case 1 but
for `BRIDGE_ANCHOR_LEVEL=2`. See the withdraw runbook's
[Case 8](./live_withdrawByProof_runbook.md#case-8--fresh-l2-deploy-first-e2e-withdrawal)
for the full L2 E2E including the burn + `withdraw-e2e` submit; this
section covers only the daemon-live half.

**Key deltas vs Case 1:**

| Item | L1 (Case 1) | L2 (Case 7) |
|---|---|---|
| Bundle stride (daemon cadence) | `W·P = 1024` | `W·P = 1024` (unchanged) |
| **Covering** bundle stride (on-chain) | `1024` (~5.7 min) | `W² = 16384` (~91 min) |
| Env override | (unset / `=1`) | `BRIDGE_ANCHOR_LEVEL=2` |
| Genesis-anchor derivation | `compute_bridge_anchors --at-head` | `compute_bridge_anchors --level 2 --at-head` |
| First covering-bundle wait | ~5-10 min from cold start | up to ~101 min (W²−1 chain-time + ~10 min prover) |
| Local state dir | `state/` | **`state_l2/`** (must be distinct — startup drift check refuses to boot cross-level) |
| Startup log signature | `seed_policy=Explicit(N)` + `layers=1` | `seed_policy=Explicit(N)` + `layers=2` |
| Contract selector | `expectedPrevAnchor(1)` | `expectedPrevAnchor(2)` (verifier-side; equality against `storedPrevMaxLevelLayerHash` is level-opaque) |

**Pre-flight (contract sanity, L2):**

```bash
export BRIDGE=0xf31E316C7E3FD4aDDBd86d6d63a1444947BFFEEE     # Deploy #12 example
export RPC=https://ethereum-sepolia-rpc.publicnode.com

cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC   # matches GENESIS_LAST_SEEN_BLOCK_SEQNO
cast call $BRIDGE 'storedPrevMaxLevelLayerHash()(uint256)' --rpc-url $RPC   # matches GENESIS_PREV_MAX_LEVEL_LAYER_HASH (L2 fold)
cast call $BRIDGE 'storedBkSetCommitment()(uint256)' --rpc-url $RPC     # matches GENESIS_BK_SET_COMMITMENT
```

Contract itself is level-opaque — the same three storage slots are
compared regardless of `anchor_level`. The value in
`storedPrevMaxLevelLayerHash` *is* the L2 fold when derived via
`--level 2`.

**Env file (`.env.shellnet.l2`) — L2-specific deltas:**

```bash
BRIDGE_ANCHOR_LEVEL=2                    # strict passthrough; daemon-side stride() → 16384
BRIDGE_STATE_DIR=./state_l2              # isolate from L1 state; startup drift check refuses cross-level resume
BRIDGE_BOOTSTRAP_SEQNO=<W²-aligned N>    # from compute_bridge_anchors --level 2 (aligned to 16384)
```

Everything else (`BRIDGE_ADDRESS`, `RELAYER_PRIVATE_KEY`,
`BRIDGE_AGGREGATOR_DIR`, etc.) is identical to L1. The `params/` dir
is shared — same K=20/21/17/19 circuit keys work for both levels.

**Cold-start launch:**

```bash
cd crates/an-bridge-prover
mkdir -p state_l2 logs
# state_l2 must be empty for cold start — no `rm state_l2/*.json` needed if fresh dir

set -a && source .env.shellnet.l2 && set +a
TS=$(date +%Y%m%d_%H%M%S)
nohup ./target/release/relayer daemon-live > logs/daemon_l2_${TS}.log 2>&1 &
echo "PID=$!"
```

**Expected log signature (L2 cold-start, first ~20s):**

```
INFO relayer: LiveProverDriver seed policy seed_policy=Explicit(<N>)
INFO relayer: daemon-live startup anchors on_chain_last_seen=<N> driver_last_seen=0
INFO bridge_prover_lib::live_driver: live_driver: bootstrap seed applied — seq_no=<N>, height=<N>, layers=2
INFO bridge_prover_lib::live_driver::bundle: === Processing key block at seq_no <N+1024> ===
```

The **`layers=2`** field is the L2 ground truth. If you see `layers=1`
after sourcing `.env.shellnet.l2`, either the env var isn't reaching the
process or you have stale `state_l2/prover_state.json` from a prior L1
run — see startup-drift note below.

**Startup drift check (L2-specific failure mode).** The daemon refuses
to boot if `prover_state.anchor_level != BRIDGE_ANCHOR_LEVEL`
(bridge-prover-daemon/src/main.rs:115). Rename the offending state dir
(`state_l2 → state_l2.pre_L1_$(date +%s)`) and rebootstrap; never
auto-migrate anchor levels on a live bridge.

Similarly, if `relayer-state.json` in cwd carries a stale
`last_observed_on_chain` from a prior deploy (Deploy #11 anchor after
Deploy #12), the daemon aborts with
`startup on-chain drift vs last_observed_on_chain`. Snapshot the file
(`mv relayer-state.json relayer-state.pre_deploy<N>_<ts>.json`) and
restart — the daemon writes a fresh one on first observation cycle.

**First covering-bundle window (~101 min worst case).** The daemon
processes 15 sub-bundles (Circuit 1a/1b/2 per W·P stride) while the L2
covering bundle lands. On-chain `storedLastSeenBlockSeqNo` does NOT
advance during this window — the verifier only accepts the covering
bundle at seed + W². Monitor via:

```bash
tail -f logs/daemon_l2_*.log | grep -E '(=== Processing|layers=|dumped verifyBlock|confirmed)'
```

The first `verifyBlock confirmed` line arrives at the covering bundle
(seed + 16384). Before that, `dumped verifyBlock submission` entries
are diagnostic-only sub-bundle proofs (unless the daemon has been
extended to submit incremental L2 layer-hash proofs — check
`daemon-live` code path if the contract accepts them).

**All other L2 recovery paths — Cases 2–6 apply as-is.** The daemon's
runtime behavior downstream of `bootstrap seed applied` is anchor-level
opaque. Case 4 drift check, Case 6 re-bootstrap, etc. work identically
against `state_l2/` + `BRIDGE_ANCHOR_LEVEL=2` — just substitute the L2
env-file and state-dir paths.

---

## Health checks (run any time)

**On-chain state snapshot:**

```bash
set -a && source .env.shellnet && set +a
export BRIDGE=$BRIDGE_ADDRESS
export RPC=$RPC_URL
echo "last_seen:         $(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC --json | jq -r '.[0]')"
# num_layers derived from getLatestPerLayer() — highest index with a nonzero hash.
# (Since storage-v2 / commit f8c5ba0, storedNumLayers()/storedLayerHashes(uint256)/getStoredLayerHashes() are gone.)
echo "latest_per_layer:  $(cast call $BRIDGE 'getLatestPerLayer()(uint256[10])' --rpc-url $RPC)"
echo "bk_last_update:    $(cast call $BRIDGE 'storedLastBkSetUpdateSeqNo()(uint64)' --rpc-url $RPC --json | jq -r '.[0]')"
echo "bk_commitment:     $(cast call $BRIDGE 'storedBkSetCommitment()(uint256)' --rpc-url $RPC)"
echo "expectedPrev(1):   $(cast call $BRIDGE 'expectedPrevAnchor(uint8)(uint256)' 1 --rpc-url $RPC)"
echo "storedPrev(genesis): $(cast call $BRIDGE 'storedPrevMaxLevelLayerHash()(uint256)' --rpc-url $RPC)"
# storedPrevMaxLevelLayerHash is `immutable` (constructor-set) — it holds
# the genesis seed forever, not the last-block max-level. The dynamic
# per-block anchor lives in _layerWindows and is folded via
# expectedPrevAnchor(numLayers) at submit time.
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
# [Fund the relayer wallet with Sepolia ETH](#fund-the-relayer-wallet-with-sepolia-eth)
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

**Persistence triggers (see [`../../bridge-relayer-daemon/src/live_source.rs:115,132,210,259`](../../bridge-relayer-daemon/src/live_source.rs)):**

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
  `relayer-state.json.stale_<ts>` first (see [Case 6b](#case-6b--state-loss--re-bootstrap-from-mid-chain)).
- `params/` — never delete; keygen takes ~7 min per circuit.

---

## Change log / known incidents

Newest first. Each entry captures **what happened, why, what changed, and
the reference commits/paths** so we don't have to reconstruct history next
time we come back to the runbook.

### 2026-08-18 — L2 anchoring landed; Case 7 added

- **What.** `BRIDGE_ANCHOR_LEVEL=2` now switches daemon-live to the L2
  covering-bundle stride (`W² = 16384`, ~91 min chain-time on
  shellnet). Runbook grew Case 7 for the L2 cold-start delta; steady-state
  Cases 2-6 apply verbatim (daemon behavior downstream of bootstrap seed
  is anchor-level opaque).
- **Why.** L2 aggregates 16 L1 sub-bundles into one covering proof —
  one-tenth the on-chain submission cadence for the same coverage.
  Contract itself is level-opaque (single `storedPrevMaxLevelLayerHash`
  scalar); the daemon computes the L2 fold before submitting.
- **Genesis anchors.** `compute_bridge_anchors --level 2 --at-head` picks
  a `W²`-aligned seed and folds `layer_hashes[0..2]` into
  `GENESIS_PREV_MAX_LEVEL_LAYER_HASH`. See
  `crates/an-bridge-prover/bridge-prover-lib/src/bin/compute_bridge_anchors.rs`.
- **First live L2 deploy.** Deploy #12 (2026-08-18) —
  `AckiNackiBridge` at `0xf31E316C7E3FD4aDDBd86d6d63a1444947BFFEEE`,
  seed `9175040` (W²-boundary). See
  `logs/deploy12_l2_20260819_001713.log` for the deploy transcript.
- **Startup drift check (new).** `bridge-prover-daemon/src/main.rs:115`
  refuses to boot if `prover_state.anchor_level != BRIDGE_ANCHOR_LEVEL`
  — prevents cross-level state reuse. Rename the state dir and
  rebootstrap; never auto-migrate.
- **Related.** Withdraw runbook Case 8/9 (fresh L2 deploy, sequential
  stress-loop); `ENRICH_TIMEOUT` bumped 90→120 min in
  `bridge-relayer-daemon/src/withdraw_e2e/driver.rs` to cover the L2
  worst-case single-bundle wait (~101 min).

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
- Current active deploy: [see reference table above](#reference-addresses-current-deploy).
  Post-mortem lives in `bridge-deployer.txt` (Deploy #4 section).

---

**Editing this runbook.** When adding a new incident: keep entries dated,
one-paragraph, and always cite the commit / file:line so the next
maintainer can jump straight to the change without spelunking through
`git log`. Old entries can be pruned once the underlying fix has been
proven for >30 days across restarts.
