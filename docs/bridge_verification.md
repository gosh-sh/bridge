# Acki Nacki Bridge — Verification Guide

This article is the operational checklist for proving that the bridge is correct: every deposit on Ethereum maps to at most one withdrawal, every Acki Nacki layer-hash update reflects a real BLS-attested block, and no off-path actor can forge state. It complements two narrower documents:

- `aave_integration.md` — verifies the AAVE V3 yield bolt-on.
- `layer_hashes_circuit_audit.md` — audits the partner's Halo2 circuit internals.

This doc is the **end-to-end view**. Every section follows the pattern: *what the property is → why it holds → how to verify it ran correctly today*.

---

## 1. Scope

**In scope** (everything the bridge contracts and ZK pipeline are responsible for):

- ETH → AN: `AckiNackiBridge.deposit()` and `AckiNackiBridge.withdraw()`, including the deposit ZK proof and double-spend nullifier.
- AN → ETH: `LayerHashBridge.updateLayerHashes()` and the layer-hash ZK proof.
- BK set commitment lifecycle: `rotateBkSet` (ZK-proven) and the timelocked owner fallback.
- Block hash oracle (`AxiomBlockHeaderOracle` + the EVM `blockhash()` opcode).
- Cross-cutting properties: ZK verifier integrity, reentrancy, access control, fork resistance.
- AAVE V3 integration: covered in detail in `docs/aave_integration.md`; this doc only summarises how it interacts with the rest.

**Out of scope** (verified elsewhere):

- The internal soundness of the partner's Halo2 circuit (see the audit). We treat the gnark-wrapped Groth16 verifier as the trusted boundary.
- Acki Nacki node-level consensus and BK economic security.
- Off-chain relayer correctness (it can be byzantine — a wrong relayer cannot forge state, only stall).

---

## 2. Bridge at a Glance

```
            Ethereum                                          Acki Nacki
            ────────                                          ──────────
deposit()  ─►  Deposit event  ─►  deposit-prover (Halo2)  ─►  AN side
                                  ─► gnark wrapper (Groth16)
                                  ─► cross-chain message
                                  ─► claim minted on AN

withdraw()  ◄─  ZK proof of past Deposit event              ◄─  AN burns/sends
            (verified against blockHeaderOracle hash,
             nullified by depositId)

updateLayerHashes() ◄── BLS-attested AN block ──            ◄─  AN consensus
                       layer-hashes-update-halo2-circuit
                       Groth16 wrap, 13 public inputs
                       chain-anchored to stored layer hash

rotateBkSet() ◄── ZK proof: old BK set attested new ──      ◄─  AN governance
            └── timelocked owner fallback (7 days) ──┘
```

Two contracts are the trust anchors:

- `AckiNackiBridge.sol` — custodies user ETH, optionally yielding via AAVE.
- `LayerHashBridge.sol` — anchors Acki Nacki state, rotates the BK set commitment.

Everything else (`Groth16DepositVerifier`, `LayerHashVerifier`, `BkSetRotationVerifier`, `AxiomBlockHeaderOracle`, the gnark-generated `Groth16Verifier` family) is plumbing that sits between these two and the proof artifacts.

---

## 3. Verification Levels

| Level | What it answers | Cost |
|---|---|---|
| L0 — code review | Are the invariants present in source? | minutes |
| L1 — `forge build` + `forge fmt --check` | Does it compile and conform to style? | seconds |
| L2 — `forge test` (mocks only) | Do unit/fuzz/E2E tests pass? | ~5 s — 1 min |
| L3 — real-proof E2E | Do real Halo2 → Groth16 → on-chain proofs verify? | already cached: < 1 min; from scratch: ~33 min keygen+proving |
| L4 — fork test (recommended pre-deploy) | Does the bridge cooperate with real mainnet AAVE / Axiom? | minutes per scenario |
| L5 — post-deployment `cast` checks | Were the immutables wired correctly on-chain? | seconds |
| L6 — runtime monitoring | Has any invariant drifted? | continuous |

Run **L0–L3** for every PR. Run **L4–L5** for every deployment. Run **L6** continuously in production.

---

## 4. ETH → AN Deposit / Withdrawal

### 4.1 What must hold

| Property | Statement |
|---|---|
| **DEP-1** | Every successful `deposit()` emits a `Deposit(depositId, sender, amount, timestamp)` with a unique monotonic `depositId`. |
| **DEP-2** | A successful `withdraw()` requires a Groth16 proof binding `(depositId, recipient, amount, bridgeAddr, blockHash)` to a real Deposit event in the receipt trie of an Ethereum block whose hash is canonical. |
| **DEP-3** | `processedDeposits[depositId]` is set before the ETH transfer and is checked before processing — no `depositId` can be replayed. |
| **DEP-4** | `treasuryBalance` is decremented before any external call (CEI). |
| **DEP-5** | `recipient` cannot be `address(0)`. |
| **DEP-6** | The `blockHash` used as the proof's public input is read from the oracle, never from caller input. |

### 4.2 Why each property holds

- **DEP-1**: `depositCounter++` is `uint256` and only increments. At 1 deposit per second it would take ~10⁷⁰ years to overflow. There is no decrement path.
- **DEP-2**: `withdraw()` builds the public input vector itself; the verifier is an `immutable` reference. The Groth16 verifier's pairing equation passes only if the prover knew witnesses to all the circuit's constraints (RLP decoding of the receipt, MPT inclusion, log selectors, and `keccak256(blockHeader) == blockHash`).
- **DEP-3**: see the function body — the order is `processedDeposits[id] = true` (or check) → `treasuryBalance -= amount` → external call. CEI is preserved across the AAVE shortfall pull and the final `recipient.transfer`.
- **DEP-4**: same as DEP-3.
- **DEP-5**: explicit `revert InvalidRecipient()` at the top of `withdraw()`.
- **DEP-6**: `bytes32 blockHash = blockHeaderOracle.getBlockHash(blockNumber);` — caller does not supply the hash.

### 4.3 How to verify

#### L0 — code review

Open `contracts/ethereum/src/AckiNackiBridge.sol` and confirm by inspection:

```bash
grep -n "depositCounter\|processedDeposits\|treasuryBalance\|recipient.transfer\|getBlockHash" \
  contracts/ethereum/src/AckiNackiBridge.sol
```

Expected ordering inside `withdraw()`:

1. `recipient == address(0)` check
2. `blockHeaderOracle.getBlockHash(blockNumber)` — public input source
3. `verifier.verifyWithdrawalProof(proof, publicInputs)` — cryptographic check
4. `processedDeposits[depositId]` check
5. `treasuryBalance -= amount` — accounting effect
6. (optional) `_pullFromAave(...)` — only if liquid balance is short
7. `recipient.transfer(amount)` — external interaction last

#### L2 — tests

```bash
cd contracts/ethereum
forge test --match-contract "AckiNackiBridgeV2Test|FuzzAckiNackiBridgeTest|FuzzGroth16DepositVerifierTest|FuzzGroth16VerifierTest" -vv
```

Expected mappings:

| Property | Tests that enforce it |
|---|---|
| DEP-1 | `testDeposit`, `testDepositMultiple`, `testDepositCounterIncrement`, `testDepositFromDifferentUsers` |
| DEP-2 | `testWithdrawal`, `FuzzGroth16VerifierTest::*` (random proofs reject), `FuzzGroth16DepositVerifierTest::*` |
| DEP-3 | `testWithdrawalDoubleSpend`, `testFuzz_DoubleSpendReverts`, `testIsDepositProcessed` |
| DEP-4 | `testWithdrawalInsufficientTreasury`, `testFuzz_DepositAmountInvariants`, `testFuzz_MultipleDepositsInvariant` |
| DEP-5 | `testWithdrawalInvalidRecipient`, `testFuzz_WithdrawToZeroAddressReverts` |
| DEP-6 | `AxiomBlockHeaderOracleTest::*` (oracle alone) + `testWithdrawal` (bridge wires oracle correctly) |

#### L3 — real Groth16 proof

The deposit pipeline currently uses a stubbed Groth16 wrapper (see `integration_plan.md` §6.5). For the layer-hash side, real proofs are exercised in L3 (next section).

#### L4 — fork test (block hash oracle)

```bash
forge test --fork-url $ETH_RPC_URL --match-contract AxiomBlockHeaderOracleTest
```

This exercises the Axiom V2 Core path against a recent mainnet block.

#### L5 — post-deployment

```bash
cast call $BRIDGE "verifier()(address)"
cast call $BRIDGE "blockHeaderOracle()(address)"
cast call $BRIDGE "depositCounter()(uint256)"
cast call $BRIDGE "treasuryBalance()(uint256)"
cast call $BRIDGE "MAX_DEPOSIT_AMOUNT()(uint256)"   # 100000000000000000000 (100 ether)
```

---

## 5. AN → ETH Layer-Hash Updates

### 5.1 What must hold

| Property | Statement |
|---|---|
| **LH-1** | Every successful `updateLayerHashes()` is preceded by a Groth16 proof that the circuit's 13 public inputs are consistent: `[bkSetCommitment, numLayers, layerHashes[0..10], prevMaxLevelLayerHash]`. |
| **LH-2** | `bkSetCommitment` used for verification equals the bridge's *current* `currentBkSetCommitment` — proofs cannot be back-dated to a stale committee. |
| **LH-3** | After the first successful update, `prevMaxLevelLayerHash` must equal the previously stored `currentLayerHashes[currentNumLayers - 1]` — the **chain anchor**. |
| **LH-4** | `numLayers ∈ [1, MAX_LAYERS]` (i.e., 1..10) — out-of-range counts revert. |
| **LH-5** | The proof is exactly 256 bytes (raw Groth16). Anything else returns `false` from the verifier and reverts in the bridge. |
| **LH-6** | The verifier never reverts on invalid proofs — it returns `false` so the bridge can produce a clean `InvalidProof` revert. |

### 5.2 Why each property holds

- **LH-1**: `LayerHashBridge.updateLayerHashes()` constructs the verifier call itself; the bridge's stored `verifier` is an `address` set at construction or via `setVerifier` (owner-only, audited path).
- **LH-2**: `bkSetCommitment` is read from contract storage, not from caller input. The user submits only `numLayers`, `newLayerHashes`, `prevMaxLevelLayerHash`, and `proof`.
- **LH-3**: explicit check in source (`PrevHashMismatch(expected, got)` revert). For the very first update (`currentNumLayers == 0`), any prev-hash is accepted (genesis), but the proof must still verify.
- **LH-4**: explicit check `if (numLayers == 0 || numLayers > MAX_LAYERS) revert InvalidNumLayers()`.
- **LH-5**: `LayerHashVerifier.verifyLayerHashUpdate` returns `false` if `proof.length != 256`.
- **LH-6**: The adapter wraps `groth16Verifier.verifyProof(...)` in a `try/catch`. Calling `verifyProof` throws on invalid proofs (gnark default), and the catch normalises that to `false`.

### 5.3 How to verify

#### L0 — read the function

`LayerHashBridge.updateLayerHashes` is short (~30 lines). Verify in order:

1. `numLayers` range check
2. Chain-anchor check (when `currentNumLayers > 0`)
3. `verifier.verifyLayerHashUpdate(...)` against **current** commitment
4. State write (`currentLayerHashes`, `currentNumLayers`, `updateCount++`)
5. Event emit

#### L2 — tests

```bash
forge test --match-contract "LayerHashBridgeTest|LayerHashVerifierTest" -vv
```

| Property | Tests |
|---|---|
| LH-1 | `testFirstUpdate`, `testSequentialUpdates` |
| LH-2 | `testZkRotation` then layer update — see `testLayerUpdateThenRotationThenLayerUpdate` |
| LH-3 | `testRevertOnPrevHashMismatch`, `testE2E_L2_H32_wrongPrevHash` |
| LH-4 | `testRevertOnTooManyLayers`, `testRevertOnZeroNumLayers` |
| LH-5 | `LayerHashVerifierTest::testVerifyInvalidProofLength` |
| LH-6 | `LayerHashVerifierTest::testVerifyRevertingGroth16` (the catch path) |

#### L3 — real proofs (the killer test)

```bash
forge test --match-contract "LayerHashE2ETest" -vv
```

This is **the** integration test. It loads the four real Groth16 proofs from `layer-hashes-prover/proofs/groth16/` (produced from real Acki Nacki node data via the full Halo2 → gnark pipeline) and submits them through the bridge. Each fixture exercises a different shape:

| Fixture | What it tests |
|---|---|
| `L2_H16_prevH0` | Genesis-anchored update, 2 layers |
| `L2_H32_prevH16` | Sequential update from `L2_H16` (chain anchor must match) |
| `L5_H12288_prevH1024` | 5-layer, long prev-chain (11 steps) |
| `L6_H45056_prevH0` | Largest, 6 layers, genesis anchor |

Plus negative tests: wrong `bkSetCommitment`, wrong `numLayers`, wrong layer hash, wrong `prevHash`, byte-mutated proof — every case rejected with `InvalidProof`.

#### L5 — post-deployment

```bash
cast call $LH_BRIDGE "verifier()(address)"
cast call $LH_BRIDGE "currentBkSetCommitment()(uint256)"
cast call $LH_BRIDGE "currentNumLayers()(uint256)"
cast call $LH_BRIDGE "MAX_LAYERS()(uint256)"        # 10
cast call $LH_BRIDGE "COMMITMENT_TIMELOCK()(uint256)" # 604800 (7 days)
```

#### L6 — monitoring invariants

```
LH(t) = LH(t-1)  OR  prev_hash_in_event_t == LH[currentNumLayers-1]_at_time_(t-1)
```

That is, every `LayerHashesUpdated` event's `prevHash` equals the previous top-level hash. A relayer/monitor that sees an event violating this should alert immediately — it would mean state corruption.

---

## 6. BK Set Rotation

### 6.1 What must hold

| Property | Statement |
|---|---|
| **BK-1** | `rotateBkSet(proof, newCommitment)` succeeds only if `bkRotationVerifier.verifyRotation(proof, currentCommitment, newCommitment)` returns `true`. |
| **BK-2** | If `bkRotationVerifier == address(0)`, the call reverts (`InvalidVerifier`). |
| **BK-3** | `rotateBkSet` is permissionless — anyone can submit a valid proof. |
| **BK-4** | The owner-only timelocked path (`proposeBkSetCommitment` → `executeBkSetCommitment`) requires `block.timestamp ≥ commitmentActivationTime` to take effect. |
| **BK-5** | `executeBkSetCommitment` is permissionless once the timelock has passed (so the owner cannot grief by refusing to flip the switch). |
| **BK-6** | `cancelBkSetCommitment` is owner-only — only the same authority that proposed can cancel. |
| **BK-7** | A pending proposal can be overwritten by a fresh `propose` call (resetting the timelock to a new 7-day window). This is intentional: the owner can change their mind, but the new window is also 7 days. |

### 6.2 Why each property holds

Read `LayerHashBridge.sol` lines 119–168. Each property maps directly to a check or modifier in the source.

**BK-2 nuance**: the bridge can be deployed without a rotation verifier set (e.g., before the Halo2 BK rotation circuit is finished). In that case `rotateBkSet` permanently reverts and rotation is only available via the timelock path. This is the current production state.

### 6.3 How to verify

#### L2 — tests

```bash
forge test --match-contract "BkSetRotationVerifierTest|LayerHashBridgeTest" -vv
```

| Property | Tests |
|---|---|
| BK-1 | `testZkRotation`, `BkSetRotationVerifierTest::testVerifyValidRotation` |
| BK-2 | `testZkRotationNoVerifierSet` |
| BK-3 | `testZkRotationCallableByAnyone` |
| BK-4 | `testTimelockProposeThenExecute`, `testTimelockExecuteBeforeExpiry` |
| BK-5 | `testTimelockExecuteCallableByAnyone` |
| BK-6 | `testTimelockCancel`, `testTimelockCancelOnlyOwner` |
| BK-7 | `testTimelockOverwritesPrevious` |

Plus negative paths: `testZkRotationInvalidProof`, `testTimelockExecuteWithNoPending`, `testTimelockCancelWithNoPending`, `BkSetRotationVerifierTest::testVerifyInvalidProofLength`, `BkSetRotationVerifierTest::testVerifyRevertingGroth16`.

#### L5 — post-deployment

```bash
cast call $LH_BRIDGE "bkRotationVerifier()(address)"        # address(0) until circuit ships
cast call $LH_BRIDGE "pendingBkSetCommitment()(uint256)"    # 0 unless proposal active
cast call $LH_BRIDGE "commitmentActivationTime()(uint256)"  # 0 unless proposal active
```

---

## 7. Block Hash Oracle (Anti-Fork)

### 7.1 What must hold

| Property | Statement |
|---|---|
| **OR-1** | For blocks within the last 256 of `block.number`, `getBlockHash(n)` returns the value of the EVM `blockhash(n)` opcode — i.e., the hash of the canonical chain the contract is executing on. |
| **OR-2** | For blocks beyond 256 ago, `getBlockHash(n)` reverts with `"Use verifyBlockHash() for historical blocks"`. The historical path requires a witness verified by `AxiomV2Core.isBlockHashValid`. |
| **OR-3** | `getBlockHash(n)` for `n >= block.number` reverts (`BlockNotYetMined`). |
| **OR-4** | The Axiom V2 Core address is set at construction time and is `immutable`. |

### 7.2 Why each property holds

Direct from `AxiomBlockHeaderOracle.sol`. The two-tier design intentionally fails closed: if neither tier can produce a hash, the call reverts and `withdraw()` cannot proceed.

### 7.3 Fork resistance reasoning

If two Ethereum chains exist (mainnet vs an attacker's fork):

- The bridge contract is deployed on **one specific chain** — there is no cross-chain bytecode sharing.
- `blockhash()` returns hashes from the chain the EVM is executing on. On the mainnet bridge instance, only mainnet hashes are available.
- Axiom V2 Core's update proofs are themselves chained back to a trusted root that lives on the same chain. A fork-specific Axiom would only know fork-specific block hashes.

So a "fork attack" only succeeds if the attacker can publish state on the **same Ethereum** the bridge is on — i.e., a 51% reorg of mainnet itself. That is a layer-1 consensus attack, not a bridge-specific vulnerability.

### 7.4 How to verify

#### L2 — tests

```bash
forge test --match-contract AxiomBlockHeaderOracleTest -vv
```

16 tests cover all four tiers (recent, historical, future, malformed) including event emission.

#### L4 — fork test

```bash
forge test --fork-url $ETH_RPC_URL --match-contract AxiomBlockHeaderOracleTest -vvv
```

Confirms the contract behaves correctly against the real `AxiomV2Core` deployment.

#### L5 — post-deployment

```bash
cast call $ORACLE "axiomV2Core()(address)"
# expected mainnet: 0x69963768F8407dE501029680dE46945F838Fc98B
cast call $ORACLE "MAX_BLOCKHASH_AGE()(uint256)"   # 256
cast call $ORACLE "getBlockHash(uint256)" $((BLOCK_NOW - 1))   # should return non-zero
```

---

## 8. ZK Verifier Plumbing

### 8.1 What must hold

| Property | Statement |
|---|---|
| **ZK-1** | The Groth16 verifier contract is auto-generated by gnark from the wrapping circuit and not subsequently edited. |
| **ZK-2** | The adapter contracts (`Groth16DepositVerifier`, `LayerHashVerifier`, `BkSetRotationVerifier`) wrap the `verifyProof` call in `try/catch` so reverts are normalised to `false`. |
| **ZK-3** | The adapter never builds a public-input vector larger than the circuit allows; layout matches the gnark VK. |
| **ZK-4** | Adapter constructors reject `address(0)` for the underlying Groth16 verifier. |

### 8.2 How to verify

#### L0 — diff against gnark output

```bash
diff -u contracts/ethereum/src/Groth16Verifier.sol \
  <(deposit-prover/gnark-wrapper/gnark-wrapper setup ... | tee /dev/stderr)
```

This is a one-shot manual check whenever the deposit circuit changes. The same applies to `LayerHashGroth16VerifierGenerated.sol` for the layer-hash side.

#### L2 — fuzz tests

```bash
forge test --match-contract "FuzzHalo2VerifierTest|FuzzGroth16VerifierTest|FuzzGroth16DepositVerifierTest" -vv
```

These exercise the verifier with thousands of malformed/random calldata payloads — every one must be rejected. Coverage:

- Random proof bytes
- Single-byte mutations
- Truncated calldata
- Inputs above the BN254 field modulus
- Generator-point identity proofs (would otherwise pass the trivial check)

#### L2 — adapter sanity

```bash
forge test --match-test "testVerifyInvalidProofLength|testVerifyRevertingGroth16|testConstructorRejectsZeroAddress" -vv
```

Exercises ZK-2, ZK-3, ZK-4 across all three adapters.

---

## 9. Reentrancy & Access Control

### 9.1 What must hold

| Property | Statement |
|---|---|
| **AC-1** | `AckiNackiBridge.deposit` and `withdraw` are `nonReentrant`. |
| **AC-2** | All AAVE-management functions on `AckiNackiBridge` are `onlyOwner` and `nonReentrant`. |
| **AC-3** | `LayerHashBridge.updateLayerHashes` and `rotateBkSet` are permissionless (no modifier other than the proof check). |
| **AC-4** | `LayerHashBridge.proposeBkSetCommitment`, `cancelBkSetCommitment`, `setBkRotationVerifier`, `setVerifier`, `transferOwnership` are `onlyOwner`. |
| **AC-5** | `LayerHashBridge.executeBkSetCommitment` is permissionless once the timelock has passed (anti-grief). |
| **AC-6** | All external interactions follow CEI: state mutation precedes external call. |

### 9.2 How to verify

```bash
grep -n "modifier\|onlyOwner\|nonReentrant" contracts/ethereum/src/AckiNackiBridge.sol \
                                              contracts/ethereum/src/LayerHashBridge.sol
```

Cross-reference each external function with the matrix above.

Tests:

```bash
forge test --match-test "OnlyOwner|NotOwner|nonReentrant|Reentrancy|Unauthorized" -vv
```

CEI by inspection: in `AckiNackiBridge.withdraw`, the line `processedDeposits[depositId] = true;` precedes both `_pullFromAave` and `recipient.transfer`. In `LayerHashBridge.updateLayerHashes`, the verifier call has no side effects (it's a `view`-style external call returning bool), and storage writes follow successful verification.

---

## 10. Acki Nacki Side Fork Resistance

### 10.1 What must hold

| Property | Statement |
|---|---|
| **FORK-1** | A proof must be signed by a BLS aggregate of the **current** BK set committee (committed via `currentBkSetCommitment`). |
| **FORK-2** | The aggregate must reach the BFT threshold (`3·signers ≥ 2·n` for primary) — this is enforced inside the Halo2 circuit. |
| **FORK-3** | Each layer-hash update is anchored to the previous top-level hash — an attacker cannot inject a discontinuous state. |
| **FORK-4** | The BK set commitment is `uint256` Poseidon hash of the sorted (signer_index, x-coordinate) tuples — collisions require breaking Poseidon. |

### 10.2 Why this is enough

If an attacker forks Acki Nacki and tries to push a fake layer-hash update to Ethereum:

1. They need a Groth16 proof. The circuit constraints require ≥2/3 BLS signatures from BKs whose Poseidon commitment matches `currentBkSetCommitment`.
2. If they control fewer than 1/3 of the BK set, they can't reach the threshold.
3. If they control more than 2/3, they're effectively the legitimate majority — that is a consensus failure, not a bridge bug.
4. Even a successful proof must satisfy the chain anchor: `prevMaxLevelLayerHash` must equal the bridge's current top-level hash. So the only "fork" they could inject is one that *continues* from current state — i.e., adversarial blocks they finalised themselves with majority control.

### 10.3 Where to inspect

- Threshold logic: `gosh-bls-verification` crate (audited; see `docs/layer_hashes_circuit_audit.md`).
- BK set commitment derivation: same crate, sorts by signer index then commits with Poseidon T=3 RATE=2.
- Chain anchor: `LayerHashBridge.updateLayerHashes` lines 92–97.

---

## 11. Reproducible End-to-End Verification Recipe

Below is a single ordered recipe a reviewer can run start-to-finish.

```bash
# ─── L1 + L2 ────────────────────────────────────────────────────
cd contracts/ethereum

forge build               # must compile with no errors
forge fmt --check         # must be clean
forge test                # 135 tests across 14 suites, all green

# ─── L3 — the real-proof E2E (proofs are committed) ───────────
forge test --match-contract LayerHashE2ETest -vv
# Expected: 14 tests pass, including 4 successful real Groth16 proof
# verifications and ~10 negative cases.

# ─── L0 — sanity grep for invariants ──────────────────────────
grep -n "treasuryBalance\|processedDeposits\|recipient.transfer" \
  src/AckiNackiBridge.sol
grep -n "currentBkSetCommitment\|prevMaxLevelLayerHash" src/LayerHashBridge.sol

# ─── L4 — fork tests (recommended pre-deploy) ─────────────────
forge test --fork-url $ETH_RPC_URL --match-contract AxiomBlockHeaderOracleTest -vvv

# ─── L5 — post-deployment cast checks ─────────────────────────
# Run these after `forge script` deployment (see DeployRealBridge.s.sol).
cast call $BRIDGE      "verifier()(address)"
cast call $BRIDGE      "blockHeaderOracle()(address)"
cast call $BRIDGE      "owner()(address)"
cast call $BRIDGE      "aaveEnabled()(bool)"          # true if USE_AAVE=true
cast call $LH_BRIDGE   "verifier()(address)"
cast call $LH_BRIDGE   "currentBkSetCommitment()(uint256)"
cast call $LH_BRIDGE   "MAX_LAYERS()(uint256)"
cast call $LH_BRIDGE   "COMMITMENT_TIMELOCK()(uint256)"  # 604800
cast call $ORACLE      "axiomV2Core()(address)"

# ─── L6 — monitoring queries (run continuously) ───────────────
# 1. Solvency (AAVE-aware)
echo "treasury  = $(cast call $BRIDGE 'treasuryBalance()(uint256)')"
echo "balance   = $(cast balance $BRIDGE)"
echo "aWETHbal  = $(cast call $aWETH 'balanceOf(address)(uint256)' $BRIDGE)"
# Invariant: balance + aWETHbal >= treasury

# 2. Layer-hash chain integrity
cast call $LH_BRIDGE "currentLayerHashes(uint256)(uint256)" $((LAYERS - 1))
# Persist and compare across blocks; transitions should be exactly the
# `prevHash` field of each LayerHashesUpdated event.

# 3. No stuck timelock proposals
cast call $LH_BRIDGE "commitmentActivationTime()(uint256)"
# Should be 0 in steady state. If non-zero and far in the past, run
# executeBkSetCommitment() to flip it (anti-grief: anyone can call).
```

---

## 12. What This Does *Not* Prove

To be explicit about the trust boundary:

| Outside this guide | Where it lives |
|---|---|
| Soundness of the Halo2 circuits proving Ethereum receipt inclusion / AN block attestation | `docs/layer_hashes_circuit_audit.md`, partner audits |
| BLS12-381 G2 subgroup gap (audit FORK-2 / BLS-1) | open audit finding, will be addressed in next circuit revision |
| Trusted setup of the KZG SRS for Halo2 | community-generated `kzg_bn254_19.srs`; verify checksum on download |
| Trusted setup of the gnark Groth16 wrapper | wrapping circuit has 14 constraints with stub `Define` — see `integration_plan.md` §6.5 |
| Acki Nacki BFT economic security | not a bridge concern; the bridge inherits whatever finality AN provides via the BK threshold |
| Off-chain relayer liveness / censorship | a malicious relayer can stall but cannot forge state; multiple competing relayers are sufficient |

When any of these change, this doc must be revisited and the affected sections updated.

---

## 13. Cross-References

- `docs/integration_analysis.md` — architecture analysis, contract ↔ circuit mapping.
- `docs/integration_plan.md` — milestones M0–M9 and risk register.
- `docs/layer_hashes_circuit_audit.md` — line-by-line circuit audit.
- `docs/aave_integration.md` — yield bolt-on verification (companion to §4 above).
- `docs/proof_metrics_report.md` — proof sizes, key sizes, gas costs per fixture.
- `bk-set-rotation-prover/CIRCUIT_SPEC.md` — spec for the Halo2 BK rotation circuit.
