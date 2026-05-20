# Manual Verification Runbook (v2)

> **v2 update (2026-05-10).** Phase D, F, G, and Attack Groups 4 / 5 of Phase J have been
> rewritten for the four-circuit architecture. The legacy `LayerHashBridge.sol` walkthrough
> (Phase F v1) and the dual ZK + timelock BK-rotation flows (Phase G v1) were retired in
> Phase 4.2; their successors are `verifyBlock` (Phase F v2) and Phase 1.C placeholder
> (Phase G v2). 8 new CC-# attack scenarios were added; 5 legacy LH-/BK-specific scenarios
> were retired.
>
> **v2.1 update (2026-05-17, Phase 4.3).** The legacy refund-style `withdraw(...)` was
> retired together with the ETH-side `Groth16DepositVerifier` chain. Effect on this runbook:
>
> - Phase E ("Anvil ETH→AN: Deposit/withdraw + 4 deliberate failures") is now Anvil-deposit-
>   only (no `withdraw`); the withdrawal-side rejection scenarios moved to AN-side
>   responsibilities (covered in `docs/verifying_eth_proof_on_an.md`).
> - Phase J attack scenarios that targeted `withdraw()` are **historical** — J.1, J.2, J.4,
>   J.5, J.6, J.21, J.22 are kept for documentation continuity but each is prefixed with
>   "**[Retired — Phase 4.3]**". The functions they probed no longer exist; the underlying
>   threat model is now AN-side (`VERHALO2SHPLONK` opcode + AN-side `TokenBridge`).
> - Test counts: 135 → **109 across 12 suites**. Test-suite table updated below.
> - The Sign-Off Checklist (Phase K) is updated to reflect the post-demolition surface.
>
> **v2.3 update (2026-05-20).** Phase E gains two new subsections:
> **E.6 — long-running relayer daemon** covers the `relayer daemon`
> subcommand (exponential backoff, signal-aware shutdown, metrics
> snapshot, optional sentry guard); **E.7 — `verify-fixture`** is a
> read-only pre-flight check operators run *before* the daemon to
> confirm anchors line up between the fixture and the live bridge.
> Production deployments should drive `verifyBlock` through the daemon
> entry point rather than `smoke-fixture`.
>
> **v2.2 update (2026-05-18).** Three small refreshes:
>
> - Foundry total now **132 across 14 suites** (109 → 125 was Circuit 4 Phase A
>   scaffolding + a new `AckiNackiBridgeVerifyEventTest` suite; 125 → 132 adds
>   `FuzzAckiNackiBridgeVerifyBlockTest`, 6 property-based fuzz tests + 1
>   plain unit test (the genesis-seqNo case has a single valid input and
>   was demoted from fuzz after pipeline #5741 tripped the 65 536-reject
>   cap) covering
>   `VerifyBlockDisabled`/`InvalidNumLayers`/`LayerHashTailNonZero`/
>   `BkSetCommitmentMismatch`/`BlockSeqNoNotMonotonic`/`PrevAnchorMismatch`).
>   Phase C test-suite table updated.
> - Stale test-name references throughout this runbook (`testHappyPathPrimary`,
>   `testRevertOnPrevAnchorMismatch`, etc.) were never renamed when the Phase 4.1
>   verifyBlock suite landed; replaced with the actual names below
>   (`test_verifyBlock_primary_bound_succeeds_andUpdatesState`,
>   `test_verifyBlock_prevAnchorMismatch_reverts`, etc.).
> - `crates/bridge-prover-orchestrator/proofs/bound/` now contains a fallback
>   sub-directory (Circuit 1B) alongside primary/layer-hashes — the
>   `export-bound-block-proofs` bin generates all three. Phase D updated.

A hands-on, copy-pasteable plan for a single human reviewer to verify the bridge **works correctly and is attack-resistant** from the outside. No prior knowledge of the codebase is assumed.

**Time budget**: ~2.5 hours for a thorough first pass, ~5 min for follow-up sanity checks after a change.

This is the procedural twin of `docs/bridge_verification.md` (which states *what* must hold). Here you actually run things and tick boxes.

## Phase Overview

| § | Phase | Duration | Purpose |
|---|---|---|---|
| 0 | Prerequisites | 5 min | Tooling check |
| 1 | A — Static Inspection | 15 min | Read the contracts |
| 2 | B — Build | 3 min | Compile clean |
| 3 | C — Automated Tests | 1 min | 109/109 green |
| 4 | D — Single-Block Bound Real-Proof | 1 min | Real Groth16 tuple verifies on-chain via `verifyBlock` |
| 5 | E — Anvil ETH→AN | 15 min | Deposit-only flow + 2 deliberate failures (withdraw retired in Phase 4.3) |
| 6 | F — `verifyBlock` Walk-Through | 15 min | Walk the cross-circuit proof tuple + anchors |
| 7 | G — BK Rotation (Phase 1.C, pending) | 5 min | Confirm `storedBkSetCommitment` is immutable today |
| 8 | H — AAVE Yield | 20 min | Solvency + owner-can't-steal |
| 9 | I — Oracle | 10 min | Recent + (optional) fork |
| **10** | **J — Attack Scenarios** | **30 min** | **Try ≥ 30 attacks; all blocked** |
| 11 | K — Final Sign-Off Checklist | 5 min | Tickbox gate |
| 12 | Troubleshooting | as needed | Common symptoms |
| 13 | After This Runbook | — | Where to go next |

---

## 0. Prerequisites

Before starting, confirm you have:

```bash
forge --version    # foundry 1.x or later
anvil --version    # ships with foundry
cast  --version    # ships with foundry
cargo --version    # rustc 1.75+ recommended
go    version      # 1.21+ (only needed for gnark wrapping; can skip for runbook)
git   --version
jq    --version    # optional, makes JSON output readable
```

If any are missing:

```bash
# Foundry
curl -L https://foundry.paradigm.xyz | bash && foundryup

# Rust toolchain (if not present)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# JQ
sudo apt install jq      # Debian/Ubuntu
```

Clone and enter the repo:

```bash
git clone https://github.com/...acki-nacki-bridge.git
cd acki-nacki-bridge
```

---

## 1. Phase A — Static Inspection (≈ 15 min)

**Goal**: get oriented before running anything.

### A.1 Walk the repo layout

```bash
ls -F
```

You should see (relevant parts):

```
contracts/ethereum/                 ← Solidity (Foundry)
crates/                             ← Rust workspace
  bridge-prover-orchestrator/       ← Halo2 prover orchestrator + per-circuit gnark wrappers (1A/1B/2)
  bridge-relayer-daemon/            ← Relayer daemon (Phase 5.1, mock sources)
  eth-frontend/, acki-nacki-interface/ ← thin clients
deposit-prover/                     ← Halo2 circuit + gnark wrapper for ETH→AN deposit
docs/                               ← Architecture + audits + runbooks (this file)
Makefile                            ← Entry point
AGENTS.md                           ← One-page project context
```

The legacy `layer-hashes-prover/` and `bk-set-rotation-prover/` directories were retired in
Phase 4.2 (2026-05-10). Their successors live under `crates/bridge-prover-orchestrator/`.

### A.2 Read the one-page summary

```bash
sed -n '1,60p' AGENTS.md
```

You should learn: it's an Ethereum↔Acki Nacki bridge using ZK proofs. The single on-chain trust anchor is **`AckiNackiBridge.sol`**: custodies user ETH (with optional AAVE yield) AND anchors AN state via the `verifyBlock` surface (Phase 4.2 folded the legacy `LayerHashBridge.sol` into this one contract).

### A.3 Spot-check the critical contracts

Open and skim each in this order. Look for the listed properties.

#### `contracts/ethereum/src/AckiNackiBridge.sol`

Confirm by inspection:

- [ ] `deposit()` only does `treasuryBalance += msg.value` and emits `Deposit(...)`. **No AAVE call on the user path.**
- [ ] There is **no** `function withdraw(...)` and **no** `import "./IAckiNackiVerifier.sol";` — both retired in Phase 4.3 (2026-05-17). Confirm via `! grep -E "function withdraw\(|IAckiNackiVerifier" contracts/ethereum/src/AckiNackiBridge.sol`.
- [ ] `MAX_DEPOSIT_AMOUNT = 100 ether`.
- [ ] The constructor takes `(blockHeaderOracle, aavePool, wethGateway, aWETH, …)` — the legacy `_verifier` parameter is gone (Phase 4.3).

#### `contracts/ethereum/src/AckiNackiBridge.sol::verifyBlock` (the AN→ETH path)

- [ ] `verifyBlock` reverts with `VerifyBlockDisabled` if any of `primaryVerifier`, `fallbackVerifier`, `layerHashesVerifier` is `address(0)` (LH-9).
- [ ] `verifyBlock` reverts with `InvalidNumLayers(numLayers)` if `numLayers == 0 || numLayers > MAX_LAYER_HASHES` (i.e. 1..10) (LH-4).
- [ ] `verifyBlock` reverts with `LayerHashTailNonZero(i)` for any `layerHashes[i] != 0` where `i ≥ numLayers` (LH-5 / CC-7).
- [ ] Chain anchor: `prevMaxLevelLayerHash != storedPrevMaxLevelLayerHash ⇒ revert PrevAnchorMismatch` (LH-3 / CC-6).
- [ ] BK-set check: `bkSetCommitment != storedBkSetCommitment ⇒ revert BkSetCommitmentMismatch` (LH-2 / CC-3).
- [ ] Strict monotonicity: `blockSeqNo <= storedLastSeenBlockSeqNo ⇒ revert BlockSeqNoNotMonotonic` (LH-6 / CC-5).
- [ ] All three verifier addresses are declared `immutable` — there is no setter post-deployment (AC-5).
- [ ] After both gnark verifications return `true`, the storage writes precede the `BlockVerified` event emission (CEI: AC-6).

#### `contracts/ethereum/src/AxiomBlockHeaderOracle.sol`

- [ ] For blocks within last 256, returns `blockhash(blockNumber)` (native EVM opcode).
- [ ] For older blocks, reverts and forces use of `verifyBlockHash(...)` with an Axiom witness.
- [ ] Future blocks revert with `BlockNotYetMined`.

If any of these checks fail, **stop here and investigate** — the rest of the runbook assumes they hold.

---

## 2. Phase B — Build (≈ 3 min)

```bash
cd contracts/ethereum
forge build 2>&1 | tail -3
```

✅ Expected:

```
... no error lines ...
```

(Pre-existing lint warnings about `unsafe-typecast` and `mixed-case-variable` are OK — they come from auto-generated verifier code.)

```bash
forge fmt --check
```

✅ Expected: exits 0 with no output. If it prints diffs, code is unformatted — run `forge fmt`.

```bash
cd ../..
cargo build --workspace 2>&1 | tail -3
```

✅ Expected: `Finished ...` with no error lines.

---

## 3. Phase C — Automated Tests (≈ 1 min)

This is the biggest single-step assurance you'll get. **If anything in this phase fails, stop and investigate.**

```bash
cd contracts/ethereum
forge test 2>&1 | tail -20
```

✅ Expected (final two lines):

```
Ran 15 test suites in ...: 132 tests passed, 0 failed, 1 skipped (133 total tests)
```

The 1 skipped test belongs to `AckiNackiBridgeAaveFork.t.sol`, which
auto-skips unless `FORK_URL` points at a mainnet RPC (see Phase H for the
opt-in flow). The remaining 14 suites all pass.

Per-suite expectation (post-Phase 4.3 + Phase A Circuit 4 scaffolding + v2.2 fuzz;
canonical breakdown maintained in `AGENTS.md`):

| Suite | Tests |
|---|---|
| `AckiNackiBridgeAaveTest` | 20 |
| `AckiNackiBridgeVerifyBlockTest` (Phase 4 AN→ETH, real bound 1A+2 proofs) | 17 |
| `FuzzAckiNackiBridgeVerifyBlockTest` (Phase 4 input invariants, 6 fuzz × 256 + 1 unit) | 7 |
| `AckiNackiBridgeRelayerLoopTest` (Phase 5.1 — 10-block loop, mock verifiers) | 6 |
| `AckiNackiBridgeVerifyEventTest` (Phase A Circuit 4 — layerWindow + verifyEvent) | 16 |
| `AxiomBlockHeaderOracleTest` | 16 |
| `Blake2bHalo2VerifierTest` | 7 |
| `KeccakHalo2VerifierTest` | 1 |
| `Halo2PoseidonVerifierTest` | 7 |
| `FuzzAckiNackiBridgeDepositTest` | 3 |
| `FuzzHalo2VerifierTest` | 6 |
| `PrimaryVerifierTest` (Circuit 1A, real gnark proof) | 8 |
| `FallbackVerifierTest` (Circuit 1B, real gnark proof) | 8 |
| `LayerHashesMovementVerifierTest` (Circuit 2, real gnark proof) | 10 |
| **Total** | **132** |

```bash
forge test --gas-report 2>&1 | grep -E "AckiNackiBridge|verifyBlock|AAVE" | head -10
```

Spot the headline numbers:

- `AckiNackiBridge.deposit` ≈ 90k gas (no AAVE call).
- `AckiNackiBridge.verifyBlock` ≈ 440k gas (two pairings + storage updates).
- (Legacy `Groth16DepositVerifier.verify` ≈ 287k gas was retired in Phase 4.3; deposit-proof verification now happens natively on the AN side via `VERHALO2SHPLONK`.)

---

## 4. Phase D — Single-Block Bound Real-Proof (≈ 1 min)

This is the **smoking gun** test for v2: a tuple of real bound Groth16 proofs (Circuit 1A + Circuit 2) generated by `crates/bridge-prover-orchestrator`, verified on-chain through `AckiNackiBridge.verifyBlock`.

```bash
cd contracts/ethereum
forge test --match-test "test_verifyBlock_primary_bound_succeeds|test_verifyBlock_fallback_routesToFallbackVerifier" -vv 2>&1 | tail -25
```

✅ Expected: both tests pass. Look for:

- `BlockVerified(blockId=…, blockSeqNo=…, finType=0 [Primary], numLayers=…)` event in the trace.
- ~440k gas total (two pairing checks + storage updates).
- `storedLastSeenBlockSeqNo`, `storedNumLayers`, `storedPrevMaxLevelLayerHash` advance after the call.

To regenerate the bound proofs from scratch (~30-45 min as of v2.2 — keygen + Halo2 prove + gnark wrap for all three circuits 1A, 1B, 2):

```bash
cd ../..
cargo run -p bridge-prover-orchestrator --bin export-bound-block-proofs --release
ls -la crates/bridge-prover-orchestrator/proofs/bound/{primary,fallback,layer-hashes}/
cat crates/bridge-prover-orchestrator/proofs/bound/bound_scenario.json | jq .
```

✅ Expected: under `crates/bridge-prover-orchestrator/proofs/bound/`:

- `bound_scenario.json` with `block_id`, `bk_set_poseidon`, `num_layers`,
  `layer_hash_decimals[]`, `prev_max_level_layer_hash_decimal`,
  and (since v2.2) `primary_proof_bytes`, `fallback_proof_bytes`,
  `layer_hashes_proof_bytes` — Circuit 1A and 1B share the same Public
  Inputs by construction; the JSON keys reflect this.
- `primary/`, `fallback/`, `layer-hashes/` sub-directories each holding a
  raw Halo2 `proof.bin` + `instances.bin` + gnark `halo2_proof.json`.

The third circuit (Fallback, Circuit 1B) is generated even though the
Foundry happy-path test for Phase D wires the Fallback verifier to a
`MockFallbackVerifier` — having all three artifacts cross-checked against
each other inside the orchestrator is how we keep cross-circuit consistency
(shared `block_id` + `bk_set_poseidon`) honest. Real-proof Fallback
on-chain coverage lives in `FallbackVerifierTest` independently.

Multi-block real-proof coverage (the legacy `LayerHashE2ETest` analogue with 4 fixtures) is **deferred to Phase 5.3** (10-block shellnet end-to-end against Anvil). At HEAD, only the single-block bound case is exercised with real proofs.

---

## 5. Phase E — Hands-On Anvil Session: ETH → AN Path (≈ 30 min)

Now drive the bridge yourself with a local Ethereum node.

### E.1 Start a local node

In **terminal 1**:

```bash
anvil --block-time 2
```

Leave it running. Note the prefunded accounts at the top — copy account 0's private key (`0xac0974...`) and address (`0xf39F...`).

In **terminal 2**:

```bash
export RPC=http://127.0.0.1:8545
export PK=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
export ME=0xf39Fd6e51aad88F6F4ce6aB8827279cfFFb92266
```

### E.2 Deploy a test bridge

```bash
cd contracts/ethereum
PRIVATE_KEY=$PK forge script script/DeployTestBridge.s.sol \
  --rpc-url $RPC --broadcast 2>&1 | tee /tmp/deploy.log
```

Pick out the addresses:

```bash
BRIDGE=$(grep "AckiNackiBridge deployed at:" /tmp/deploy.log | awk '{print $NF}')
ORACLE=$(grep "MockBlockHeaderOracle deployed at:" /tmp/deploy.log | awk '{print $NF}')
echo "Bridge: $BRIDGE"
echo "Oracle: $ORACLE"
```

Sanity-check it's wired (legacy `verifier()` getter is gone — Phase 4.3):

```bash
cast call $BRIDGE "blockHeaderOracle()(address)"   # → $ORACLE
cast call $BRIDGE "treasuryBalance()(uint256)"     # → 0
cast call $BRIDGE "depositCounter()(uint256)"      # → 0
cast call $BRIDGE "MAX_DEPOSIT_AMOUNT()(uint256)"  # → 100000000000000000000
```

### E.3 Make a deposit

```bash
cast send $BRIDGE "deposit()" --value 1ether --rpc-url $RPC --private-key $PK
```

✅ Expected: a transaction receipt with `status 0x1`. Now verify state:

```bash
cast call $BRIDGE "depositCounter()(uint256)"           # → 1
cast call $BRIDGE "treasuryBalance()(uint256)"          # → 1000000000000000000
cast balance $BRIDGE                                    # → 1000000000000000000
```

Inspect the event:

```bash
cast logs --address $BRIDGE --rpc-url $RPC | head -20
```

You should see a log with topic `Deposit(uint256,address,uint256,uint256)` and the indexed `depositId=0`, `sender=$ME`.

### E.4 Try a zero-value deposit — must fail

```bash
cast send $BRIDGE "deposit()" --value 0 --rpc-url $RPC --private-key $PK
```

✅ Expected: revert with `InvalidAmount()` selector. Confirms DEP-2 (amount range check).

### E.5 Try an over-cap deposit — must fail

```bash
cast send $BRIDGE "deposit()" --value 101ether --rpc-url $RPC --private-key $PK
```

✅ Expected: revert with `InvalidAmount()` selector. Confirms the upper bound `MAX_DEPOSIT_AMOUNT = 100 ether`.

**Phase E checkpoint** — at this point you have empirically verified:

- Deposits emit events and update treasury accounting (DEP-1, DEP-3).
- Zero-value and over-cap deposits are rejected (DEP-2).

> **Retired in Phase 4.3 (2026-05-17)**: the legacy `withdraw(recipient, amount, depositId, blockNumber, proof)` cast probes (former E.4–E.8), the `isDepositProcessed(...)` view, `TestDepositVerifier` deployment, and the `verifier()` getter are no longer applicable — the legacy refund-style withdrawal mechanism was removed. The equivalent acceptance / rejection scenarios moved to the AN-side `TokenBridge.finalizeDeposit(...)` path, exercised via `tvm-sdk` tests once `VERHALO2SHPLONK` ships (see `docs/verifying_eth_proof_on_an.md` §2).

### E.6 Drive `verifyBlock` via the long-running relayer daemon

The AN→ETH side is exercised by `crates/bridge-relayer-daemon`. For
Phase D you already saw the one-shot `smoke-fixture` subcommand; the
operator entry point is the new `daemon` subcommand (B5, 2026-05-20):

```bash
# Build once. The relayer is excluded from the workspace (own Cargo.lock,
# own alloy + clap deps), so it has dedicated CI jobs and is built per-crate.
cd crates/bridge-relayer-daemon
cargo build --release --bin relayer

# (Re-)deploy a wired bridge so verifyBlock isn't disabled. The deploy
# script also prints the genesis BK-set commitment that you'll feed back
# into the relayer's fixture directory. See `script/DeployRealBridge.s.sol`.
cd ../../contracts/ethereum
WIRE_VERIFY_BLOCK=true \
  GENESIS_BK_SET_COMMITMENT=$(cat ../../crates/bridge-prover-orchestrator/proofs/bound/bound_scenario.json \
                              | jq -r '.bk_set_poseidon_decimal') \
  GENESIS_PREV_MAX_LEVEL_LAYER_HASH=$(cat ../../crates/bridge-prover-orchestrator/proofs/bound/bound_scenario.json \
                                       | jq -r '.prev_max_level_layer_hash_decimal') \
  PRIVATE_KEY=$PK \
  forge script script/DeployRealBridge.s.sol --rpc-url $RPC --broadcast | grep "deployed at:"

# Read the wired bridge address from `deployment_real.json`.
BRIDGE=$(jq -r .bridge deployment_real.json)

# Start the daemon. It will submit the one bound fixture (Verified),
# then back off exponentially (NotYetAvailable on every subsequent tick).
cd ../../crates/bridge-relayer-daemon
RUST_LOG=info ./target/release/relayer daemon \
    --fixtures-dir ../bridge-prover-orchestrator/proofs/bound \
    --rpc-url $RPC \
    --bridge-address $BRIDGE \
    --private-key $PK \
    --backoff-initial-secs 1 \
    --backoff-max-secs 8 \
    --backoff-multiplier 2
```

✅ Expected log shape (truncated):

```
INFO daemon starting backoff=BackoffConfig { initial: 1s, max: 8s, multiplier: 2 } an_node_url=None
WARN running without sentry — a live BK rotation will NOT pause this daemon
INFO daemon: verified seq_no=1
INFO daemon: tick ... seq_no=2 status=NotYetAvailable          # backoff = 1s
INFO daemon: tick ... seq_no=2 status=NotYetAvailable          # backoff = 2s
INFO daemon: tick ... seq_no=2 status=NotYetAvailable          # backoff = 4s
INFO daemon: tick ... seq_no=2 status=NotYetAvailable          # backoff = 8s (capped)
...
```

Hit `Ctrl-C` at any time. You should observe:

- An immediate `INFO SIGINT received` log line.
- The daemon flushes `state.json` and prints `daemon stopped` with the
  final `DaemonRunSummary` and `RelayerMetricsSnapshot`.
- `cat ./relayer-state.json` shows `last_processed_seqno = 1` and
  `attempts_since_progress` matches the number of post-Verified ticks.

To exercise the sentry-guarded path, add `--an-node-url
http://94.156.178.19:8600` (or your own AN-node URL). The daemon will
then run inside a `SentryGuardedRelayer`; if a live BK rotation lands
during the run you'll see a `daemon[guarded]: rotation detected …
relayer is now paused, awaiting Phase 5.2 reconcile` warning and no
further verifyBlock submissions until you restart (Phase 5.2 will wire
`resume()` into the Circuit 3 rotation pipeline).

**Phase E.6 checkpoint** — empirically verified:

- The daemon honours `--backoff-{initial,max}-secs` + `--backoff-multiplier`.
- `Ctrl-C` triggers a clean shutdown at the next sleep boundary (no
  half-finished transactions, `state.json` consistent on disk).
- Metrics counters (`ticks_total`, `verified_total`,
  `not_yet_available_total`, `current_backoff_secs`,
  `last_verified_seq_no`) are emitted in the final `daemon stopped`
  log line.

### E.7 Pre-flight: `verify-fixture` (no private key needed)

Before letting the daemon submit anything to a freshly deployed bridge,
operators should run the **read-only** `verify-fixture` subcommand. It
loads the same fixture the daemon would consume, reads the on-chain
anchors over HTTP, **and** by default `eth_call`-simulates the full
`verifyBlock(...)` so bad proofs surface in pre-flight too. No private
key, no transaction, no gas — suitable for pre-deploy CI.

```bash
cd crates/bridge-relayer-daemon
RUST_LOG=info ./target/release/relayer verify-fixture \
    --fixtures-dir ../bridge-prover-orchestrator/proofs/bound \
    --rpc-url $RPC \
    --bridge-address $BRIDGE
echo "exit=$?"
```

✅ Expected on a freshly deployed wired bridge with valid fixtures:

```
INFO on-chain state read … last_seen=0
INFO fixture loaded fixture_seq_no=1 fixture_block_id=… fixture_num_layers=…
INFO BkSetCommitment matches on-chain
INFO seqNo is strictly greater than last_seen
INFO PrevAnchor matches on-chain
INFO verify-fixture: cheap anchor checks PASS
INFO verify-fixture: running eth_call simulation of verifyBlock(...) — …
INFO verify-fixture: eth_call simulation PASS; a real submit at the current head would verify
exit=0
```

What it catches (per-field diagnostics + `exit=1`):

- **Wrong network** — `read_state` returns garbage zeros and the
  BkSetCommitment line says `MISMATCH: fixture = 0xabc…, on-chain = 0x0`.
- **Stale fixture** — re-submitting after a successful daemon run gives
  `BlockSeqNo NOT MONOTONIC: fixture seqNo = 1, on-chain last_seen = 1`.
- **Mis-paired fixture** — hot-swapping to one generated against a
  different BK set: `BkSetCommitment MISMATCH`.
- **Bad ZK proof** — the eth_call surfaces the verifier triple's revert
  as `eth_call simulation REVERTED: …AttestationProofRejected…` or
  `…LayerHashesProofRejected…` (the help text enumerates all common
  selectors). This used to only manifest as a real `daemon`-submit
  `Reverted` outcome; pre-flight now catches it without burning a tx.
- **Disabled bridge** — eth_call returns `VerifyBlockDisabled` if the
  bridge was deployed without `WIRE_VERIFY_BLOCK=true`.

Pass `--no-simulate` to skip the eth_call step (drops back to the cheap
anchor checks only). Useful when the operator wants a sub-second
pre-flight and trusts the proof generation pipeline (e.g. when the
fixture was just regenerated against a known-good VK).

---

## 6. Phase F — `verifyBlock` Walk-Through (≈ 15 min)

The Solidity tests already exercise this with real proofs (Phase D) and a 10-block mock loop (Phase C → `AckiNackiBridgeRelayerLoopTest`). It's worth doing one round by hand to feel the cross-circuit binding and the state machine.

### F.1 Read the bound proof tuple

```bash
cd ../..
cat crates/bridge-prover-orchestrator/proofs/bound/bound_scenario.json | jq .
```

You should see fields like:

```json
{
  "block_id_hex": "0x...",
  "bk_set_poseidon_hex": "0x...",
  "block_seq_no": 12345,
  "last_seen_block_seq_no": 12340,
  "num_layers": 3,
  "layer_hashes_hex": ["0x...", "0x...", "0x...", "0x0", ..., "0x0"],
  "prev_max_level_layer_hash_hex": "0x...",
  "primary_attestation_proof_hex": "0x..." (256 B),
  "layer_hashes_proof_hex": "0x..." (256 B)
}
```

Both proofs commit to the same `block_id` and `bk_set_poseidon` by construction (CC-1, CC-2). This is what `AckiNackiBridgeVerifyBlockTest` feeds into `AckiNackiBridge.verifyBlock`.

### F.2 Watch the tuple verify in slow motion

```bash
cd contracts/ethereum
forge test --match-test test_verifyBlock_primary_bound_succeeds_andUpdatesState -vvv 2>&1 | grep -A5 "Verify\|verifyPrimary\|verifyLayerHashes\|BlockVerified\|Gas"
```

You will see, in order:

1. The cheap shape & anchor checks (revert before crypto if any fail).
2. The first pairing check via `PrimaryVerifier` → `PrimaryGroth16VerifierGenerated`.
3. The second pairing check via `LayerHashesMovementVerifier` → `LayerHashesGroth16VerifierGenerated`.
4. The state writes (`storedLastSeenBlockSeqNo`, `storedNumLayers`, `storedLayerHashes`, `storedPrevMaxLevelLayerHash`).
5. The `BlockVerified` event.

Total cost: ~440k gas (two pairings dominate).

### F.3 Watch the chain anchor enforced

```bash
forge test --match-test test_verifyBlock_prevAnchorMismatch_reverts -vvv 2>&1 | tail -25
```

This test takes a known-good bound proof tuple and submits it with a `prevMaxLevelLayerHash` argument that doesn't match `storedPrevMaxLevelLayerHash`. The bridge reverts with `PrevAnchorMismatch(supplied, stored)` **before** either gnark verifier is called — the anchor check is among the cheap pre-crypto checks. (LH-3 / CC-6.)

### F.4 Watch the strict monotonicity enforced

```bash
forge test --match-test test_relayerLoop_replaySameSeqNo_reverts -vvv 2>&1 | tail -25
```

Submitting the same `blockSeqNo` twice (or a lower seqno) reverts with `BlockSeqNoNotMonotonic(supplied, stored)`. (LH-6 / CC-5.)

### F.5 Watch a tampered proof rejected

```bash
forge test --match-test "test_verifyBlock_tamperedAttestationProof_reverts|test_verifyBlock_tamperedLayerHashesProof_reverts" -vvv 2>&1 | tail -15
```

A single-byte mutation of either proof causes one of the two pairings to fail; the bridge surfaces `AttestationProofRejected` or `LayerHashesProofRejected` accordingly. Soundness in action.

### F.6 Watch the cross-circuit binding break

```bash
forge test --match-test test_verifyBlock_blockIdMismatchAcrossProofs_reverts -vvv 2>&1 | tail -15
```

This test changes the `bkSetCommitment` argument fed into `verifyBlock`, leaving both proofs untouched. Result: `BkSetCommitmentMismatch(supplied, stored)` revert (cheap pre-crypto check). To exercise the *circuit-level* binding (CC-2), generate two bound proofs with intentionally different `bk_set_poseidon` and watch one of the verifiers return `false` — the orchestrator's bound-test-data harness specifically prevents this construction, so you'd need to hand-craft the fixture.

---

## 7. Phase G — BK Set Rotation (Phase 1.C, pending — ≈ 5 min)

> **v2 status**: the legacy `LayerHashBridge.rotateBkSet` (ZK-proven) and 7-day timelocked
> owner fallback (`proposeBkSetCommitment` / `executeBkSetCommitment` / `cancelBkSetCommitment`)
> were retired in Phase 4.2 (2026-05-10). Until Phase 1.C ships Circuit 3, BK-set rotation is
> **not available on-chain**. Confirm `storedBkSetCommitment` is effectively immutable.

### G.1 Confirm immutability

```bash
cast call $BRIDGE "storedBkSetCommitment()(uint256)"
# → equals the deployment-time genesisBkSetCommitment

cast call $BRIDGE "primaryVerifier()(address)"
cast call $BRIDGE "fallbackVerifier()(address)"
cast call $BRIDGE "layerHashesVerifier()(address)"
# → all three are immutable; no setter exists post-deployment
```

There is no `setBkSetCommitment`, no `proposeBkSetCommitment`, no `rotateBkSet` function on the v2 bridge. The only way to install a new committee at HEAD is to redeploy the bridge with a fresh `genesisBkSetCommitment`.

### G.2 Confirm the negative test

```bash
forge test --match-test test_verifyBlock_bkSetMismatch_reverts -vv 2>&1 | tail -10
```

✅ Expected: pass — any proof signed by a different committee is rejected with `BkSetCommitmentMismatch(supplied, stored)`. This is the only BK-set-related negative path on the v2 surface today.

### G.3 Phase 1.C target invariants (forward-looking)

Once Phase 1.C ships, this section will be expanded to cover:

- `verifyBkSetUpdate(proof, oldCommitment, newCommitment)` — single-step ZK-proven rotation.
- BK-1..BK-5 invariants (see `docs/bridge_verification.md` §6.2 for the target list).
- Rotation across `verifyBlock` calls (a real shellnet epoch transition).

Until then, this phase is a 5-minute confirmation that nothing rotates today.

---

## 8. Phase H — AAVE Yield Path (≈ 20 min)

The full design is in `docs/aave_integration.md`. Here we run the runtime checks.

### H.1 Run the AAVE suite

```bash
forge test --match-contract AckiNackiBridgeAaveTest -vv 2>&1 | tail -30
```

✅ Expected: 23 tests pass.

### H.2 Verify the solvency invariant survives fuzzing

```bash
forge test --match-test testFuzz_totalAssetsCoversTreasury -vv 2>&1 | tail -5
```

✅ Expected:

```
[PASS] testFuzz_totalAssetsCoversTreasury(uint96,uint16) (runs: 256, μ: ..., ~: ...)
```

256 random combinations of `(depositAmount, reserveBps)` all preserve `ETH + aWETH ≥ treasury`.

### H.3 Trace a complete AAVE lifecycle by hand

In `forge test`, this is `test_emergencyWithdrawAll_pullsEverything` — read its assertions:

```bash
forge test --match-test test_emergencyWithdrawAll_pullsEverything -vvv 2>&1 | head -40
```

Walk through the trace and confirm:

```
deposit(10 ether)        → treasury=10, ETH=10, aWETH=0,  principal=0
supplyToAave(MAX)        → treasury=10, ETH=1,  aWETH=9,  principal=9
accrueYield(0.7)         → treasury=10, ETH=1,  aWETH=9.7, principal=9
emergencyWithdrawAll()   → treasury=10, ETH=10.7, aWETH=0, principal=0
```

The 0.7 ether of yield ends up as part of the bridge's ETH balance and can be harvested separately. **Treasury is never touched.**

### H.4 Confirm the owner cannot steal principal

The negative test:

```bash
forge test --match-test test_harvestYield_amountExceedsYieldReverts -vvv 2>&1 | tail -10
```

Confirms `harvestYield(amount)` reverts with `NoYield` if `amount > accruedYield()`. There is no path where the owner can extract `treasuryBalance` worth of ETH for themselves.

---

## 9. Phase I — Block Hash Oracle (≈ 10 min)

### I.1 Local mock behaviour

```bash
forge test --match-contract AxiomBlockHeaderOracleTest -vv 2>&1 | tail -25
```

✅ Expected: 16 tests pass, covering recent blocks, historical blocks, future blocks, malformed witnesses.

### I.2 Optional: live mainnet check

Set an Ethereum RPC URL (Alchemy, Infura, or a public one) and:

```bash
forge test --fork-url $ETH_RPC_URL --match-contract AxiomBlockHeaderOracleTest -vvv 2>&1 | tail -25
```

This exercises the oracle against the **real `AxiomV2Core`** at `0x69963768F8407dE501029680dE46945F838Fc98B` — the actual oracle the bridge will use post-deployment. If this passes, you've confirmed live integration.

### I.3 Live blockhash via `cast`

You can verify the EVM `blockhash` opcode independently:

```bash
LATEST=$(cast block-number --rpc-url $ETH_RPC_URL)
HASH_FROM_RPC=$(cast block $((LATEST - 10)) --rpc-url $ETH_RPC_URL --json | jq -r .hash)
echo "From RPC:    $HASH_FROM_RPC"

# Now ask the deployed oracle (replace $ORACLE with mainnet address)
HASH_FROM_ORC=$(cast call $ORACLE "getBlockHash(uint256)(bytes32)" $((LATEST - 10)) --rpc-url $ETH_RPC_URL)
echo "From oracle: $HASH_FROM_ORC"
```

✅ Expected: identical values. If they differ, the oracle is misbehaving.

---

## 10. Phase J — Attack Scenarios (Red-Team Walk-Through, ≈ 30 min)

**Goal**: don't just verify "the bridge accepts valid inputs" — actively try to *break* it. Each scenario below is a known attacker objective; for each, you attempt the attack and confirm it fails with a specific revert.

This phase reuses the Anvil session from Phase E (so deposit `0` and the test verifier are already deployed). Where indicated, some attacks are easier to exercise via `forge test -vvv` instead of `cast`, because they need cryptographic context the test verifier doesn't enforce.

The attacks are grouped by surface. Numbers in **bold** are the property labels from `docs/bridge_verification.md` (DEP-#, LH-#, BK-#, OR-#, AC-#, FORK-#, **CC-#**, ZK-#).

### Attack Group 1 — Proof Forgery and Replay (DEP-2, DEP-3, LH-1, LH-5)

#### J.1 Replay the same withdrawal proof twice

Attacker objective: drain the treasury by submitting the same proof multiple times.

You already executed this in Phase E.5. Reconfirm:

```bash
# Set up: deposit, then withdraw once
cast send $BRIDGE "deposit()" --value 1ether --rpc-url $RPC --private-key $PK
ID=$(cast call $BRIDGE "depositCounter()(uint256)")
ID=$((ID - 1))
BLOCKNUM=$(cast block-number --rpc-url $RPC); BLOCKNUM=$((BLOCKNUM - 1))

cast send $BRIDGE "withdraw(address,uint256,uint256,uint256,bytes)" \
  $ME 1ether $ID $BLOCKNUM 0xdeadbeef --rpc-url $RPC --private-key $PK

# Now replay
cast send $BRIDGE "withdraw(address,uint256,uint256,uint256,bytes)" \
  $ME 1ether $ID $BLOCKNUM 0xdeadbeef --rpc-url $RPC --private-key $PK
```

✅ Expected: second call reverts with selector `0xa1ff8b3b` (`DepositAlreadyProcessed()`).

**[Retired — Phase 4.3]** The `processedDeposits[depositId]` nullifier and the entire withdrawal-side double-spend protection moved to AN-side `TokenBridge.finalizeDeposit(...)`. The Halo2 deposit-prover circuit is unchanged; only the consumer side is gone on Ethereum. See `docs/verifying_eth_proof_on_an.md` §2 V5 for the AN-side nullifier responsibility.

#### J.2 Reuse a proof for a different `depositId`

**[Retired — Phase 4.3]** With the legacy ETH-side `Groth16DepositVerifier` removed, this attack now applies to the AN-side native verifier (`VERHALO2SHPLONK`). The cryptographic binding is the same — `depositId` is a public input of the Halo2 circuit, and the verifier rejects any proof whose witnessed `depositId` doesn't match. Coverage moves to `tvm-sdk` opcode tests + AN-side `TokenBridge` tests once those land.

#### J.3 Submit a proof for a **different bridge** to this bridge

**[Reframed — Phase 4.3]** Same threat model, now AN-side. `TokenBridge.finalizeDeposit` will enforce `require(publicInputs[3] == ETH_BRIDGE_ADDRESS_FR, "wrong bridge contract");`. The Halo2 circuit's `contractAddress` public input is the binding hook. See `docs/verifying_eth_proof_on_an.md` DEP-N-2.

#### J.4 Bypass the proof entirely with a malformed length

**[Retired — Phase 4.3]** for the ETH-side deposit path. The remaining ETH-side fuzz tests on length-rejection live in `FuzzHalo2VerifierTest` (for the bare Halo2 Yul verifier) and in the per-circuit AN→ETH adapter tests (`PrimaryVerifierTest::testVerifyInvalidProofLength`, etc.):

```bash
forge test --match-test "testVerify_wrongProofLength_fails" -vv 2>&1 | tail -10
```

#### J.5 Submit a single-byte-mutated Groth16 proof

Only the AN→ETH-side fuzz tests apply now:

```bash
forge test --match-test "testVerify_tamperedProof_fails|test_verifyBlock_tamperedAttestationProof_reverts|test_verifyBlock_tamperedLayerHashesProof_reverts" -vv 2>&1 | tail -15
```

✅ Expected: every mutation rejected. The pairing equation is rigid — no nearby points work. (Deposit-side mutation tests are now an AN-side responsibility.)

### Attack Group 2 — Recipient / Identity Binding (DEP-2 / DEP-N-#)

#### J.6 Withdraw to a different address than the original sender

**[Retired — Phase 4.3]** The Ethereum-side `withdraw(recipient, ...)` function no longer exists. The "sender == recipient" enforcement now lives in the Halo2 deposit circuit's public-input binding consumed on the AN side; the AN-side `TokenBridge.finalizeDeposit` will mint to the address committed by `publicInputs[1]`, so impersonation requires forging the Halo2 proof itself.

#### J.7 Withdraw to `address(0)`

**[Retired — Phase 4.3]** No ETH-side `withdraw()` path exists. The AN-side mint target is derived from the Halo2 public input — `address(0)` would require the Halo2 prover to have witnessed it, which fails the in-circuit constraints.

### Attack Group 3 — Block Hash Manipulation (DEP-6, OR-1, OR-2, OR-3, FORK-1)

> **Note (Phase 4.3)**: The on-chain block-hash oracle is currently unused by the public surface — the only consumer used to be the legacy `withdraw()`. Attack scenarios J.8–J.11 remain valid as a *standalone* property check of `AxiomBlockHeaderOracle.sol` (and as a forward-compatibility check for the future burn-proof flow), but they no longer guard any user-facing entry point on the present `AckiNackiBridge.sol`.

#### J.8 Provide a forged block hash via the user input (oracle property)

There is **no API** for the user to supply a block hash to the oracle. The only input is `blockNumber`; the hash is computed internally. To verify:

```bash
grep -n "function getBlockHash" contracts/ethereum/src/AxiomBlockHeaderOracle.sol
```

✅ Expected: a `view` function whose only parameter is `uint256 blockNumber`. No caller bytes reach the returned hash.

#### J.9 Reference a future block

```bash
forge test --match-test test_GetBlockHash_FutureBlock -vv 2>&1 | tail -5
```

✅ Expected: oracle reverts with `BlockNotYetMined()` for any block whose number is ≥ current.

#### J.10 Reference a historical block (>256 ago) without a witness

The Axiom-backed oracle reverts on `getBlockHash(n)` for any `n` older than 256 without an attached witness:

```bash
forge test --match-test test_GetBlockHash_HistoricalBlock -vv 2>&1 | tail -5
```

✅ Expected: pass — confirms the historical path requires an Axiom witness. (Until the burn-proof flow wires it in, this is a property of the oracle in isolation.)

#### J.11 Substitute a fork-side block hash

If an attacker forks Ethereum, the mainnet `AxiomBlockHeaderOracle.getBlockHash` returns the **mainnet** hash for that block number — the on-chain bytecode only sees the chain it executes on. Forks therefore cannot inject a fork-side hash into the oracle's output.

```bash
grep -A3 "function getBlockHash" contracts/ethereum/src/AxiomBlockHeaderOracle.sol
```

This is FORK-3 by construction. The hash is computed from EVM state, not from caller input.

### Attack Group 4 — `verifyBlock` State Injection (LH-1..LH-9, CC-1..CC-7, FORK-1..FORK-4)

#### J.12 Skip the chain anchor (LH-3 / CC-6)

Attacker objective: inject an arbitrary layer-hash state (e.g., one that mints fake balances) by submitting a tuple with a wrong `prevMaxLevelLayerHash`.

```bash
forge test --match-test test_verifyBlock_prevAnchorMismatch_reverts -vv 2>&1 | tail -10
forge test --match-test test_relayerLoop_anchorMismatch_reverts -vv 2>&1 | tail -10
```

✅ Expected: every wrong anchor is rejected with `PrevAnchorMismatch(supplied, stored)`. The chain anchor only allows updates that continue from the stored top-level hash.

#### J.13 Submit a proof with a stale BK set commitment (LH-2 / CC-3)

Attacker objective: use a proof signed by an old (compromised) BK committee.

```bash
forge test --match-test test_verifyBlock_bkSetMismatch_reverts -vv 2>&1 | tail -10
```

✅ Expected: rejected with `BkSetCommitmentMismatch(supplied, stored)` (cheap pre-crypto check). And: even if the caller supplies the matching `bkSetCommitment`, the proof's BLS witness must commit to the same Poseidon — if it doesn't, the gnark verifier returns `false`.

#### J.14 Lie about `numLayers` (LH-4)

```bash
forge test --match-test "test_verifyBlock_numLayersZero_reverts|test_verifyBlock_numLayersAboveCap_reverts" -vv 2>&1 | tail -10
```

✅ Expected: rejected with `InvalidNumLayers(numLayers)`. `numLayers` is range-checked at the contract level (1..=10) **and** is bound into Circuit 2's PI[2] inside the proof.

#### J.15 Inject silent garbage in unused layer slots (LH-5 / CC-7)

Attacker objective: with `numLayers = 3`, set `layerHashes[3..10]` to non-zero values, hoping the bridge stores them and they leak into a future call.

```bash
forge test --match-test test_verifyBlock_layerTailNonZero_reverts -vv 2>&1 | tail -10
```

✅ Expected: rejected with `LayerHashTailNonZero(i)` for the first non-zero index `i ≥ numLayers`.

#### J.16 Replay an older proof to "rewind" state (LH-6 / CC-5)

Attacker objective: submit a previously valid proof tuple to revert state.

```bash
forge test --match-test "test_relayerLoop_replaySameSeqNo_reverts|test_relayerLoop_lowerSeqNo_reverts" -vv 2>&1 | tail -10
```

✅ Expected: rejected with `BlockSeqNoNotMonotonic(supplied, stored)`. Strict monotonicity blocks both same-seqno replay and lower-seqno rewinds.

#### J.17 Mix proofs from two different blocks (CC-1)

Attacker objective: pair Circuit 1A's proof for block X with Circuit 2's proof for block Y, hoping the cross-binding fails to fire.

The bridge passes a single `blockId` argument into both verifier calls. If the two proofs commit to different `block_id` values, then *whichever* proof's `block_id` differs from the supplied argument will return `false`. There is no way to satisfy both verifiers with mismatched `block_id`. To exercise:

```bash
# Programmatic — see crates/bridge-prover-orchestrator/tests/cross_block_binding.rs
# (when added in Phase 5.3); at HEAD we rely on:
forge test --match-test "test_verifyBlock_primary_bound_succeeds_andUpdatesState" -vv  # passes by construction
```

The bound-test-data harness specifically *prevents* generating fixtures with diverging
`block_id` between Circuit 1A and Circuit 2; you'd need to hand-craft the negative case.

#### J.18 Swap Primary and Fallback proofs (LH-1)

Attacker objective: send a Primary proof but with `finType = Fallback` (or vice versa), routing it through the wrong verifier.

```bash
# Cross-path: a proof generated for Primary public-inputs fed into the Fallback path
# (or vice versa) is rejected by the receiving verifier — the deterministic case is
# covered by the AttestationProofRejected branch in `test_verifyBlock_fallback_rejected_byMock_reverts`
# and by `testVerify_wrongBlockSeqNo_fails` in `PrimaryVerifier.t.sol` / `FallbackVerifier.t.sol`
# (the two circuits expose different public inputs around blockSeqNo / lastSeenBlockSeqNo).
forge test --match-test "test_verifyBlock_fallback_rejected_byMock_reverts|testVerify_wrongBlockSeqNo_fails" -vv 2>&1 | tail -10
```

✅ Expected: rejected — each verifier has its own VK, so a Primary proof fails the Fallback pairing and vice versa. Inside the circuits, the target_type discriminant at offset 116 of `AttestationData` is constrained to `0` (Primary) or `1` (Fallback) respectively, so the BLS witness for one cannot satisfy the other.

#### J.19 Disable `verifyBlock` post-deployment (LH-9)

Attacker objective: set one of the three verifier slots to `address(0)` to permanently brick the surface.

There is no setter for `primaryVerifier`, `fallbackVerifier`, or `layerHashesVerifier` — they are `immutable` after construction. To prove this:

```bash
grep -n "immutable.*Verifier\|primaryVerifier\|fallbackVerifier\|layerHashesVerifier" \
  contracts/ethereum/src/AckiNackiBridge.sol
```

✅ Expected: each appears in an `immutable` declaration; no setter is defined.

### Attack Group 5 — BK Rotation Abuse (Phase 1.C pending — placeholder)

> **v2 status**: the legacy `LayerHashBridge.rotateBkSet` and 7-day timelocked owner
> fallback were retired in Phase 4.2. There is no on-chain rotation surface at HEAD. The
> attacks J.20 below is the only relevant negative path until Phase 1.C lands Circuit 3.

#### J.20 Try to forge a BK-set rotation (no surface exists)

Attacker objective: rotate the committee without the current committee's consent.

```bash
grep -n "rotateBkSet\|proposeBkSetCommitment\|setBkSetCommitment" \
  contracts/ethereum/src/AckiNackiBridge.sol
```

✅ Expected: no matches. There is no function that mutates `storedBkSetCommitment` post-deployment. Until Phase 1.C ships, the only way to install a new committee is to redeploy the bridge.

#### J.20a (forward-looking, Phase 1.C) Forge a Circuit 3 proof

Once Phase 1.C lands, this attack becomes:

```bash
# Future:
# forge test --match-test "testRevertOnInvalidBkSetUpdateProof" -vv
```

The Circuit 3 verifier will check that the *current* committee (committed to `storedBkSetCommitment`) signed off on the *new* committee's commitment. Forgery without the current committee's signing keys is reduced to BLS12-381 forgery (covered by FORK-1 / K-6).

### Attack Group 6 — Reentrancy (AC-1, AC-2, AC-6)

#### J.21 Reenter `withdraw` from the recipient

**[Retired — Phase 4.3]** The `withdraw(...)` function and its `recipient.transfer(amount)` call site are gone. The defence used to be a 2300-gas stipend + `nonReentrant` modifier on `withdraw()`. The remaining mutating functions on `AckiNackiBridge` (`deposit`, `supplyToAave`, `withdrawFromAave`, `emergencyWithdrawAll`, `harvestYield`, `verifyBlock`) all retain `nonReentrant`; only `verifyBlock` performs external calls (the two `view`-style Groth16 verifier calls), which cannot reenter even if the verifier is malicious. See J.22.

To see the protection in source:

```bash
grep -n "nonReentrant\|recipient.transfer" contracts/ethereum/src/AckiNackiBridge.sol
```

✅ Expected: `withdraw` has the `nonReentrant` modifier and uses `transfer(amount)` (not `call`). Both layers must be defeated for a reentrancy attack.

#### J.22 Reenter `deposit` during AAVE pull

Attacker objective: trigger `withdraw → _pullFromAave → gateway → … → fallback into bridge.deposit` to corrupt accounting mid-flight.

```bash
grep -B1 -A2 "function deposit\|function withdraw\|function supplyToAave\|function _pullFromAave" \
  contracts/ethereum/src/AckiNackiBridge.sol | grep -E "function|nonReentrant"
```

✅ Expected: every mutating function has `nonReentrant`. Even if the AAVE gateway misbehaves, the reentrancy guard blocks any attempt to re-enter `deposit/withdraw/supplyToAave/...`.

### Attack Group 7 — Access Control (AC-2, AC-4)

#### J.23 Non-owner attempts every privileged function

Set up a non-owner account in your Anvil session:

```bash
ATTACKER_PK=0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d
ATTACKER=0x70997970C51812dc3A010C7d01b50e0d17dc79C8
```

Try each owner-only function:

```bash
# Bridge AAVE management — all should revert with NotOwner (selector 0x30cd7471)
cast send $BRIDGE "supplyToAave(uint256)" 1 --rpc-url $RPC --private-key $ATTACKER_PK
cast send $BRIDGE "withdrawFromAave(uint256)" 1 --rpc-url $RPC --private-key $ATTACKER_PK
cast send $BRIDGE "emergencyWithdrawAll()" --rpc-url $RPC --private-key $ATTACKER_PK
cast send $BRIDGE "harvestYield(uint256)" 1 --rpc-url $RPC --private-key $ATTACKER_PK
cast send $BRIDGE "setAaveEnabled(bool)" true --rpc-url $RPC --private-key $ATTACKER_PK
cast send $BRIDGE "setLiquidReserveBps(uint256)" 100 --rpc-url $RPC --private-key $ATTACKER_PK
cast send $BRIDGE "setYieldRecipient(address)" $ATTACKER --rpc-url $RPC --private-key $ATTACKER_PK
cast send $BRIDGE "transferOwnership(address)" $ATTACKER --rpc-url $RPC --private-key $ATTACKER_PK
```

✅ Expected: every call reverts.

(Most of these will revert anyway because the Anvil deployment used `address(0)` for AAVE — but the access control check fires first, and you'll see the `NotOwner` selector in the revert data.)

#### J.24 Old owner attempts after `transferOwnership`

```bash
forge test --match-test test_transferOwnership_flowsAllAuthorities -vv 2>&1 | tail -10
```

✅ Expected: the old owner's calls revert; only the new owner can act.

### Attack Group 8 — Field / Encoding Edge Cases

#### J.25 Public input above the BN254 field modulus

Attacker objective: confuse the verifier by passing an oversized scalar.

```bash
forge test --match-test "testFuzz_InputsAboveFieldModulusRevert|test_Regression_FieldOverflowInstance" -vv 2>&1 | tail -10
```

✅ Expected: pass. Inputs ≥ p (BN254 modulus) are rejected by the auto-generated Groth16 verifier (it does `assert(input < p)` for each input).

#### J.26 Wrong number of public inputs

```bash
forge test --match-test "testFuzz_WrongInputCountRejects" -vv 2>&1 | tail -5
```

✅ Expected: pass — adapter rejects any vector that doesn't match the circuit's expected length.

#### J.27 Submit valid-looking calldata of arbitrary length

```bash
forge test --match-test "testFuzz_RandomCalldataReverts|testFuzz_TruncatedCalldataReverts|testFuzz_CorrectLengthRandomCalldataReverts" -vv 2>&1 | tail -15
```

✅ Expected: every one rejected.

### Attack Group 9 — Economic / DoS

#### J.28 Deposit above `MAX_DEPOSIT_AMOUNT`

```bash
cast send $BRIDGE "deposit()" --value 101ether --rpc-url $RPC --private-key $PK
```

✅ Expected: revert with `DepositTooLarge()` selector. Caps catastrophic single-deposit risk at 100 ETH.

#### J.29 Try to lock funds by raising `liquidReserveBps`

Attacker objective: a malicious owner sets `liquidReserveBps = 100%` to lock all funds in the bridge as ETH (no AAVE yield possible).

```bash
forge test --match-test test_setLiquidReserveBps_capped -vv 2>&1 | tail -5
```

✅ Expected: pass — `MAX_LIQUID_RESERVE_BPS = 5000` caps the reserve at 50%. And even at 50%, this is a yield-griefing attack at worst — it does not affect user solvency.

#### J.30 Attempt to drain treasury via AAVE drain

Attacker objective: the owner calls `withdrawFromAave(suppliedPrincipal)` then takes the ETH.

`withdrawFromAave` only changes the bridge's accounting (ETH ↔ aWETH within the bridge). It does **not** transfer ETH to anyone.

```bash
grep -A6 "function withdrawFromAave" contracts/ethereum/src/AckiNackiBridge.sol
```

The function calls `_pullFromAave(target)` which only updates `address(this).balance` and `suppliedPrincipal`. There is no `recipient.transfer` or `.call` to an external address. The owner ends with the same total assets they started with — just rebalanced.

#### J.31 Attempt to drain via `harvestYield` overdraw

```bash
forge test --match-test test_harvestYield_amountExceedsYieldReverts -vv 2>&1 | tail -5
```

✅ Expected: pass — `harvestYield` is bounded by `accruedYield()`. Owner cannot withdraw principal under the guise of yield.

#### J.32 Direct ETH transfer to the bridge

Attacker objective: send ETH directly to the bridge to corrupt accounting.

```bash
cast send $BRIDGE --value 0.1ether --rpc-url $RPC --private-key $PK
cast call $BRIDGE "treasuryBalance()(uint256)"   # unchanged
cast balance $BRIDGE                              # increased by 0.1 ether
```

✅ Expected: `treasuryBalance` is unchanged; the extra ETH is "phantom liquidity" that improves solvency but has no claim attached. It is **not** withdrawable by anyone (no proof can match). It can only be recovered if the owner sets up a route — but no such route exists in this contract.

### Attack Summary Table

| # | Attack | Expected revert / property | Test / source |
|---|---|---|---|
| J.1 | Replay withdraw | **[Retired Phase 4.3]** AN-side nullifier in `TokenBridge.finalizeDeposit` | `tvm-sdk` opcode tests (once `VERHALO2SHPLONK` lands) |
| J.2 | Reuse proof for another deposit ID | **[Retired Phase 4.3]** AN-side cryptographic rejection | same |
| J.3 | Cross-contract proof | **[Reframed Phase 4.3]** AN-side `require(publicInputs[3] == ETH_BRIDGE_ADDRESS_FR)` | same |
| J.4 | Wrong proof length | **[Retired Phase 4.3]** for deposit; AN→ETH adapters still enforce 256 B | `PrimaryVerifierTest::testVerify_wrongProofLength_fails` (+ `FallbackVerifierTest`, `LayerHashesMovementVerifierTest` analogues) |
| J.5 | Single-byte-mutated proof | Pairing fails (AN→ETH only) | `test_verifyBlock_tamperedAttestationProof_reverts` + `test_verifyBlock_tamperedLayerHashesProof_reverts` |
| J.6 | Different recipient | **[Retired Phase 4.3]** — no ETH-side `withdraw()` | — |
| J.7 | Recipient = address(0) | **[Retired Phase 4.3]** — no ETH-side `withdraw()` | — |
| J.8 | Forged block hash via user input (oracle property) | impossible (no API) | source review of `AxiomBlockHeaderOracle.sol` |
| J.9 | Future block (oracle property) | `BlockNotYetMined` | `test_GetBlockHash_FutureBlock` |
| J.10 | Historical block without witness (oracle property) | revert | `test_GetBlockHash_HistoricalBlock` |
| J.11 | Fork block hash (oracle property) | Oracle returns canonical hash | source review |
| J.12 | Skip chain anchor | `PrevAnchorMismatch` | `test_verifyBlock_prevAnchorMismatch_reverts` + `testFuzz_prevAnchorMismatch_reverts` |
| J.13 | Stale BK commitment | `BkSetCommitmentMismatch` (cheap) + pairing fails | `test_verifyBlock_bkSetMismatch_reverts` + `testFuzz_bkSetMismatch_reverts` |
| J.14 | Wrong `numLayers` | `InvalidNumLayers` | `test_verifyBlock_numLayersZero_reverts` / `test_verifyBlock_numLayersAboveCap_reverts` + `testFuzz_invalidNumLayers_reverts` |
| J.15 | Tail garbage in `layerHashes` | `LayerHashTailNonZero` | `test_verifyBlock_layerTailNonZero_reverts` + `testFuzz_layerTailNonZero_reverts` |
| J.16 | Replay / rewind seqno | `BlockSeqNoNotMonotonic` | `test_relayerLoop_replaySameSeqNo_reverts` |
| J.17 | Mix proofs from two blocks (CC-1) | gnark `false` ⇒ `*ProofRejected` | by construction in `test_verifyBlock_primary_bound_succeeds_andUpdatesState` (bound scenario shares `block_id` + `bk_set_poseidon` across 1A and 2 by construction) |
| J.18 | Swap Primary↔Fallback (LH-1) | gnark `false` ⇒ `AttestationProofRejected` | per-route mismatch test |
| J.19 | Disable verifier slot post-deploy | impossible (immutable) | source review |
| J.20 | Forge BK-set rotation (Phase 1.C, pending) | no surface; redeploy required | source review (no `rotateBkSet`) |
| J.21 | Reenter via recipient | **[Retired Phase 4.3]** — no `withdraw()` callsite | — |
| J.22 | Reenter via AAVE | `nonReentrant` everywhere | source review |
| J.23 | Non-owner privileged calls | `NotOwner` (every function) | per-function tests |
| J.24 | Old owner after transfer | `NotOwner` | `test_transferOwnership_flowsAllAuthorities` |
| J.25 | Input ≥ field modulus | Verifier rejects | per-adapter fuzz |
| J.26 | Wrong input count | False / revert | per-adapter tests |
| J.27 | Random calldata | False / revert | `FuzzHalo2VerifierTest::*` |
| J.28 | Deposit > 100 ETH | `InvalidAmount` | `testFuzz_DepositInvalidAmountReverts` |
| J.29 | Owner sets reserve = 100% | `ReserveBpsTooHigh` (capped at 50%) | `test_setLiquidReserveBps_capped` |
| J.30 | Drain via withdrawFromAave | No path; only rebalances | source review |
| J.31 | Drain via harvestYield overdraw | `NoYield` | `test_harvestYield_amountExceedsYieldReverts` |
| J.32 | Direct ETH transfer | Phantom liquidity, no claim | manual |

✅ All ETH-side attack vectors blocked at HEAD. (J.1, J.2, J.3 enforcement moved to the AN side; J.6, J.7, J.21 are retired together with `withdraw()`. J.20a is forward-looking for Phase 1.C.) If any of the live ones unexpectedly succeeds, **stop and escalate**.

### Where attacks could theoretically still work

To be honest about residual risk:

- **51% attack on Ethereum L1**: would let an attacker rewrite history that the oracle reads. Out of scope for any L1 dApp.
- **>2/3 attack on Acki Nacki BK set**: would let attackers forge BLS-signed Primary attestations. Out of scope for any chain-to-chain bridge — see FORK-2 / K-9 in `audit_trail_v2.md`.
- **>1/2 attack on Acki Nacki BK set with Fallback path enabled**: a smaller adversarial threshold (1/2+1) suffices to forge Fallback-finalized blocks. The bridge accepts Fallback-attested updates by design (the AN protocol does too).
- **G2 subgroup gap (audit FORK-2 / BLS-1 / K-6)**: open audit finding; attacker would need a non-trivial G2 element in the wrong subgroup with a forged-looking signature. **Carries over to v2's Circuit 1A and 1B unchanged.** Launch blocker for mainnet; testnet OK.
- **gnark wrapper stub `Define` (AN→ETH side)**: R15 — the per-circuit AN→ETH wrappers (1A/1B/2) currently produce Groth16 proofs that commit to the public inputs without verifying the underlying Halo2 proof inside Groth16. The deposit-side wrapper that used to share this finding was **retired in Phase 4.3 together with the rest of the ETH-side deposit-verifier chain**, so this is now a one-sided AN→ETH-only concern, tracked under `an_partner_integration_plan.md` Phase 8.
- **`VERHALO2SHPLONK` opcode soundness (new in Phase 4.3)**: the AN-side deposit verification depends on the correctness of the new TVM opcode in `tvm-sdk`. The opcode is in development; CI coverage + a partner-side review are required before mainnet. Tracked under Decision Log 2026-05-17 + Phase 8.
- **Trusted setup**: KZG SRS and per-circuit gnark Groth16 setup ceremonies are points of trust. Verify the SRS checksums match a recognised ceremony before deployment.
- **No on-chain BK-set rotation at HEAD**: a stale committee can attack until the bridge is redeployed with a fresh `genesisBkSetCommitment`. Phase 1.C eliminates this gap.

These are documented in `docs/audit_trail_v2.md` §2 and `docs/an_partner_integration_plan.md` §5 (Risk Register).

---

## 11. Phase K — Final Sign-Off Checklist

Run through this in order. Tick each. **Do not deploy if any item is unchecked.**

### Static

- [ ] `forge build` succeeds with no errors.
- [ ] `forge fmt --check` is clean.
- [ ] AGENTS.md was read and matches what's actually in the repo.

### Tests

- [ ] `forge test` reports **132 passed; 0 failed; 0 skipped** across **14 suites** (+1 auto-skipped suite without `FORK_URL`).
- [ ] `AckiNackiBridgeVerifyBlockTest` (17) + `AckiNackiBridgeRelayerLoopTest` (6) pass — `verifyBlock` happy paths and 6 sequencing scenarios.
- [ ] `AckiNackiBridgeAaveTest` (20 tests) passes — including the fuzz solvency invariant.

### Property checks (Phase E hands-on)

- [ ] Deposit succeeds, increments `depositCounter`, increases `treasuryBalance`, emits `Deposit`.
- [ ] Zero-value deposit reverts with `InvalidAmount`.
- [ ] Over-cap deposit (> 100 ETH) reverts with `InvalidAmount`.
- [ ] `! grep -E "function withdraw\(|IAckiNackiVerifier" contracts/ethereum/src/AckiNackiBridge.sol` is clean (no legacy surface).

### `verifyBlock` (Phase F hands-on)

- [ ] Real bound proof tuple `test_verifyBlock_primary_bound_succeeds_andUpdatesState` verifies on-chain (~440k gas).
- [ ] `test_verifyBlock_fallback_routesToFallbackVerifier` verifies on-chain through `FallbackVerifier` (mock-shaped at this layer; real Fallback proof coverage in `FallbackVerifierTest`).
- [ ] Wrong `prevMaxLevelLayerHash` reverts with `PrevAnchorMismatch`.
- [ ] Single-byte mutation of either 256-byte proof is rejected (`AttestationProofRejected` or `LayerHashesProofRejected`).
- [ ] Wrong `bkSetCommitment` argument reverts with `BkSetCommitmentMismatch` (cheap pre-crypto check).
- [ ] Replay or lower `blockSeqNo` reverts with `BlockSeqNoNotMonotonic`.
- [ ] `numLayers` outside `[1,10]` reverts with `InvalidNumLayers`.
- [ ] Non-zero `layerHashes[i]` for `i ≥ numLayers` reverts with `LayerHashTailNonZero`.
- [ ] Verifier slots are `immutable` — confirmed by source review.

### BK rotation (Phase 1.C, pending)

- [ ] `storedBkSetCommitment` returns the deployment-time `genesisBkSetCommitment` and is unchanged.
- [ ] No on-chain rotation function exists (`grep -n "rotateBkSet\|setBkSetCommitment" src/AckiNackiBridge.sol` returns no matches).
- [ ] `test_verifyBlock_bkSetMismatch_reverts` (+ `testFuzz_bkSetMismatch_reverts`) confirms wrong-committee proofs are rejected.

### AAVE

- [ ] Solvency invariant holds under fuzzing (256 runs).
- [ ] `harvestYield` cannot drain principal.
- [ ] `emergencyWithdrawAll` returns all funds and disables further supplies.
- [ ] User withdrawals continue to work after emergency drain.

### Oracle

- [ ] Recent-block path returns the canonical `blockhash`.
- [ ] Future-block call reverts.
- [ ] (Optional) fork-test against live Axiom V2 Core succeeds.

### Attacks blocked (Phase J)

All live ETH-side attack scenarios in Phase J reach their expected revert / rejection. Phase 4.3 moved some scenarios to AN-side responsibilities; group totals at HEAD:

- [ ] **Group 1 (Proof forgery, J.1–J.5)** — J.1/J.2/J.3 retired to AN side; J.4 retired on deposit, live on AN→ETH adapters; J.5 live on AN→ETH.
- [ ] **Group 2 (Identity binding, J.6–J.7)** — both retired in Phase 4.3 (no ETH-side `withdraw()`).
- [ ] **Group 3 (Block hash, J.8–J.11)** — oracle properties only; no live consumer on ETH side.
- [ ] **Group 4 (`verifyBlock` injection, J.12–J.19)** — 8 attacks blocked (LH-#, CC-#, FORK-#).
- [ ] **Group 5 (BK rotation abuse, J.20)** — 1 attack blocked at HEAD; J.20a deferred to Phase 1.C.
- [ ] **Group 6 (Reentrancy, J.21–J.22)** — J.21 retired (no `withdraw()`); J.22 live (`nonReentrant` on all mutating functions).
- [ ] **Group 7 (Access control, J.23–J.24)** — every owner-only function rejects non-owners.
- [ ] **Group 8 (Field/encoding, J.25–J.27)** — fuzz suites pass.
- [ ] **Group 9 (Economic/DoS, J.28–J.32)** — 5 attacks blocked.
- [ ] Residual risks (51% on L1, >2/3 on AN, G2 subgroup, gnark stub on AN→ETH side, `VERHALO2SHPLONK` opcode soundness, trusted setup, no-rotation gap) acknowledged and tracked in `docs/audit_trail_v2.md` §2 and `docs/an_partner_integration_plan.md` §5.

### Pre-deployment (only if shipping)

- [ ] Mainnet deployment uses `USE_AXIOM_ORACLE=true`.
- [ ] If AAVE is desired: `USE_AAVE=true` and chain ID is 1 (mainnet).
- [ ] After deployment, run all `cast call` checks from `docs/bridge_verification.md` §11 — every immutable matches its expected value.
- [ ] `transferOwnership` to multisig **before** any non-zero funds are deposited.

---

## 12. Common Issues / Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `forge build` complains about `node_modules/poseidon-solidity` | `npm install` not run | `cd contracts/ethereum && npm install` |
| `test_verifyBlock_primary_bound_succeeds_andUpdatesState` fails | bound proof bytes hard-coded in the test file no longer match a regenerated scenario | The bound proof bytes (`PROOF_PRIMARY`, `PROOF_LAYER_HASHES`) and public-input constants (`BLOCK_ID`, `BK_SET_POSEIDON`, `LAYER_HASH_*`, `PREV_MAX_LEVEL_LAYER_HASH`) are hard-coded in `contracts/ethereum/test/AckiNackiBridgeVerifyBlock.t.sol`. If they need refreshing, regenerate via `cargo run -p bridge-prover-orchestrator --bin export-bound-block-proofs --release` (~30-45 min), then copy the new values out of `crates/bridge-prover-orchestrator/proofs/bound/bound_scenario.json` into the constants and re-run `forge test`. |
| `forge test` 1 fewer test than expected | filter / rename / removed test | Run `forge test --list` and diff against the table in §3 |
| Anvil session: `cast send` complains about gas | account out of ETH | use a different prefunded account |
| Fork test fails with "no upstream" or 401 | bad RPC URL | use a paid Alchemy / Infura key; public RPCs sometimes block historical reads |

---

## 13. After This Runbook

If everything in §11 ticks:

- The bridge is correct in the sense exercised by **132 unit/fuzz/E2E tests** across 14 suites (post-Phase 4.3 + Circuit 4 Phase A scaffolding + verifyBlock fuzz coverage), a **complete manual walk-through**, and the ≥ 30 deliberate attack scenarios in Phase J (with the Phase 4.3 reclassifications above) all failing in the expected way.
- You have personally observed:
  - A real deposit event being emitted.
  - A real bound Groth16 tuple (Circuit 1A + Circuit 2) being verified through `verifyBlock`.
  - Zero-value and over-cap deposits being rejected.
  - The chain anchor (`PrevAnchorMismatch`) and strict monotonicity (`BlockSeqNoNotMonotonic`) preventing state injection.
  - The cross-circuit binding via `block_id` and `bk_set_poseidon` (CC-1, CC-2) holding by construction.
  - The owner being unable to steal principal.
  - `storedBkSetCommitment` confirmed immutable at HEAD (Phase 1.C pending).
  - Reentrancy guards firing.
  - Every owner-only function rejecting non-owners.

If anything didn't tick: **stop and report**. Cross-reference the failing test or assertion to:

- `docs/bridge_verification.md` for the property statement (DEP-#, LH-#, BK-#, OR-#, AC-#, FORK-#, CC-#, ZK-#).
- `docs/four_circuit_architecture.md` for the architectural context.
- `docs/audit_trail_v2.md` for the trust-assumption delta.
- `docs/layer_hashes_circuit_audit.md` for circuit-level details.

Total elapsed time for a thorough first run: **≈ 2 hours**, split roughly 15 min static / 5 min build / 5 min test / 5 min real-proof / 30 min Anvil / 15 min `verifyBlock` walk / 5 min Phase 1.C check / 20 min AAVE / 10 min oracle / 30 min attacks / 5 min checklist.

Subsequent runs (CI + spot-check after a code change): **≈ 5 min** (Phase B + C + D). Add Phase J for any change that touches `AckiNackiBridge.sol` or any verifier — the attack scenarios catch regressions that unit tests might miss.
