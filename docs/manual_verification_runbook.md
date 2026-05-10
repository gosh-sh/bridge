# Manual Verification Runbook (v2)

> **v2 update (2026-05-10).** Phase D, F, G, and Attack Groups 4 / 5 of Phase J have been
> rewritten for the four-circuit architecture. The legacy `LayerHashBridge.sol` walkthrough
> (Phase F v1) and the dual ZK + timelock BK-rotation flows (Phase G v1) were retired in
> Phase 4.2; their successors are `verifyBlock` (Phase F v2) and Phase 1.C placeholder
> (Phase G v2). 8 new CC-# attack scenarios were added; 5 legacy LH-/BK-specific scenarios
> were retired.

A hands-on, copy-pasteable plan for a single human reviewer to verify the bridge **works correctly and is attack-resistant** from the outside. No prior knowledge of the codebase is assumed.

**Time budget**: ~2.5 hours for a thorough first pass, ~5 min for follow-up sanity checks after a change.

This is the procedural twin of `docs/bridge_verification.md` (which states *what* must hold). Here you actually run things and tick boxes.

## Phase Overview

| § | Phase | Duration | Purpose |
|---|---|---|---|
| 0 | Prerequisites | 5 min | Tooling check |
| 1 | A — Static Inspection | 15 min | Read the contracts |
| 2 | B — Build | 3 min | Compile clean |
| 3 | C — Automated Tests | 1 min | 135/135 green |
| 4 | D — Single-Block Bound Real-Proof | 1 min | Real Groth16 tuple verifies on-chain via `verifyBlock` |
| 5 | E — Anvil ETH→AN | 30 min | Deposit/withdraw + 4 deliberate failures |
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
- [ ] `withdraw(...)` reads `bytes32 blockHash = blockHeaderOracle.getBlockHash(blockNumber);` — caller does **not** supply the block hash.
- [ ] In `withdraw`, the order is: `processedDeposits[depositId] = true;` → `treasuryBalance -= amount;` → external call (CEI).
- [ ] `recipient.transfer(amount)` (2300 gas stipend) is the last action.
- [ ] `MAX_DEPOSIT_AMOUNT = 100 ether`.

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
Ran 15 test suites in ...: 135 tests passed, 0 failed, 0 skipped (135 total tests)
```

Per-suite expectation (post-Phase 4.2; canonical breakdown maintained in `AGENTS.md`):

| Suite | Tests |
|---|---|
| `AckiNackiBridgeAaveTest` | 23 |
| `AckiNackiBridgeV2Test` | 14 |
| `AckiNackiBridgeVerifyBlockTest` (Phase 4 AN→ETH) | 17 |
| `AckiNackiBridgeRelayerLoopTest` (Phase 5.1 — 10-block loop) | 6 |
| `AxiomBlockHeaderOracleTest` | 16 |
| `Blake2bHalo2VerifierTest` / `KeccakHalo2VerifierTest` | 8 |
| `Halo2PoseidonVerifierTest` | 7 |
| `FuzzAckiNackiBridgeTest` | 5 |
| `FuzzGroth16DepositVerifierTest` | 3 |
| `FuzzGroth16VerifierTest` | 4 |
| `FuzzHalo2VerifierTest` | 6 |
| `PrimaryVerifierTest` (Circuit 1A) | 8 |
| `FallbackVerifierTest` (Circuit 1B) | 8 |
| `LayerHashesMovementVerifierTest` (Circuit 2) | 10 |
| **Total** | **135** |

```bash
forge test --gas-report 2>&1 | grep -E "AckiNackiBridge|verifyBlock|AAVE" | head -10
```

Spot the headline numbers:

- `AckiNackiBridge.deposit` ≈ 90k gas (no AAVE call).
- `AckiNackiBridge.verifyBlock` ≈ 440k gas (two pairings + storage updates).
- `Groth16DepositVerifier.verify` ≈ 287k gas (Groth16 pairing for the deposit path).

---

## 4. Phase D — Single-Block Bound Real-Proof (≈ 1 min)

This is the **smoking gun** test for v2: a tuple of real bound Groth16 proofs (Circuit 1A + Circuit 2) generated by `crates/bridge-prover-orchestrator`, verified on-chain through `AckiNackiBridge.verifyBlock`.

```bash
cd contracts/ethereum
forge test --match-test "testHappyPathPrimary|testHappyPathFallback" -vv 2>&1 | tail -25
```

✅ Expected: both tests pass. Look for:

- `BlockVerified(blockId=…, blockSeqNo=…, finType=0 [Primary], numLayers=…)` event in the trace.
- ~440k gas total (two pairing checks + storage updates).
- `storedLastSeenBlockSeqNo`, `storedNumLayers`, `storedPrevMaxLevelLayerHash` advance after the call.

To regenerate the bound proofs from scratch (~10-15 min, requires Halo2 keygen + gnark wrap for both circuits):

```bash
cd ../..
cargo run -p bridge-prover-orchestrator --bin export-bound-block-proofs --release
ls -la crates/bridge-prover-orchestrator/exports/ | head
```

✅ Expected: a `bound_scenario.json` plus `groth16_proof_circuit-1a.hex` (256 B), `groth16_proof_circuit-2.hex` (256 B), and matching `groth16_public_inputs_*.hex` files.

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

Pick out the bridge address:

```bash
BRIDGE=$(grep "AckiNackiBridge deployed at:" /tmp/deploy.log | awk '{print $NF}')
ORACLE=$(grep "MockBlockHeaderOracle deployed at:" /tmp/deploy.log | awk '{print $NF}')
VERIFIER=$(grep "TestDepositVerifier deployed at:" /tmp/deploy.log | awk '{print $NF}')
echo "Bridge:   $BRIDGE"
echo "Oracle:   $ORACLE"
echo "Verifier: $VERIFIER"
```

Sanity-check it's wired:

```bash
cast call $BRIDGE "verifier()(address)"            # → $VERIFIER
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

### E.4 Make a successful withdrawal

The test verifier `TestDepositVerifier` accepts any non-empty proof with the right format, so we can drive `withdraw` end-to-end.

```bash
# Get a recent block hash — required input for the proof
BLOCKNUM=$(cast block-number --rpc-url $RPC)
BLOCKNUM=$((BLOCKNUM - 1))   # use a confirmed block
echo "Using block: $BLOCKNUM"

# A non-empty dummy proof — TestDepositVerifier accepts this
PROOF=0xdeadbeef

# Withdraw
cast send $BRIDGE "withdraw(address,uint256,uint256,uint256,bytes)" \
  $ME 1ether 0 $BLOCKNUM $PROOF \
  --rpc-url $RPC --private-key $PK
```

✅ Expected: transaction succeeds. State after:

```bash
cast call $BRIDGE "treasuryBalance()(uint256)"          # → 0
cast call $BRIDGE "isDepositProcessed(uint256)(bool)" 0 # → true
cast balance $BRIDGE                                    # → 0
```

### E.5 Try a double-spend — must fail

```bash
cast send $BRIDGE "withdraw(address,uint256,uint256,uint256,bytes)" \
  $ME 1ether 0 $BLOCKNUM $PROOF \
  --rpc-url $RPC --private-key $PK
```

✅ Expected:

```
Error: ... 0xa1ff8b3b   (selector for DepositAlreadyProcessed())
```

This is **DEP-3** (the nullifier check) firing exactly as designed.

### E.6 Try withdrawal to zero address — must fail

First refund:

```bash
cast send $BRIDGE "deposit()" --value 1ether --rpc-url $RPC --private-key $PK
```

Then attempt:

```bash
cast send $BRIDGE "withdraw(address,uint256,uint256,uint256,bytes)" \
  0x0000000000000000000000000000000000000000 1ether 1 $BLOCKNUM $PROOF \
  --rpc-url $RPC --private-key $PK
```

✅ Expected: revert with selector `0x9c8d2cd2` (`InvalidRecipient()`).

### E.7 Try empty proof — must fail

```bash
cast send $BRIDGE "withdraw(address,uint256,uint256,uint256,bytes)" \
  $ME 1ether 1 $BLOCKNUM 0x \
  --rpc-url $RPC --private-key $PK
```

✅ Expected: revert with `InvalidProof()` selector. The `TestDepositVerifier` rejects empty proofs.

### E.8 Try future block — must fail

```bash
FUTURE=$((BLOCKNUM + 1000000))
cast send $BRIDGE "withdraw(address,uint256,uint256,uint256,bytes)" \
  $ME 1ether 1 $FUTURE $PROOF \
  --rpc-url $RPC --private-key $PK
```

✅ Expected: revert. The mock oracle returns `0` for unknown blocks, and the bridge's `if (blockHash == bytes32(0)) revert InvalidBlockHash()` catches it.

**Phase E checkpoint** — at this point you have empirically verified:

- Deposits emit events and update treasury.
- Withdrawals require a non-empty proof and a known block hash.
- Double-spends are rejected.
- Zero-recipient is rejected.
- Empty proofs are rejected.
- Future blocks are rejected.

---

## 6. Phase F — `verifyBlock` Walk-Through (≈ 15 min)

The Solidity tests already exercise this with real proofs (Phase D) and a 10-block mock loop (Phase C → `AckiNackiBridgeRelayerLoopTest`). It's worth doing one round by hand to feel the cross-circuit binding and the state machine.

### F.1 Read the bound proof tuple

```bash
cd ../..
cat crates/bridge-prover-orchestrator/exports/bound_scenario.json | jq .
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
forge test --match-test testHappyPathPrimary -vvv 2>&1 | grep -A5 "Verify\|verifyPrimary\|verifyLayerHashes\|BlockVerified\|Gas"
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
forge test --match-test testRevertOnPrevAnchorMismatch -vvv 2>&1 | tail -25
```

This test takes a known-good bound proof tuple and submits it with a `prevMaxLevelLayerHash` argument that doesn't match `storedPrevMaxLevelLayerHash`. The bridge reverts with `PrevAnchorMismatch(supplied, stored)` **before** either gnark verifier is called — the anchor check is among the cheap pre-crypto checks. (LH-3 / CC-6.)

### F.4 Watch the strict monotonicity enforced

```bash
forge test --match-test test_relayerLoop_replaySameSeqNo_reverts -vvv 2>&1 | tail -25
```

Submitting the same `blockSeqNo` twice (or a lower seqno) reverts with `BlockSeqNoNotMonotonic(supplied, stored)`. (LH-6 / CC-5.)

### F.5 Watch a tampered proof rejected

```bash
forge test --match-test "testRevertOnTamperedAttestationProof|testRevertOnTamperedLayerHashesProof" -vvv 2>&1 | tail -15
```

A single-byte mutation of either proof causes one of the two pairings to fail; the bridge surfaces `AttestationProofRejected` or `LayerHashesProofRejected` accordingly. Soundness in action.

### F.6 Watch the cross-circuit binding break

```bash
forge test --match-test testRevertOnTamperedBkSetInProof -vvv 2>&1 | tail -15
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
forge test --match-test testRevertOnBkSetCommitmentMismatch -vv 2>&1 | tail -10
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

**Why it fails**: `processedDeposits[depositId] = true;` is set on the first successful call before the ETH transfer.

#### J.2 Reuse a proof for a different `depositId`

Attacker objective: re-purpose a valid proof to drain a different deposit.

```bash
# Make a second deposit
cast send $BRIDGE "deposit()" --value 1ether --rpc-url $RPC --private-key $PK
# Try to withdraw deposit 1 using a proof crafted for deposit 0
# (with TestDepositVerifier, the proof must "match" — it just checks format,
# but a real Groth16 verifier ties depositId to the proof.)
```

For the *real* Groth16 verifier, the binding is enforced cryptographically. To see it:

```bash
cd contracts/ethereum
forge test --match-test testFuzz_RandomProofAndInputsReject -vv 2>&1 | tail -5
```

✅ Expected: 256 random combinations of (proof, inputs) all reject. Cryptographic binding holds.

#### J.3 Submit a proof for a **different bridge** to this bridge

Attacker objective: take a valid proof from bridge B and submit it to bridge A.

The bridge writes `publicInputs[3] = uint256(uint160(address(this)));` — its own address. The Groth16 verifier checks this against the proof's witnessed contract. So a proof made for bridge B has `contractAddress = B`, and bridge A constructs `publicInputs[3] = A`, causing the pairing to fail.

```bash
# Conceptual: see the public input layout in source
grep -A8 "publicInputs\[3\]" contracts/ethereum/src/AckiNackiBridge.sol
```

#### J.4 Bypass the proof entirely with a malformed length

```bash
forge test --match-test testFuzz_WrongProofLengthRejects -vv 2>&1 | tail -5
forge test --match-test "testVerifyInvalidProofLength" -vv 2>&1 | tail -10
```

✅ Expected: every length other than 256 (deposit: 288 = 256 + 32 commit) is rejected.

#### J.5 Submit a single-byte-mutated Groth16 proof

```bash
forge test --match-test "testE2E_L5_corruptedProof|testFuzz_SingleByteMutationReverts|test_CorruptedProofPointReverts|test_CorruptedProofScalarReverts" -vv 2>&1 | tail -15
```

✅ Expected: every mutation rejected. The pairing equation is rigid — no nearby points work.

### Attack Group 2 — Recipient / Identity Binding (DEP-2, DEP-5)

#### J.6 Withdraw to a different address than the original sender

Attacker objective: deposit from account A, withdraw to attacker-controlled account B without a valid proof binding.

The bridge passes `publicInputs[1] = uint256(uint160(address(recipient)));` — the recipient appears as a public input. The deposit-prover circuit constrains `sender == recipient` (i.e. only the original depositor can withdraw to themselves, via a fresh recipient address they sign for). With the *real* Groth16 verifier:

```bash
forge test --match-test testFuzz_RandomProofAndInputsReject -vv 2>&1 | tail -5
```

Random recipient + valid-format proof → reject.

#### J.7 Withdraw to `address(0)`

```bash
cast send $BRIDGE "withdraw(address,uint256,uint256,uint256,bytes)" \
  0x0000000000000000000000000000000000000000 1ether 1 $BLOCKNUM 0xdeadbeef \
  --rpc-url $RPC --private-key $PK
```

✅ Expected: revert with selector `0x9c8d2cd2` (`InvalidRecipient()`).

This is checked **before** the proof verification, so it's a cheap fail.

### Attack Group 3 — Block Hash Manipulation (DEP-6, OR-1, OR-2, OR-3, FORK-1)

#### J.8 Provide a forged block hash via the user input

There is **no API** for the user to supply a block hash. The only input is `blockNumber`; the hash is read from the oracle. To verify:

```bash
grep -n "blockHeaderOracle.getBlockHash\|blockHash =" contracts/ethereum/src/AckiNackiBridge.sol
```

✅ Expected: exactly one assignment, `bytes32 blockHash = blockHeaderOracle.getBlockHash(blockNumber);`. No path through which the caller's bytes touch the public inputs.

#### J.9 Reference a future block

```bash
FUTURE=$((BLOCKNUM + 1000000))
cast send $BRIDGE "withdraw(address,uint256,uint256,uint256,bytes)" \
  $ME 1ether 1 $FUTURE 0xdeadbeef --rpc-url $RPC --private-key $PK
```

✅ Expected: revert (oracle returns `0`, bridge reverts with `InvalidBlockHash()`).

#### J.10 Reference a historical block (>256 ago) without a witness

The Axiom-backed oracle reverts on `getBlockHash(n)` for any `n` older than 256:

```bash
forge test --match-test test_GetBlockHash_HistoricalBlock -vv 2>&1 | tail -5
```

✅ Expected: pass — confirms the historical path requires a witness.

This means relayers must submit withdrawal proofs within ~51 minutes of the deposit (256 × 12 s). Older proofs require an Axiom witness path, which currently is not wired into `withdraw()` (deliberate, to keep the trust model minimal).

#### J.11 Substitute a fork-side block hash

If an attacker forks Ethereum, builds a fake deposit on the fork, and tries to submit a proof to the mainnet bridge:

- The mainnet `AxiomBlockHeaderOracle.getBlockHash` returns the **mainnet** hash for that block number.
- The attacker's fork-side proof embeds the fork hash.
- The two differ, the pairing fails.

This is FORK-3 by construction. To verify the property:

```bash
grep -A3 "blockHash = blockHeaderOracle" contracts/ethereum/src/AckiNackiBridge.sol
```

The hash is read from the oracle, not from caller input. There is no way for a fork-only block hash to enter the public input vector on the mainnet bridge.

### Attack Group 4 — `verifyBlock` State Injection (LH-1..LH-9, CC-1..CC-7, FORK-1..FORK-4)

#### J.12 Skip the chain anchor (LH-3 / CC-6)

Attacker objective: inject an arbitrary layer-hash state (e.g., one that mints fake balances) by submitting a tuple with a wrong `prevMaxLevelLayerHash`.

```bash
forge test --match-test testRevertOnPrevAnchorMismatch -vv 2>&1 | tail -10
forge test --match-test test_relayerLoop_anchorMismatch_reverts -vv 2>&1 | tail -10
```

✅ Expected: every wrong anchor is rejected with `PrevAnchorMismatch(supplied, stored)`. The chain anchor only allows updates that continue from the stored top-level hash.

#### J.13 Submit a proof with a stale BK set commitment (LH-2 / CC-3)

Attacker objective: use a proof signed by an old (compromised) BK committee.

```bash
forge test --match-test testRevertOnBkSetCommitmentMismatch -vv 2>&1 | tail -10
```

✅ Expected: rejected with `BkSetCommitmentMismatch(supplied, stored)` (cheap pre-crypto check). And: even if the caller supplies the matching `bkSetCommitment`, the proof's BLS witness must commit to the same Poseidon — if it doesn't, the gnark verifier returns `false`.

#### J.14 Lie about `numLayers` (LH-4)

```bash
forge test --match-test "testRevertOnZeroNumLayers|testRevertOnNumLayersAboveMax" -vv 2>&1 | tail -10
```

✅ Expected: rejected with `InvalidNumLayers(numLayers)`. `numLayers` is range-checked at the contract level (1..=10) **and** is bound into Circuit 2's PI[2] inside the proof.

#### J.15 Inject silent garbage in unused layer slots (LH-5 / CC-7)

Attacker objective: with `numLayers = 3`, set `layerHashes[3..10]` to non-zero values, hoping the bridge stores them and they leak into a future call.

```bash
forge test --match-test testRevertOnLayerHashTailNonZero -vv 2>&1 | tail -10
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
forge test --match-test "testHappyPathPrimary" -vv  # passes by construction
```

The bound-test-data harness specifically *prevents* generating fixtures with diverging
`block_id` between Circuit 1A and Circuit 2; you'd need to hand-craft the negative case.

#### J.18 Swap Primary and Fallback proofs (LH-1)

Attacker objective: send a Primary proof but with `finType = Fallback` (or vice versa), routing it through the wrong verifier.

```bash
forge test --match-test "testRevertOnPrimaryProofViaFallbackPath|testRevertOnFallbackProofViaPrimaryPath" -vv 2>&1 | tail -10
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

Attacker objective: the recipient is a smart contract that calls `withdraw` again during the ETH transfer, draining the treasury.

`recipient.transfer(amount)` uses the EVM `transfer` opcode with a 2300-gas stipend — not enough to make a `call` back into the bridge. As an added defence, `withdraw()` is `nonReentrant`.

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

| # | Attack | Expected revert / property | Test |
|---|---|---|---|
| J.1 | Replay withdraw | `DepositAlreadyProcessed` | `testWithdrawalDoubleSpend` |
| J.2 | Reuse proof for another deposit ID | Cryptographic rejection | `testFuzz_RandomProofAndInputsReject` |
| J.3 | Cross-contract proof | Pairing fails (contractAddress in PI) | source review |
| J.4 | Wrong proof length | False / `InvalidProofLength` | `testFuzz_WrongProofLengthRejects` |
| J.5 | Single-byte-mutated proof | Pairing fails | `testE2E_L5_corruptedProof` |
| J.6 | Different recipient | Pairing fails | `testFuzz_RandomProofAndInputsReject` |
| J.7 | Recipient = address(0) | `InvalidRecipient` | `testWithdrawalInvalidRecipient` |
| J.8 | Forged block hash via user input | impossible (no API) | source review |
| J.9 | Future block | `InvalidBlockHash` (oracle returns 0) | `test_GetBlockHash_FutureBlock` |
| J.10 | Historical block without witness | revert | `test_GetBlockHash_HistoricalBlock` |
| J.11 | Fork block hash | Pairing fails (oracle returns canonical) | source review |
| J.12 | Skip chain anchor | `PrevAnchorMismatch` | `testRevertOnPrevAnchorMismatch` |
| J.13 | Stale BK commitment | `BkSetCommitmentMismatch` (cheap) + pairing fails | `testRevertOnBkSetCommitmentMismatch` |
| J.14 | Wrong `numLayers` | `InvalidNumLayers` | `testRevertOnZeroNumLayers` / `testRevertOnNumLayersAboveMax` |
| J.15 | Tail garbage in `layerHashes` | `LayerHashTailNonZero` | `testRevertOnLayerHashTailNonZero` |
| J.16 | Replay / rewind seqno | `BlockSeqNoNotMonotonic` | `test_relayerLoop_replaySameSeqNo_reverts` |
| J.17 | Mix proofs from two blocks (CC-1) | gnark `false` ⇒ `*ProofRejected` | by construction in `testHappyPathPrimary` |
| J.18 | Swap Primary↔Fallback (LH-1) | gnark `false` ⇒ `AttestationProofRejected` | per-route mismatch test |
| J.19 | Disable verifier slot post-deploy | impossible (immutable) | source review |
| J.20 | Forge BK-set rotation (Phase 1.C, pending) | no surface; redeploy required | source review (no `rotateBkSet`) |
| J.21 | Reenter via recipient | 2300 gas stipend + `nonReentrant` | source review |
| J.22 | Reenter via AAVE | `nonReentrant` everywhere | source review |
| J.23 | Non-owner privileged calls | `NotOwner` (every function) | per-function tests |
| J.24 | Old owner after transfer | `NotOwner` | `test_transferOwnership_flowsAllAuthorities` |
| J.25 | Input ≥ field modulus | Verifier rejects | `testFuzz_InputsAboveFieldModulusRevert` |
| J.26 | Wrong input count | False / revert | `testFuzz_WrongInputCountRejects` |
| J.27 | Random calldata | False / revert | fuzz suites |
| J.28 | Deposit > 100 ETH | `DepositTooLarge` | `testDepositTooLarge` |
| J.29 | Owner sets reserve = 100% | `ReserveBpsTooHigh` (capped at 50%) | `test_setLiquidReserveBps_capped` |
| J.30 | Drain via withdrawFromAave | No path; only rebalances | source review |
| J.31 | Drain via harvestYield overdraw | `NoYield` | `test_harvestYield_amountExceedsYieldReverts` |
| J.32 | Direct ETH transfer | Phantom liquidity, no claim | manual |

✅ All 30+ attack vectors blocked at HEAD. (J.20a is forward-looking for Phase 1.C.) If any of these unexpectedly succeeds, **stop and escalate**.

### Where attacks could theoretically still work

To be honest about residual risk:

- **51% attack on Ethereum L1**: would let an attacker rewrite history that the oracle reads. Out of scope for any L1 dApp.
- **>2/3 attack on Acki Nacki BK set**: would let attackers forge BLS-signed Primary attestations. Out of scope for any chain-to-chain bridge — see FORK-2 / K-9 in `audit_trail_v2.md`.
- **>1/2 attack on Acki Nacki BK set with Fallback path enabled**: a smaller adversarial threshold (1/2+1) suffices to forge Fallback-finalized blocks. The bridge accepts Fallback-attested updates by design (the AN protocol does too).
- **G2 subgroup gap (audit FORK-2 / BLS-1 / K-6)**: open audit finding; attacker would need a non-trivial G2 element in the wrong subgroup with a forged-looking signature. **Carries over to v2's Circuit 1A and 1B unchanged.** Launch blocker for mainnet; testnet OK.
- **gnark wrapper stub `Define` (deposit side only)**: the deposit wrapper currently produces a Groth16 proof committing to the public inputs without verifying the underlying Halo2 proof inside Groth16. The v2 AN→ETH wrappers (1A/1B/2) inherit the same shape — covered by `audit_trail_v2.md` K-4. Mitigation: the prover never publishes `pk_groth16` (kept in a single trusted prover service). Final mitigation will be full in-circuit Halo2 verification (`integration_plan.md` §6.5).
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

- [ ] `forge test` reports **135 passed; 0 failed; 0 skipped** across **15 suites**.
- [ ] `AckiNackiBridgeVerifyBlockTest` (17) + `AckiNackiBridgeRelayerLoopTest` (6) pass — `verifyBlock` happy paths and 6 sequencing scenarios.
- [ ] `AckiNackiBridgeAaveTest` (23 tests) passes — including the fuzz solvency invariant.

### Property checks (Phase E hands-on)

- [ ] Deposit succeeds, increments `depositCounter`, increases `treasuryBalance`, emits `Deposit`.
- [ ] Withdraw with valid (test) proof succeeds and decrements treasury.
- [ ] Double-spend (same `depositId`) reverts with `DepositAlreadyProcessed`.
- [ ] Withdrawal to `address(0)` reverts with `InvalidRecipient`.
- [ ] Empty proof reverts with `InvalidProof`.
- [ ] Future block reverts.

### `verifyBlock` (Phase F hands-on)

- [ ] Real bound proof tuple `testHappyPathPrimary` verifies on-chain (~440k gas).
- [ ] `testHappyPathFallback` verifies on-chain through `FallbackVerifier`.
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
- [ ] `testRevertOnBkSetCommitmentMismatch` confirms wrong-committee proofs are rejected.

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

All ≥ 30 attack scenarios in Phase J reach their expected revert / rejection. Group totals:

- [ ] **Group 1 (Proof forgery, J.1–J.5)** — 5 attacks blocked.
- [ ] **Group 2 (Identity binding, J.6–J.7)** — 2 attacks blocked.
- [ ] **Group 3 (Block hash, J.8–J.11)** — 4 attacks blocked.
- [ ] **Group 4 (`verifyBlock` injection, J.12–J.19)** — 8 attacks blocked (LH-#, CC-#, FORK-#).
- [ ] **Group 5 (BK rotation abuse, J.20)** — 1 attack blocked at HEAD; J.20a deferred to Phase 1.C.
- [ ] **Group 6 (Reentrancy, J.21–J.22)** — 2 attacks blocked.
- [ ] **Group 7 (Access control, J.23–J.24)** — every owner-only function rejects non-owners.
- [ ] **Group 8 (Field/encoding, J.25–J.27)** — 3 fuzz suites pass.
- [ ] **Group 9 (Economic/DoS, J.28–J.32)** — 5 attacks blocked.
- [ ] Residual risks (51% on L1, >2/3 on AN, G2 subgroup, gnark stub, trusted setup, no-rotation gap) acknowledged and tracked in `docs/audit_trail_v2.md` §2 and `docs/an_partner_integration_plan.md` §5.

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
| `testHappyPathPrimary` fails to load bound proof JSON | bound-proof artifacts deleted or never generated | Run `cargo run -p bridge-prover-orchestrator --bin export-bound-block-proofs --release` (~10-15 min). The Foundry test reads `crates/bridge-prover-orchestrator/exports/bound_scenario.json` + `groth16_proof_*.hex`. |
| `forge test` 1 fewer test than expected | filter / rename / removed test | Run `forge test --list` and diff against the table in §3 |
| Anvil session: `withdraw` reverts unexpectedly | `BLOCKNUM` is the current block (no hash yet) — use `block.number - 1` | re-export `BLOCKNUM=$((... - 1))` |
| Anvil session: `cast send` complains about gas | account out of ETH | use a different prefunded account |
| Fork test fails with "no upstream" or 401 | bad RPC URL | use a paid Alchemy / Infura key; public RPCs sometimes block historical reads |

---

## 13. After This Runbook

If everything in §11 ticks:

- The bridge is correct in the sense exercised by **135 unit/fuzz/E2E tests**, a **complete manual walk-through**, and **≥ 30 deliberate attack scenarios** that all fail in the expected way.
- You have personally observed:
  - A real deposit event being emitted.
  - A real Groth16 deposit proof being verified on-chain.
  - A real bound Groth16 tuple (Circuit 1A + Circuit 2) being verified through `verifyBlock`.
  - A double-spend being rejected.
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
