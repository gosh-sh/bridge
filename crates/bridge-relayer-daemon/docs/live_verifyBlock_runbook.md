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
- [Binary + env prerequisites](#binary--env-prerequisites)
- [Case 1 — First-time bootstrap from a fresh deploy](#case-1--first-time-bootstrap-from-a-fresh-deploy)
- [Case 2 — Steady-state operation](#case-2--steady-state-operation)
- [Case 3 — Clean restart (no state loss)](#case-3--clean-restart-no-state-loss)
- [Case 4 — Restart after RPC-induced hard-abort](#case-4--restart-after-rpc-induced-hard-abort)
- [Case 5 — Restart after on-chain revert](#case-5--restart-after-on-chain-revert)
- [Case 6 — Chain-resurrect (shared contract + fresh daemon)](#case-6--chain-resurrect-shared-contract--fresh-daemon)
- [Health checks (run any time)](#health-checks-run-any-time)
- [File & state reference](#file--state-reference)
- [Change log / known incidents](./live_verifyBlock_changelog.md) *(separate file)*

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
| not running | local < chain | any | [Case 6](#case-6--chain-resurrect-shared-contract--fresh-daemon) — restart; daemon auto-resurrects from chain |
| not running | local > chain | any | [Case 5](#case-5--restart-after-on-chain-revert) — investigate first; daemon will `Stop` on restart |
| running/not | state/ missing | — | [Case 6](#case-6--chain-resurrect-shared-contract--fresh-daemon) — restart; daemon auto-resurrects |

---

## Shared test deploy (open — for quick onboarding)

This is a **deliberately public** Sepolia burner + bridge bundle, deployed
so a third-party developer can `git clone` and run
`./target/release/relayer daemon-live` end-to-end without deploying
anything or funding a wallet.

**When to use it.** One-shot smoke test — confirm the daemon builds,
picks a startup arm, generates a proof, and submits a real `verifyBlock`
tx that lands on Sepolia.

**When NOT to use it.**

- Continuous / multi-day operation.
- Any workflow where two developers run against it concurrently. The
  burner is a single key; concurrent submits from different machines
  collide on nonce and one side reverts.
- Anything you care about not being drained. This key is in-repo —
  Sepolia key-scraper bots harvest it within minutes. Treat any balance
  as ephemeral.

For serious work, use [Deploy your own bridge bundle](#deploy-your-own-bridge-bundle-external-users)
to spin up a private burner + private contract bundle.

### Credentials

| Item | Value |
|---|---|
| Network | Sepolia (chain 11155111) |
| RPC | `https://ethereum-sepolia-rpc.publicnode.com` |
| Burner address | `0xb586356D52eAee055Ca569Ff412DFeFFc5bB2307` |
| Burner private key | `0xb27eec55faeb770f38f86130f4ec9abf901987a68251aade12331fe77f4ecd87` |
| `AckiNackiBridge` | `0xb883Abb563F4Aab0fEd634f5654E1fE332ec3c1c` |
| `PrimaryAggregatorVerifier` | `0xE03337cfC0498a4C40B538A332D18a26df833502` |
| `FallbackAggregatorVerifier` | `0xBe652c88e3d4639Aa11390a878F0da4B4a91fBb8` |
| `LayerHashesAggregatorVerifier` | `0x149f61E049B7a325d720cbF56D102E7376d38643` |
| `BridgeWithdrawalAggregatorVerifier` | `0x4127E8c7F3C6308eD836CBc819aA556868AfD139` |
| `MockBlockHeaderOracle` | `0xB7ad2342F8Fe665435Fe934757AA4f208ccf2417` |
| Bootstrap seed seq_no | `7726080` |
| Genesis `bk_set_commitment` | `0x08eb0a1892e4f75a8b5c8cff69322f95bf0437c371903998c9365fbe293ca71c` |
| Genesis `prev_max_level_layer_hash` | `0x265511da2029a440b78947e07b9f57fb59e234e0845e9e3b1acc8f41e5aca507` |

### Wiring the daemon (no deploy needed)

```bash
cd crates/an-bridge-prover
cat > .env.shellnet <<'EOF'
RPC_URL=https://ethereum-sepolia-rpc.publicnode.com
BRIDGE_ADDRESS=0xb883Abb563F4Aab0fEd634f5654E1fE332ec3c1c
RELAYER_PRIVATE_KEY=0xb27eec55faeb770f38f86130f4ec9abf901987a68251aade12331fe77f4ecd87
BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql
BRIDGE_BOOTSTRAP_SEQNO=7726080
BRIDGE_BK_SET_CONFIG=./bk_set.shellnet.json
BRIDGE_PARAMS_DIR=./params
BRIDGE_STATE_DIR=./state
BRIDGE_AGGREGATOR_DIR=../bridge-evm-aggregator
BRIDGE_VERIFIERS_DIR=../../contracts/ethereum/verifiers
EOF
```

Then jump straight to [Binary + env prerequisites](#binary--env-prerequisites)
and [Case 6 — Chain-resurrect](#case-6--chain-resurrect-shared-contract--fresh-daemon)
(the contract already has history; a fresh daemon will auto-resurrect
from chain — no local bootstrap needed).

**Balance check before launch:**

```bash
cast balance 0xb586356D52eAee055Ca569Ff412DFeFFc5bB2307 \
  --rpc-url https://ethereum-sepolia-rpc.publicnode.com --ether
```

If `< 0.005 ETH`, one of the faucets under
[Deploy your own bridge bundle §2](#2-fund-it-with-sepolia-eth) will
top it back up (pk910 PoW is public-goods and does not require you to
own the address).

### Recommended server-side path (headless Linux)

**Best strategy: shared *contract*, your own *key*.** `verifyBlock` is
permissionless (`external nonReentrant`, no role gate) — anyone with a
funded Sepolia burner and a valid proof can submit against
`0xb883Abb563F4Aab0fEd634f5654E1fE332ec3c1c`. Using your own key avoids
the two problems of the shared burner: nonce collisions when several
devs run concurrently, and getting drained by faucet-scraper bots.
Reserve the in-repo burner for a one-shot "does my clone build and
land a tx" smoke test.

```bash
# 1. Clone + build (~15 min)
git clone https://github.com/gosh-sh/bridge.git && cd bridge
git checkout deposit_proof_fixes
cd crates/an-bridge-prover
cargo build --release -p bridge-relayer-daemon --bin relayer
(cd ../bridge-evm-aggregator && cargo build --release)

# 2. Provision params/ (~30 min one-time). See TECHNICAL_README.md
#    "KZG SRS provisioning"; or rsync ~17 GB from a teammate.

# 3. Wire env — shared contract, YOUR key (fund via API-key faucet:
#    Alchemy / Infura / QuickNode all work headlessly).
cat > .env.shellnet <<'EOF'
RPC_URL=https://ethereum-sepolia-rpc.publicnode.com
BRIDGE_ADDRESS=0xb883Abb563F4Aab0fEd634f5654E1fE332ec3c1c
RELAYER_PRIVATE_KEY=<your funded Sepolia burner>
BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql
BRIDGE_BOOTSTRAP_SEQNO=7726080
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

**Expected first-launch signature** — until the first verifyBlock
lands, contract has `last_seen=7726080` but empty layer windows;
`decide()` routes this "post-deploy transitional" state to Cold, not
Resurrect:

```
startup: Cold — contract at genesis, bootstrapping ...
LiveProverDriver seed policy seed_policy=Explicit(7726080)
```

After someone lands the first submit, later fresh launches will see
`startup: Resurrect …` + `seed_policy=Resume` instead — both healthy
(Case 1 and Case 6).

**~15 min later**, the burner should have paid for its first Sepolia
tx and the contract cursor should advance from `7726080` → `7727104`:

```bash
cast call 0xb883Abb563F4Aab0fEd634f5654E1fE332ec3c1c \
  'storedLastSeenBlockSeqNo()(uint64)' \
  --rpc-url https://ethereum-sepolia-rpc.publicnode.com --json | jq -r '.[0]'
```

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
fund**. `verifyBlock` (`AckiNackiBridge.sol:650`) is a
state-mutating `external nonReentrant` function; every relayer submit is a
signed Sepolia transaction. The daemon reads its signer from the
`RELAYER_PRIVATE_KEY` env var (`bridge-relayer-daemon/src/bin/relayer.rs:86`);
there is no default. The [shared test deploy](#shared-test-deploy-open--for-quick-onboarding)
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
submit (one per
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
```

Now jump to [Binary + env prerequisites](#binary--env-prerequisites) and
[Case 1 — First-time bootstrap from a fresh deploy](#case-1--first-time-bootstrap-from-a-fresh-deploy).

---

*Maintainer-only note:* Alina keeps her personal burner + per-deploy
change history + genesis-anchor paper trail in
`~/HALO2_TVM_EXPERIMENTS/bridge-deployer.txt` — off-tree, not shipped,
not linked from this doc.

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
INFO relayer: LiveProverDriver seed policy seed_policy=Explicit(<BRIDGE_BOOTSTRAP_SEQNO>)   ← cold-start signature
```

**How the daemon picks its startup path.** Since 2026-08-12, the daemon
reads the *full* on-chain state (four scalars + all 10 `_layerWindows`
via `getLayerWindow(uint8)`) at startup and routes through one of four
arms via `startup_decide::decide()` (`bridge-relayer-daemon/src/startup_decide.rs`).
Inputs: local `state/prover_state.json` + on-chain `ContractFullState` +
`BRIDGE_BOOTSTRAP_SEQNO`. `seed_policy` (`Resume` / `Explicit(N)` / `Auto`)
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
multi-tester scenario (see [Case 6](#case-6--chain-resurrect-shared-contract--fresh-daemon))
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
restart, `state/prover_state.json` is missing — go to [Case 6](#case-6--chain-resurrect-shared-contract--fresh-daemon).

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
| `0x87bf1c06` | `AttestationProofRejected()` | Adapter equality check on a public input failed. **Bug class: BN254 Fr canonicalization** — if this fires on `blockId`, the client fix in `bridge-relayer-daemon/src/types.rs:83` (`U256::from_be_bytes(b.block_id_be) % BN254_FR_MODULUS`) is missing/reverted. See [changelog — 2026-08-03 BN254 Fr fix](./live_verifyBlock_changelog.md#2026-08-03--bn254-fr-canonicalization-client-fix). |
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

## Case 6 — Chain-resurrect (shared contract + fresh daemon)

**When it fires.** Any startup where the on-chain contract has history
(`storedLastSeenBlockSeqNo > 0`) but the local `state/prover_state.json`
is either absent, `initialized=false`, or has `stored_last_seen_block_seq_no`
strictly less than chain. This is the **default** operating case for
multi-tester development against a shared bridge, and for fresh checkouts
on a new machine that need to catch up with an already-advanced deploy.

**No manual bootstrap needed** — the daemon does it automatically. Since
2026-08-12, on startup the daemon:

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
nohup ./target/release/relayer daemon-live > logs/live_$(date +%Y%m%d_%H%M%S).log 2>&1 &
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

- `local_last_seen > chain_last_seen` — local state is *ahead* of
  chain. Means either a reorg, a redeployment the operator hasn't
  acknowledged, or a state file copied from another environment. Never
  silently rewound.
- `local_last_seen == chain_last_seen` but `stored_bk_set_commitment`
  or `stored_last_bk_set_update_seq_no` diverges — same cursor,
  different state (different verifier wrote local, or state drifted).
- `local_last_seen == chain_last_seen` but per-layer windows diverge
  byte-for-byte — cursor match with inconsistent history mirror.
- `window_size` mismatch — contract deployed with a different
  `HISTORY_PROOF_WINDOW` than daemon's `HISTORY_WINDOW_SIZE`.

Each `Stop` message carries the diagnostic. Operator inspects, then
either points at the intended contract, archives the local state
(`mv state state.stale_$(date +%Y%m%d_%H%M%S)` — resurrect will then
rebuild from chain), or rolls back to a matching contract.

**Destructive re-bootstrap (rarely needed).** Only when the contract
itself has been redeployed at a *different address* and the operator
wants to bootstrap into it from a specific seed:

```bash
# 1. Confirm you're pointing at the intended contract:
CURRENT=$(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC --json | jq -r '.[0]')
echo "chain last_seen = $CURRENT"      # if > 0, resurrect will handle it

# 2. Only if the contract is at genesis (last_seen == 0) AND you need a
#    non-default seed: set BRIDGE_BOOTSTRAP_SEQNO in .env.shellnet, then
#    launch. This exercises the Cold arm, not the Resurrect arm.
grep '^BRIDGE_BOOTSTRAP_SEQNO=' .env.shellnet     # must match your intended seed
# Optional sanity: regenerate genesis anchors
cd ../bridge-prover-lib && \
  cargo run --release --bin compute_bridge_anchors -- \
    --seed-seqno $BRIDGE_BOOTSTRAP_SEQNO \
    --gql-endpoint https://shellnet.ackinacki.org/graphql && \
  cd ../an-bridge-prover
```

See the changelog's
[2026-08-03 chicken-and-egg fix](./live_verifyBlock_changelog.md#2026-08-03--deploy-4-storedlastseenblockseqno0-chicken-and-egg-fix)
for why the constructor now takes `genesisLastSeenBlockSeqNo`.

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
  `relayer-state.json.stale_<ts>` first (see [Case 6](#case-6--chain-resurrect-shared-contract--fresh-daemon)).
- `params/` — never delete; keygen takes ~7 min per circuit.

---

## Change log / known incidents

Moved to [`live_verifyBlock_changelog.md`](./live_verifyBlock_changelog.md)
to keep this runbook focused on operational procedures. The changelog
covers dated post-mortems (BN254 Fr canonicalization, RPC hard-abort
false-positive, Deploy #4/#5 constructor + storage changes) with the
commit/file:line cites needed to jump straight to each fix.
