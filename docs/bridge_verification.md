# Acki Nacki Bridge — Verification Guide (v2)

> **v2 update (2026-05-10).** Rewritten for the four-circuit architecture. Sections §5 and §6
> have been retargeted from the retired `LayerHashBridge.sol` (deleted in Phase 4.2) to
> `AckiNackiBridge.verifyBlock`. New §6.5 enumerates cross-circuit invariants (CC-#).
> Read `docs/four_circuit_architecture.md` first for context.

This article is the operational checklist for proving that the bridge is correct: every deposit on Ethereum maps to at most one withdrawal, every Acki Nacki block update reflects a real BLS-attested block whose `block_id` is bound across two ZK proofs, and no off-path actor can forge state. It complements two narrower documents:

- `aave_integration.md` — verifies the AAVE V3 yield bolt-on.
- `layer_hashes_circuit_audit.md` — audits the partner's Halo2 circuits.

This doc is the **end-to-end view**. Every section follows the pattern: *what the property is → why it holds → how to verify it ran correctly today*.

---

## 1. Scope

**In scope** (everything the bridge contracts and ZK pipeline are responsible for):

- ETH → AN: `AckiNackiBridge.deposit()` and `AckiNackiBridge.withdraw()`, including the deposit ZK proof and double-spend nullifier.
- AN → ETH: `AckiNackiBridge.verifyBlock()` and the tuple of two cross-circuit-bound ZK proofs (Circuit 1A or 1B + Circuit 2).
- BK-set commitment lifecycle: deferred to Phase 1.C (Circuit 3 / `bkSetUpdateProof`); the legacy 7-day timelocked owner fallback (`LayerHashBridge.proposeBkSetCommitment`) was **retired in Phase 4.2**. Until Phase 1.C ships, `storedBkSetCommitment` only changes via redeployment.
- Block-hash oracle (`AxiomBlockHeaderOracle` + the EVM `blockhash()` opcode) — used by the deposit-side `withdraw()`.
- Cross-cutting properties: ZK verifier integrity, reentrancy, access control, fork resistance.
- AAVE V3 integration: covered in detail in `docs/aave_integration.md`; this doc only summarises how it interacts with the rest.

**Out of scope** (verified elsewhere):

- The internal soundness of the partner's Halo2 circuits (see the audit). We treat the per-circuit gnark-wrapped Groth16 verifier as the trusted boundary.
- Acki Nacki node-level consensus and BK economic security.
- Off-chain relayer correctness (it can be byzantine — a wrong relayer cannot forge state, only stall).

---

## 2. Bridge at a Glance (v2)

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

verifyBlock(finType, attestationProof, layerHashesProof,    ◄── AN consensus
            blockId, bkSetCommitment,                          (Primary ≥ 2/3
            blockSeqNo, numLayers,                              or Fallback > 1/2)
            layerHashes, prevMaxLevelLayerHash)
        ◄── 1A / 1B + 2 (256 B each, gnark Groth16)
            cross-bound by blockId & bkSetCommitment
            chain-anchored to storedPrevMaxLevelLayerHash

verifyBkSetUpdate(...)                                       ◄── Phase 1.C, planned
        ◄── Circuit 3 proves (oldCommit, newCommit) hand-off    (Halo2 stub upstream)
```

The single trust anchor is now:

- `AckiNackiBridge.sol` — custodies user ETH (with optional AAVE yield), AND anchors Acki Nacki state via the `verifyBlock` surface. Phase 4.2 removed the legacy `LayerHashBridge.sol` and folded its responsibilities into this one contract.

Plumbing: `Groth16DepositVerifier`, `PrimaryVerifier`, `FallbackVerifier`, `LayerHashesMovementVerifier` (each calling its own `*Groth16VerifierGenerated.sol`), `AxiomBlockHeaderOracle`. All sit between `AckiNackiBridge` and the proof artifacts.

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

## 5. AN → ETH Block Updates (`verifyBlock`)

### 5.1 What must hold

| Property | Statement |
|---|---|
| **LH-1** | Every successful `verifyBlock(...)` is preceded by **two** Groth16 proofs: one attestation proof (Circuit 1A or 1B, depending on `finType`) and one layer-hashes-movement proof (Circuit 2). |
| **LH-2** | `bkSetCommitment` (the argument flowing into both verifier calls) equals the bridge's stored `storedBkSetCommitment` — proofs cannot be back-dated to a stale committee. |
| **LH-3** | `prevMaxLevelLayerHash` (the argument flowing into Circuit 2) equals the bridge's stored `storedPrevMaxLevelLayerHash` — the **chain anchor**. For the very first call after deployment this equals `genesisPrevMaxLevelLayerHash` (typically 0). |
| **LH-4** | `numLayers ∈ [1, MAX_LAYER_HASHES]` (i.e., 1..10) — out-of-range counts revert with `InvalidNumLayers`. |
| **LH-5** | `layerHashes[i] == 0` for every `i ≥ numLayers`. The bridge will revert with `LayerHashTailNonZero(i)` — the unused tail must not carry silent garbage. |
| **LH-6** | `blockSeqNo > storedLastSeenBlockSeqNo` — strict monotonicity. Replay attempts revert with `BlockSeqNoNotMonotonic`. |
| **LH-7** | Each verifier adapter (`PrimaryVerifier`, `FallbackVerifier`, `LayerHashesMovementVerifier`) returns `false` if `proof.length != 256`, and never reverts on invalid proofs — it normalises gnark reverts to `false` via try/catch so `verifyBlock` produces a clean `AttestationProofRejected` / `LayerHashesProofRejected` revert. |
| **LH-8** | After a successful call, `storedPrevMaxLevelLayerHash = layerHashes[numLayers - 1]` (the new top-of-chain becomes the anchor for the next call), `storedNumLayers = numLayers`, `storedLayerHashes = layerHashes`, `storedLastSeenBlockSeqNo = blockSeqNo`. There is no admin path that bypasses this. |
| **LH-9** | If any of the three verifier slots is `address(0)` at construction, `verifyBlock` reverts with `VerifyBlockDisabled`. The deposit/AAVE surface remains fully functional in that mode. |

### 5.2 Why each property holds

- **LH-1**: `verifyBlock` directly calls both verifier adapters; the verifier addresses are `immutable` and set in the constructor's `VerifyBlockConfig`. There is no setter that can replace them post-deployment.
- **LH-2**: `bkSetCommitment != storedBkSetCommitment ⇒ revert BkSetCommitmentMismatch`. The user-supplied `bkSetCommitment` is the same value passed to **both** verifier calls — the partner's circuits commit to it as a public input (offset 96 of the envelope tree), so a wrong value would also fail the gnark pairing.
- **LH-3**: explicit `prevMaxLevelLayerHash != storedPrevMaxLevelLayerHash ⇒ revert PrevAnchorMismatch`. The first-call genesis case is just `storedPrevMaxLevelLayerHash = genesisPrevMaxLevelLayerHash` seeded in the constructor.
- **LH-4**: explicit `if (numLayers == 0 || numLayers > MAX_LAYER_HASHES) revert InvalidNumLayers(numLayers)`.
- **LH-5**: explicit `for (i = numLayers; i < MAX_LAYER_HASHES; i++) if (layerHashes[i] != 0) revert LayerHashTailNonZero(i)` — guards against silent garbage in unused slots.
- **LH-6**: explicit `blockSeqNo <= storedLastSeenBlockSeqNo ⇒ revert BlockSeqNoNotMonotonic`.
- **LH-7**: each adapter wraps `groth16Verifier.verifyProof(...)` in try/catch, normalising gnark reverts to `false`. See `PrimaryVerifier.sol`, `FallbackVerifier.sol`, `LayerHashesMovementVerifier.sol`.
- **LH-8**: state writes happen *only* after both verifiers return `true` and all anchor checks pass. CEI-clean: there are no external calls between the writes and `BlockVerified` emission.
- **LH-9**: feature gate at the top of `verifyBlock`: any zero verifier address ⇒ revert.

### 5.3 How to verify

#### L0 — read the function

`AckiNackiBridge.verifyBlock` is ~85 lines. Verify in order (top-down, no skips):

1. Feature-gate the three verifier slots (LH-9).
2. Shape & range checks: `numLayers` in `[1, MAX_LAYER_HASHES]` (LH-4); `layerHashes` tail (LH-5).
3. Anchor checks against stored state: `bkSetCommitment` (LH-2), `blockSeqNo` (LH-6), `prevMaxLevelLayerHash` (LH-3).
4. Crypto: route to `primaryVerifier` or `fallbackVerifier` based on `finType`, then call `layerHashesVerifier`. Both must return `true` (LH-1, LH-7).
5. Effects (CEI): commit `storedLastSeenBlockSeqNo`, `storedNumLayers`, `storedLayerHashes`, `storedPrevMaxLevelLayerHash` (LH-8).
6. Emit `BlockVerified(blockId, blockSeqNo, finType, numLayers)`.

```bash
grep -n "verifyBlock\|stored\(BkSet\|LastSeen\|NumLayers\|LayerHashes\|PrevMaxLevel\)" \
  contracts/ethereum/src/AckiNackiBridge.sol
```

#### L2 — tests

```bash
cd contracts/ethereum
forge test --match-contract "AckiNackiBridgeVerifyBlockTest|AckiNackiBridgeRelayerLoopTest|PrimaryVerifierTest|FallbackVerifierTest|LayerHashesMovementVerifierTest" -vv
```

| Property | Tests |
|---|---|
| LH-1 | `AckiNackiBridgeVerifyBlockTest::testHappyPathPrimary`, `…::testHappyPathFallback` |
| LH-2 | `…::testRevertOnBkSetCommitmentMismatch`, `…::testRevertOnTamperedBkSetInProof` |
| LH-3 | `…::testRevertOnPrevAnchorMismatch`, `AckiNackiBridgeRelayerLoopTest::test_relayerLoop_anchorMismatch_reverts` |
| LH-4 | `…::testRevertOnZeroNumLayers`, `…::testRevertOnNumLayersAboveMax` |
| LH-5 | `…::testRevertOnLayerHashTailNonZero` |
| LH-6 | `AckiNackiBridgeRelayerLoopTest::test_relayerLoop_replaySameSeqNo_reverts`, `…::test_relayerLoop_lowerSeqNo_reverts` |
| LH-7 | `PrimaryVerifierTest::testVerifyInvalidProofLength`, `…::testVerifyRevertingGroth16`; same for Fallback / LayerHashesMovement |
| LH-8 | `AckiNackiBridgeRelayerLoopTest::test_relayerLoop_advancesAcrossTenBlocks`, `…::test_relayerLoop_restartRecoversFromState` |
| LH-9 | `AckiNackiBridgeVerifyBlockTest::testRevertWhenVerifyBlockDisabled` |

#### L3 — real proofs (single-block bound fixture)

```bash
forge test --match-test "testHappyPathPrimary" -vv
```

Drives `verifyBlock` end-to-end with a real bound proof set generated by:

```bash
cargo run -p bridge-prover-orchestrator --bin export-bound-block-proofs --release
```

The export binary writes `bound_scenario.json` plus `proof_*.bin` into `crates/bridge-prover-orchestrator/exports/`; the Foundry test loads them as hardcoded fixtures (regenerated on demand). Multi-block real-proof coverage (the legacy `LayerHashE2ETest` analogue) is deferred to **Phase 5.3**.

#### L5 — post-deployment

```bash
cast call $BRIDGE "primaryVerifier()(address)"
cast call $BRIDGE "fallbackVerifier()(address)"
cast call $BRIDGE "layerHashesVerifier()(address)"
cast call $BRIDGE "storedBkSetCommitment()(uint256)"
cast call $BRIDGE "storedLastSeenBlockSeqNo()(uint64)"
cast call $BRIDGE "storedNumLayers()(uint8)"
cast call $BRIDGE "storedPrevMaxLevelLayerHash()(uint256)"
cast call $BRIDGE "MAX_LAYER_HASHES()(uint256)"   # 10
cast call $BRIDGE "getStoredLayerHashes()(uint256[10])"
```

#### L6 — monitoring invariants

```
storedPrevMaxLevelLayerHash(t) == storedLayerHashes[storedNumLayers - 1](t)            # always
prevMaxLevelLayerHash_in_event_t == storedPrevMaxLevelLayerHash(t-1)                   # event continuity
storedLastSeenBlockSeqNo(t) > storedLastSeenBlockSeqNo(t-1)                            # strict monotonicity
```

A relayer/monitor that sees a `BlockVerified` event with a `prevMaxLevelLayerHash` parameter that doesn't match the prior `storedPrevMaxLevelLayerHash` should alert — it would indicate a state-machine break.

---

## 6. BK Set Rotation (Phase 1.C — pending)

### 6.1 Status

The legacy `LayerHashBridge.rotateBkSet` (ZK-proven via a 2-input Halo2 stub) **and** its 7-day timelocked owner fallback (`proposeBkSetCommitment` / `executeBkSetCommitment` / `cancelBkSetCommitment`) were retired in **Phase 4.2** (2026-05-10) along with the rest of `LayerHashBridge.sol`.

The v2 plan is **Phase 1.C**: extend `verifyBlock` (or add a sibling `verifyBkSetUpdate`) with an optional `bkSetUpdateProof` argument that, on a successful Circuit 3 pairing, advances `storedBkSetCommitment` from old → new in the same transaction. Until Phase 1.C ships:

- `storedBkSetCommitment` is **immutable post-deployment** in practice — there is no setter.
- BK-set rotation requires a **redeployment** of `AckiNackiBridge` with a new `genesisBkSetCommitment` until Circuit 3 is wired (and the relayer can produce the proof).
- This is intentional: it removes the "trusted owner can flip the committee" assumption that the legacy `LayerHashBridge.proposeBkSetCommitment` introduced.

### 6.2 What must hold (target invariants for Phase 1.C)

The following list is **forward-looking** and will be re-verified once Circuit 3 is wired:

| Property | Statement (target) |
|---|---|
| **BK-1** | `verifyBkSetUpdate(proof, oldCommitment, newCommitment)` succeeds only if Circuit 3's gnark verifier returns `true` AND `oldCommitment == storedBkSetCommitment`. |
| **BK-2** | If `bkSetUpdateVerifier == address(0)`, the call reverts (the slot is reserved but not yet filled). |
| **BK-3** | `verifyBkSetUpdate` is permissionless — anyone can submit a valid proof. |
| **BK-4** | After success, `storedBkSetCommitment = newCommitment` and a `BkSetCommitmentRotated(oldCommitment, newCommitment)` event is emitted. |
| **BK-5** | The transition is single-step: there is no "pending"/"executed" two-phase flow (the v1 timelock was a workaround for the absence of Circuit 3). |

### 6.3 How to verify (today)

There are no BK-set rotation tests at HEAD — the surface doesn't exist yet. The closest thing is the negative path in `AckiNackiBridgeVerifyBlockTest::testRevertOnBkSetCommitmentMismatch`, which confirms that `verifyBlock` rejects any proof signed by a different committee.

```bash
cast call $BRIDGE "storedBkSetCommitment()(uint256)"   # static between deployments
```

### 6.4 Why this is safe in the interim

The deposit/withdraw surface and the layer-hash advancement surface remain operational. The
only operational impact of "no rotation" is that if the AN-side BK set rotates, the bridge
must be redeployed with a fresh `genesisBkSetCommitment`. This is acceptable for testnet /
shellnet operation; production launch will gate on Phase 1.C.

---

## 6.5 Cross-Circuit Invariants (CC-#)

These are the v2-specific invariants binding two independent ZK proofs into a single block update. See `docs/four_circuit_architecture.md` §4 for the detailed mechanism.

| Property | Statement |
|---|---|
| **CC-1** | `blockId` is the same value committed by Circuit 1A/1B (PI[0]) and Circuit 2 (PI[0]). The bridge passes one argument to both verifier calls — any mismatch ⇒ at least one `verifyProof` returns `false` ⇒ `verifyBlock` reverts. |
| **CC-2** | `bkSetCommitment` is the same value committed by Circuit 1A/1B (PI[1]) and Circuit 2 (PI[1]). Same enforcement mechanism as CC-1. |
| **CC-3** | `bkSetCommitment == storedBkSetCommitment` at the contract level (binds the proofs to the *currently active* committee, not just to "some" committee). |
| **CC-4** | `blockSeqNo` is committed by Circuit 1A/1B (PI[2]) but not by Circuit 2; relayer-side discipline + the partner's `bridge-test-data-gen` enforce coherent values at proof-generation time. |
| **CC-5** | Strict monotonicity at the contract: `blockSeqNo > storedLastSeenBlockSeqNo`. Prevents replay across blocks. |
| **CC-6** | Chain continuity at the contract: `prevMaxLevelLayerHash == storedPrevMaxLevelLayerHash`. Prevents fork injection between consecutive blocks. |
| **CC-7** | No silent garbage: `layerHashes[i] == 0` for `i ≥ numLayers`. Prevents an attacker from sneaking values into unused slots that would survive future calls. |

### 6.5.1 How to verify

CC-1 / CC-2 / CC-4 are **circuit-level** invariants and have no Solidity-side tests; they are
enforced inside the partner's circuits and are exercised by the happy-path bound-proof tests
(`testHappyPathPrimary`, `testHappyPathFallback`). A bound-proof generator that produces
inconsistent `blockId` between Circuit 1A and Circuit 2 would cause `testHappyPathPrimary` to
fail — that's the regression check.

CC-3 / CC-5 / CC-6 / CC-7 are **bridge-level** invariants enforced before either gnark call.
Their negative tests (one per invariant) live in `AckiNackiBridgeVerifyBlockTest` and
`AckiNackiBridgeRelayerLoopTest`:

```bash
forge test --match-test "testRevertOnBkSetCommitmentMismatch|testRevertOnPrevAnchorMismatch|testRevertOnLayerHashTailNonZero|test_relayerLoop_replaySameSeqNo_reverts|test_relayerLoop_lowerSeqNo_reverts|test_relayerLoop_anchorMismatch_reverts" -vv
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
| **ZK-1** | Each gnark Groth16 verifier (`Groth16Verifier`, `PrimaryGroth16VerifierGenerated`, `FallbackGroth16VerifierGenerated`, `LayerHashesGroth16VerifierGenerated`) is auto-generated and not subsequently edited. |
| **ZK-2** | The adapter contracts (`Groth16DepositVerifier`, `PrimaryVerifier`, `FallbackVerifier`, `LayerHashesMovementVerifier`) wrap the `verifyProof` call in `try/catch` so reverts are normalised to `false`. |
| **ZK-3** | The adapter never builds a public-input vector larger than the circuit allows; layout matches the gnark VK. (Deposit: 7 inputs; Circuit 1A/1B: 4 inputs; Circuit 2: 14 inputs.) |
| **ZK-4** | Adapter constructors reject `address(0)` for the underlying Groth16 verifier. |
| **ZK-5** | Each adapter rejects proofs of the wrong byte length (256 B for the v2 attestation/layer-hashes path; 288 B for the deposit path's 256+32 promise commit). |

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
| **AC-3** | `AckiNackiBridge.verifyBlock` is permissionless and `nonReentrant` (no modifier other than the proof checks). |
| **AC-4** | `transferOwnership`, `setYieldRecipient`, `setLiquidReserveBps`, `setAaveEnabled`, `harvestYield`, `emergencyWithdrawAll` are `onlyOwner`. |
| **AC-5** | The verifier slots (`primaryVerifier`, `fallbackVerifier`, `layerHashesVerifier`, `verifier`, `blockHeaderOracle`, `aavePool`, `wethGateway`, `aWETH`) are `immutable` — there is no setter that can replace them post-deployment. |
| **AC-6** | All external interactions follow CEI: state mutation precedes external call. |

### 9.2 How to verify

```bash
grep -n "modifier\|onlyOwner\|nonReentrant\|immutable" contracts/ethereum/src/AckiNackiBridge.sol
```

Cross-reference each external function with the matrix above.

Tests:

```bash
forge test --match-test "OnlyOwner|NotOwner|nonReentrant|Reentrancy|Unauthorized" -vv
```

CEI by inspection: in `AckiNackiBridge.withdraw`, the line `processedDeposits[depositId] = true;` precedes both `_pullFromAave` and `recipient.transfer`. In `AckiNackiBridge.verifyBlock`, the two gnark verifier calls are `view`-style externals returning bool, and storage writes follow both successful verifications (effects come after both interactions return).

---

## 10. Acki Nacki Side Fork Resistance

### 10.1 What must hold

| Property | Statement |
|---|---|
| **FORK-1** | A proof must be signed by a BLS aggregate of the **current** BK set committee (committed via `storedBkSetCommitment`). |
| **FORK-2** | The aggregate must reach the BFT threshold (`3·signers ≥ 2·n` for primary) — this is enforced inside the Halo2 circuit. |
| **FORK-3** | Each layer-hash update is anchored to the previous top-level hash — an attacker cannot inject a discontinuous state. |
| **FORK-4** | The BK set commitment is `uint256` Poseidon hash of the sorted (signer_index, x-coordinate) tuples — collisions require breaking Poseidon. |

### 10.2 Why this is enough

If an attacker forks Acki Nacki and tries to push a fake block update to Ethereum:

1. They need **two** Groth16 proofs (Circuit 1A *or* 1B + Circuit 2). The attestation circuit requires ≥2/3 BLS signatures (Primary) or >1/2 (Fallback) from BKs whose Poseidon commitment matches `storedBkSetCommitment`.
2. If they control fewer than 1/3 of the BK set, they can't reach the Primary threshold; if they control fewer than 1/2, they can't reach the Fallback threshold either.
3. If they control more than 2/3, they're effectively the legitimate majority — that is a consensus failure, not a bridge bug.
4. Even a successful pair of proofs must satisfy the chain anchor (`prevMaxLevelLayerHash == storedPrevMaxLevelLayerHash`) AND strict monotonicity (`blockSeqNo > storedLastSeenBlockSeqNo`). So the only "fork" they could inject is one that *continues* from current state with strictly increasing seqno — i.e., adversarial blocks they finalised themselves with majority control.
5. They cannot mix-and-match proofs from different blocks: `blockId` is bound across both circuits via the 8-leaf envelope hash tree (offset 48), and the bridge passes a single `blockId` argument into both verifier calls (CC-1).

### 10.3 Where to inspect

- Threshold logic: `gosh-bls-verification` crate (audited; see `docs/layer_hashes_circuit_audit.md`).
- BK set commitment derivation: same crate, sorts by signer index then commits with Poseidon T=3 RATE=2.
- Chain anchor + monotonicity: `AckiNackiBridge.verifyBlock` (the lines that revert with `PrevAnchorMismatch` / `BlockSeqNoNotMonotonic`).
- Cross-circuit binding: `docs/four_circuit_architecture.md` §2 (envelope hash tree leaves), §4 (CC-1..CC-7).

---

## 11. Reproducible End-to-End Verification Recipe

Below is a single ordered recipe a reviewer can run start-to-finish.

```bash
# ─── L1 + L2 ────────────────────────────────────────────────────
cd contracts/ethereum

forge build               # must compile with no errors
forge fmt --check         # must be clean
forge test                # 135 tests across 15 suites, all green

# ─── L3 — single-block real-proof bound test ──────────────────
forge test --match-test "testHappyPathPrimary|testHappyPathFallback" -vv
# Drives verifyBlock with a real bound proof set generated by:
#   cargo run -p bridge-prover-orchestrator --bin export-bound-block-proofs --release
# Multi-block real-proof E2E is deferred to Phase 5.3 (relayer + Anvil).

# ─── L0 — sanity grep for invariants ──────────────────────────
grep -n "treasuryBalance\|processedDeposits\|recipient.transfer" src/AckiNackiBridge.sol
grep -n "verifyBlock\|stored\(BkSet\|LastSeen\|NumLayers\|LayerHashes\|PrevMaxLevel\)" \
  src/AckiNackiBridge.sol

# ─── L4 — fork tests (recommended pre-deploy) ─────────────────
forge test --fork-url $ETH_RPC_URL --match-contract AxiomBlockHeaderOracleTest -vvv

# ─── L5 — post-deployment cast checks ─────────────────────────
# Run these after `forge script` deployment (see DeployRealBridge.s.sol).
cast call $BRIDGE  "verifier()(address)"                         # deposit verifier
cast call $BRIDGE  "blockHeaderOracle()(address)"
cast call $BRIDGE  "owner()(address)"
cast call $BRIDGE  "aaveEnabled()(bool)"                         # true if USE_AAVE=true
cast call $BRIDGE  "primaryVerifier()(address)"                  # AN→ETH adapters
cast call $BRIDGE  "fallbackVerifier()(address)"
cast call $BRIDGE  "layerHashesVerifier()(address)"
cast call $BRIDGE  "storedBkSetCommitment()(uint256)"
cast call $BRIDGE  "storedLastSeenBlockSeqNo()(uint64)"
cast call $BRIDGE  "storedNumLayers()(uint8)"
cast call $BRIDGE  "storedPrevMaxLevelLayerHash()(uint256)"
cast call $BRIDGE  "MAX_LAYER_HASHES()(uint256)"                 # 10
cast call $ORACLE  "axiomV2Core()(address)"

# ─── L6 — monitoring queries (run continuously) ───────────────
# 1. Solvency (AAVE-aware)
echo "treasury  = $(cast call $BRIDGE 'treasuryBalance()(uint256)')"
echo "balance   = $(cast balance $BRIDGE)"
echo "aWETHbal  = $(cast call $aWETH 'balanceOf(address)(uint256)' $BRIDGE)"
# Invariant: balance + aWETHbal >= treasury

# 2. AN→ETH chain integrity (poll every block)
cast call $BRIDGE "storedPrevMaxLevelLayerHash()(uint256)"
cast call $BRIDGE "getStoredLayerHashes()(uint256[10])"
# Invariant: prevMaxLevelLayerHash in each new BlockVerified event must equal
# the stored value at the time of the prior call. Strict monotonicity on
# storedLastSeenBlockSeqNo is the second invariant — alert on any drop.

# 3. No genesis drift
cast call $BRIDGE "storedBkSetCommitment()(uint256)"
# Must equal the deployment-time genesisBkSetCommitment until Phase 1.C
# (Circuit 3) ships — there is no rotation path in v2 today.
```

---

## 12. What This Does *Not* Prove

To be explicit about the trust boundary:

| Outside this guide | Where it lives |
|---|---|
| Soundness of the Halo2 circuits (1A, 1B, 2, and 3 once it lands) | `docs/layer_hashes_circuit_audit.md`, partner audits, `docs/four_circuit_architecture.md` §3 |
| BLS12-381 G2 subgroup gap (audit FORK-2 / BLS-1) | open audit finding; carries over to v2's Circuit 1A/1B (same `gosh-bls-verification` chip); will be addressed in next circuit revision |
| Trusted setup of the KZG SRS for Halo2 | community-generated `kzg_bn254_19.srs` shared across all four circuits; verify checksum on download |
| Trusted setup of the gnark Groth16 wrappers (one per circuit) | wrapping circuits live under `crates/bridge-prover-orchestrator/gnark-wrappers/{circuit-1a,circuit-1b,circuit-2}/`; deposit-side wrapper retains its v1 status (stub `Define`, see `integration_plan.md` §6.5) |
| Acki Nacki BFT economic security | not a bridge concern; the bridge inherits whatever finality AN provides via the Primary ≥ 2/3 / Fallback > 1/2 thresholds |
| Off-chain relayer liveness / censorship | a malicious relayer can stall but cannot forge state; multiple competing relayers are sufficient |
| Phase 1.C BK-set rotation circuit + on-chain wiring | not yet shipped; until then `storedBkSetCommitment` is effectively immutable post-deployment |

When any of these change, this doc must be revisited and the affected sections updated.

---

## 13. Cross-References

- `docs/four_circuit_architecture.md` — canonical v2 architectural overview (envelope hash tree, public-input layouts, CC-#).
- `docs/an_partner_integration_plan.md` — live phase-by-phase roadmap (Phase 4.1/4.2/5.1 done; 5.2/5.3/1.C pending).
- `docs/integration_analysis.md` — architecture analysis, contract ↔ circuit mapping (v2-updated §3 and §5).
- `docs/integration_plan.md` — historical milestones M0–M9 (M7–M9 superseded by `an_partner_integration_plan.md`).
- `docs/layer_hashes_circuit_audit.md` — line-by-line legacy circuit audit (BLS-1 / FORK-2 still apply to v2's Circuit 1A/1B).
- `docs/aave_integration.md` — yield bolt-on verification (companion to §4 above).
- `docs/proof_metrics_report.md` — proof sizes, key sizes, gas costs per circuit.
