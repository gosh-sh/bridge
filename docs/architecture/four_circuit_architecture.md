# Four-Circuit Architecture — `AckiNackiBridge` v2

**Status**: canonical, in force from `v2.0.0-rc1` (Phase 4.2 complete, Phase 5.1 complete).
**Supersedes**: the legacy single-circuit narrative in `docs/architecture/integration_analysis.md` §3 and the layer-hash sections of `_archive/docs/legacy/integration_plan.md` (M7–M9). Those docs are now historical record only.
**Read first**: [docs/README.md](../README.md) for the documentation index; this doc for architectural details; [an_partner_integration_plan.md](../integration/an_partner_integration_plan.md) for the phase roadmap; [bridge_verification.md](../operations/bridge_verification.md) for invariant labels.

---

## 1. Why Four Circuits

The legacy bridge had a single Halo2 circuit (`layer-hashes-update-halo2-circuit`) that proved everything in one shot: BLS attestation, layer-hash extraction, BK-set commitment, Merkle-chain anchoring. That worked for an MVP but it had three structural problems:

1. **One huge circuit ⇒ one huge proving key.** K=19, 80 advice columns, ~5 GB PK, ~26 min real-prover wall-clock (or 22 min for four fixtures with shared keys). Hard to iterate, hard to recover from any change to bincode layout.
2. **Two finalization paths conflated.** Acki Nacki has two attestation rounds — Primary (≥2/3 quorum) and Fallback (>1/2). The legacy circuit only handled Primary; Fallback blocks were silently skipped by the prover daemon (`stats.skipped_blocks`).
3. **No path for BK-set rotation.** A new committee could only be installed via the owner-only timelocked admin route — there was no ZK-proven hand-off proving "the previous committee attests the next committee".

The v2 architecture splits the responsibility across four independent circuits, **bound at the public-input level** so the bridge can verify them as a tuple:

| Circuit | Repo | What it proves | Public inputs | Wired? |
|---|---|---|---|---|
| **1A** Primary attestation | `acki-nacki-to-eth-bridge-halo2-circuits/attestation-bls-checker-circuit` (`primary_circuit.rs`) | BLS aggregate ≥ 2/3 of BK set signed an envelope whose `block_id` is committed | 4 | ✅ |
| **1B** Fallback attestation | same repo, `fallback_circuit.rs` | BLS aggregate > 1/2 of BK set signed a fallback envelope (same `block_id`) | 4 | ✅ |
| **2** Layer-hashes movement | `…/historical-layer-hashes-movement-checker-circuit` | The new layer-hash roots + chain anchor are consistent with the AN block whose `block_id` matches Circuit 1A/1B | 14 | ✅ |
| **3** BK-set update | `…/bk-set-update-checker-circuit` | The previous committee proved a transition to the next committee | 2 (planned) | ⏸ Phase 1.C |
| **4** Bridge withdrawal | `…/bridge-event-prove-circuit` (`circuit4-single-final-root`) | AN `TokenBridge` emitted `WithdrawalInitiated`; event anchored to a known layer root | 10 | ✅ via `withdrawByProof` |

`verifyBlock` consumes exactly **two** of these per block:

- `Circuit 1A` **or** `Circuit 1B` (depending on finalization type), AND
- `Circuit 2`

Circuit 3 is **optional** and only submitted on epoch boundaries via a future `verifyBkSetUpdate` (Phase 1.C, gated on partner's `bk-set-update-checker-circuit` reaching feature parity with the others).

---

## 2. The 8-Leaf Envelope Hash Tree

The cross-circuit binding lives at the *attested-bytes* level, not at the Solidity ABI level. Without a shared anchor, an attacker could submit a Circuit 1A proof for block X and a Circuit 2 proof for block Y, satisfy the bridge's outer invariants, and corrupt state.

The fix (partner commit `672854b`, 2026-05-08): every AN block envelope is committed by an **8-leaf SHA-256 Merkle tree** rooted at the canonical `block_id`, with the leaves placed at known offsets:

```
                 block_id (root)
                       │
       ┌───────────────┴───────────────┐
   inner-L                           inner-R
   ┌───┴───┐                       ┌───┴───┐
 L0     L1                       L2     L3
                                   ...etc...
```

| Leaf | Offset (bytes) | What it commits |
|---|---|---|
| L0 | `0` | 331-byte layer-hashes preimage (Poseidon-friendly serialization of the per-layer roots, padded) |
| L1 | `48` | **`block_id`** (32 bytes) — used by both Circuit 1A/1B and Circuit 2 |
| L2 | `96` | BK-set Poseidon commitment for the **active** committee |
| L3 | `144` | BK-set Poseidon commitment for the **next** committee (epoch hand-off) |
| L4 | `192` | Block sequence number + finalization metadata |
| L5 | `240` | Reserved for governance / migration data |
| L6 | `288` | `transition_hashes` (Plan B: relayer recomputes locally) |
| L7 | `336` | Reserved (zero-filled in the current shellnet) |

The full spec lives in `acki-nacki-to-eth-bridge-halo2-circuits/circuits/ENVELOPE_HASH_MERKLE_SPEC.md` (partner-authoritative).

The attestation circuits (1A/1B) verify that the BLS-signed message is exactly the 32-byte `block_id` (offset 48 of the envelope), not the wider envelope. The layer-hashes-movement circuit (2) embeds the same 32-byte `block_id` as a public input. Both circuits emit `block_id` in their public instances at index `[0]`. The bridge passes a **single** `blockId` argument to both verifier calls — any divergence between the two Halo2 proofs surfaces as one of the gnark verifications returning `false`.

---

## 3. Public-Input Layouts

All public inputs are BN254 `Fr` field elements. Production on Ethereum uses **R15 SHPLONK aggregator** verifiers (`.bin` bytecode under `contracts/ethereum/verifiers/`). Legacy gnark Groth16 adapters remain for Foundry test coverage only.

### 3.1 Circuit 1A — Primary Attestation (`IPrimaryVerifier`)

| Index | Field | Description |
|---|---|---|
| 0 | `blockId` | 32-byte AN block identifier; SHA-256 leaf-1 of the envelope tree, reduced mod Fr |
| 1 | `bkSetCommitment` | Poseidon commitment to the active BK set (sorted `(signer_index, x_limbs)` tuples, 300-padded) |
| 2 | `blockSeqNo` | AN block sequence number being attested |
| 3 | `lastSeenBlockSeqNo` | Bridge's currently stored seqno (the prover commits to it; the contract checks the equality on submission) |

Note: `lastSeenBlockSeqNo` is a public input, not an "anchor" baked into the VK. The relayer reads `storedLastSeenBlockSeqNo` from the contract before generating each proof, so the same VK serves every block.

### 3.2 Circuit 1B — Fallback Attestation (`IFallbackVerifier`)

Identical layout to 1A:

| Index | Field |
|---|---|
| 0 | `blockId` |
| 1 | `bkSetCommitment` |
| 2 | `blockSeqNo` |
| 3 | `lastSeenBlockSeqNo` |

The two circuits share their public-input shape on purpose: the bridge routes `attestationProof` through one verifier or the other based on a `FinalizationType` selector (`Primary` or `Fallback`), but the decoder, the cross-circuit binding, and the state update path are identical.

### 3.3 Circuit 2 — Layer Hashes Movement (`ILayerHashesMovementVerifier`)

| Index | Field | Description |
|---|---|---|
| 0 | `blockId` | Same value committed by Circuit 1A/1B |
| 1 | `bkSetCommitment` | Same value committed by Circuit 1A/1B |
| 2 | `numLayers` | Active layer count, range `[1, 10]` |
| 3..12 | `layerHashes[0..10]` | Per-layer Poseidon Merkle roots; index ≥ `numLayers` = 0 |
| 13 | `prevMaxLevelLayerHash` | Poseidon root anchoring the previous chain (chain-anchor invariant) |

Circuit 2 internally:

- verifies the SHA-256 of the layer-hash preimage matches leaf L0;
- runs a Poseidon Merkle chain from `prevMaxLevelLayerHash` up to `layerHashes[numLayers - 1]`;
- packs 32-byte root hashes into Fr via 256-byte windows;
- range-checks `numLayers` ∈ `[1, 10]` via three 4-bit checks.

### 3.4 Circuit 3 — BK-Set Update (Phase 1.C, **NOT YET WIRED**)

Planned layout (2 PI):

| Index | Field |
|---|---|
| 0 | `oldBkSetCommitment` |
| 1 | `newBkSetCommitment` |

When Phase 1.C lands, `verifyBlock` will gain an optional `bkSetUpdateProof` argument that, on a successful pairing, advances `storedBkSetCommitment` from old → new in the same transaction as the layer-hash update. Until then, BK-set rotation only happens via the owner-only path planned for Phase 1.C (the legacy `LayerHashBridge` 7-day timelock has been retired in Phase 4.2).

---

## 4. Cross-Circuit Binding — the Cheap Soundness Trick

`verifyBlock` accepts:

```solidity
function verifyBlock(
    FinalizationType finType,
    bytes calldata attestationProof,        // Circuit 1A or 1B
    bytes calldata layerHashesProof,        // Circuit 2
    uint256 blockId,                        // shared
    uint256 bkSetCommitment,                // shared
    uint64  blockSeqNo,                     // attestation only (input [2])
    uint8   numLayers,                      // layer-hashes only (input [2])
    uint256[10] calldata layerHashes,       // layer-hashes only (input [3..12])
    uint256 prevMaxLevelLayerHash           // layer-hashes only (input [13])
) external nonReentrant
```

The `blockId` and `bkSetCommitment` parameters are passed **once** but flow into both verifier calls. Each SHPLONK aggregator verifier has its own VK, so the `blockId` that the bridge supplies must match the one each prover committed.

This is the **minimal-cost cross-circuit consistency check**. It costs zero extra gas at the contract level (no extra equality test on a separate input vector) and zero extra VK complexity at the circuit level (the binding is already the canonical public-input position).

| Cross-circuit invariant (CC) | Where it's enforced |
|---|---|
| **CC-1** Same block | `blockId` argument flows into both verifier calls |
| **CC-2** Same committee | `bkSetCommitment` argument flows into both verifier calls |
| **CC-3** Same epoch on the bridge side | `bkSetCommitment == storedBkSetCommitment` (hard-coded check before crypto) |
| **CC-4** Same sequence number | `blockSeqNo` is a public input of 1A/1B but not Circuit 2; the relayer is responsible for picking matching numbers, and the partner's test-data generator enforces this at proof-generation time |
| **CC-5** Strict monotonicity | `blockSeqNo > storedLastSeenBlockSeqNo` (hard-coded check in the bridge) |
| **CC-6** Chain continuity | `prevMaxLevelLayerHash == storedPrevMaxLevelLayerHash` (hard-coded check in the bridge) |
| **CC-7** No silent garbage | `layerHashes[i] == 0` for `i ≥ numLayers` (hard-coded check in the bridge) |

CC-1, CC-2, CC-4 are *circuit-level* invariants: the partner's test-data generator and live AN node bind these by construction; the bridge gets them for free as long as both proofs verify.

CC-3, CC-5, CC-6, CC-7 are *bridge-level* invariants: the bridge enforces them in `verifyBlock` *before* either gnark call, on the cheap side of the cost curve.

---

## 5. State Machine

### 5.1 Storage

```solidity
uint256  storedBkSetCommitment;        // active committee Poseidon commitment
uint64   storedLastSeenBlockSeqNo;     // monotonic; bumped by every successful verifyBlock
uint8    storedNumLayers;              // 1..=10
uint256[10] storedLayerHashes;         // tail (>= storedNumLayers) is zero
uint256  storedPrevMaxLevelLayerHash;  // chain anchor for the next call
```

The four immutables wired at construction time:

```solidity
IPrimaryVerifier        immutable primaryVerifier;
IFallbackVerifier       immutable fallbackVerifier;
ILayerHashesMovementVerifier immutable layerHashesVerifier;
// (Circuit 3 verifier slot reserved for Phase 1.C)
```

If any of the three verifier addresses is `address(0)` at construction, `verifyBlock` reverts with `VerifyBlockDisabled`. This is the path used by deposit-only deployments (early test deployments, mock harnesses) — see `test/helpers/VerifyBlockConfigLib.sol::disabled()`.

### 5.2 Transition

After all checks pass:

```solidity
storedLastSeenBlockSeqNo    = blockSeqNo;
storedNumLayers             = numLayers;
storedLayerHashes[i]        = layerHashes[i];                  // for all i in 0..MAX_LAYER_HASHES
storedPrevMaxLevelLayerHash = layerHashes[numLayers - 1];      // new top-of-chain
```

Then `BlockVerified(blockId, blockSeqNo, finType, numLayers)` is emitted.

The new `storedPrevMaxLevelLayerHash` becomes the chain anchor for the **next** call. There is no admin path that sets these fields — they only advance through `verifyBlock`.

### 5.3 Genesis

The constructor seeds:

- `storedBkSetCommitment = vb.genesisBkSetCommitment` — the Poseidon commitment to the very first BK set the bridge will accept proofs against. This must match the AN node's BK set at the moment of deployment.
- `storedPrevMaxLevelLayerHash = vb.genesisPrevMaxLevelLayerHash` — the chain anchor expected by the **first** `verifyBlock`. Pass `0` if the very first block carries `prevMaxLevelLayerHash = 0` (i.e. it's an AN-side genesis attestation); otherwise pass the AN node's last "key block" Merkle root.
- `storedLastSeenBlockSeqNo = 0` — implicit; the first call must carry `blockSeqNo > 0`.
- `storedNumLayers = 0` — implicit; cleared on the first successful update.

---

## 6. End-to-End Pipeline (per block)

```
[Acki Nacki node]
   │  GraphQL: blocks, BOC, bkSetUpdates, attestations
   ▼
[crates/an-bridge-prover]               ← live AN→ETH prover (daemons, Circuit 4)
   │  Halo2 SHPLONK prove per circuit
   │  Circuit 4: SHPLONK wrap via bridge-relayer-daemon/aggregator.rs
   ▼
[crates/bridge-relayer-daemon]          ← production relayer
   │  daemon-live / daemon-bridge: verifyBlock + withdrawByProof
   ▼
[Ethereum: AckiNackiBridge]
   │  verifyBlock: Primary/Fallback + LayerHashes SHPLONK aggregators
   │  withdrawByProof: Circuit 4 SHPLONK aggregator
```

Wall-clock budget (Phase 4.1 measurements, real proofs):

| Stage | Cost | Notes |
|---|---|---|
| Halo2 SHPLONK prove (per circuit) | ~3–6 min | K=17–21 depending on circuit |
| R15 SHPLONK aggregator wrap | seconds | export binaries in `bridge-prover-orchestrator` |
| `verifyBlock` on Ethereum | ~700k gas | two SHPLONK verifier calls + storage |

---

## 7. Test Surface

The Foundry suite (~174 tests across 21 suites) covers `verifyBlock`, `withdrawByProof`, AAVE, pause, production SHPLONK paths, and per-adapter sanity. Key suites:

| Suite | What it covers |
|---|---|
| `AckiNackiBridgeVerifyBlockTest` | Real bound 1A+2 proofs + negative cases |
| `AckiNackiBridgeProductionVerifyBlockTest` | Production SHPLONK aggregator calldata |
| `AckiNackiBridgeWithdrawByProofTest` | Circuit 4 payout (27 tests) |
| `AckiNackiBridgeRelayerLoopTest` | Multi-block mock-verifier loop |
| `PrimaryVerifierTest`, `LayerHashesMovementVerifierTest` | Per-adapter sanity |

Rust: `cargo test` in `bridge-relayer-daemon`, `deposit-relayer-daemon`, `an-bridge-prover`.

---

## 8. Trust Assumptions — Reduced and Retained in v2

### 8.1 Reduced (now ZK-enforced)

| Assumption (legacy) | v2 status |
|---|---|
| The relayer correctly identifies which AN block a Circuit 2 proof is for | **Removed**: `block_id` is committed by every circuit and bound by SHA-256 leaf-1 of the envelope tree. A wrong-block proof is rejected by the gnark verifier. |
| The relayer doesn't replay an old block | **Removed**: `storedLastSeenBlockSeqNo` is strict-monotonic; `BlockSeqNoNotMonotonic` reverts on replay. |
| The relayer doesn't inject a discontinuous chain | **Removed**: `prevMaxLevelLayerHash` must equal `storedPrevMaxLevelLayerHash`; `PrevAnchorMismatch` reverts otherwise. |
| The relayer doesn't mix primary/fallback proofs | **Removed**: `FinalizationType` selector routes to the correct verifier; signing the wrong target_type fails the BLS check inside Circuit 1A/1B. |
| The relayer doesn't pad `layerHashes` with garbage | **Removed**: `LayerHashTailNonZero` reverts if any index ≥ `numLayers` is non-zero. |

### 8.2 Retained (still trusted)

| Assumption | Why we still need it |
|---|---|
| Halo2 / SHPLONK / Groth16 / KZG soundness | Standard; well-studied. Trusted setup of `kzg_bn254_19.srs` is community-generated — verify its checksum on download. |
| `gosh-halo2-crypto-lib` correctness | Audited (`docs/audit/layer_hashes_circuit_audit.md`); two open mediums (BLS-1 / FORK-2: G2 subgroup gap) carry over to v2 because Circuit 1A/1B reuse the same BLS gadget. Severity unchanged from the legacy assessment. |
| Acki Nacki BFT economic security | Not a bridge concern; the bridge inherits whatever finality AN provides via the Primary ≥ 2/3 / Fallback > 1/2 thresholds. |
| Relayer liveness | A byzantine relayer **can stall** but **cannot forge state**. Multiple competing relayers are sufficient for liveness. |
| Genesis BK-set commitment | Seeded once at construction. A wrong genesis means the very first proof can't be made (no live attacker advantage; just a deployment redo). |

### 8.3 New trust assumptions introduced by v2

None. The four-circuit split removes assumptions; it doesn't add any. The cross-circuit binding via `block_id` is verified inside the circuits and in the bridge — no new trusted operator role is introduced.

---

## 9. References

- `contracts/ethereum/src/AckiNackiBridge.sol` — the contract.
- `contracts/ethereum/src/{IPrimaryVerifier,IFallbackVerifier,ILayerHashesMovementVerifier}.sol` — verifier interfaces.
- `contracts/ethereum/src/{PrimaryVerifier,FallbackVerifier,LayerHashesMovementVerifier}.sol` — gnark adapters.
- `contracts/ethereum/src/{Primary,Fallback,LayerHashes}Groth16VerifierGenerated.sol` — gnark output (regenerate from `crates/bridge-prover-orchestrator/gnark-wrappers/{circuit-1a,circuit-1b,circuit-2}/main.go`).
- `contracts/ethereum/test/AckiNackiBridgeVerifyBlock.t.sol` — single-block tests (17).
- `contracts/ethereum/test/AckiNackiBridgeRelayerLoop.t.sol` — sequential tests (6).
- `crates/bridge-prover-orchestrator/` — Rust prover orchestrator.
- `crates/bridge-relayer-daemon/` — Rust relayer (Phase 5.1, mock sources only).
- `docs/integration/an_partner_integration_plan.md` — live phase-by-phase roadmap.
- `docs/operations/bridge_verification.md` — invariant labels (DEP-#, LH-#, BK-#, OR-#, AC-#, FORK-#, CC-#, ZK-#).
- `docs/operations/aave/aave_integration.md` — independent yield bolt-on (AAVE V3); orthogonal to the four-circuit verifyBlock surface.

---

## 11. Circuit 4 — Bridge Withdrawal (single-final-root)

> **Status (2026-07):** Landed. Partner branch `circuit4-single-final-root`; wired via `AckiNackiBridge.withdrawByProof()`. Legacy Phase A `verifyEvent` (103 PI) and `_layerWindow[100]` scaffold were retired.

Circuit 4 is **orthogonal to `verifyBlock`**. Where Circuits 1A/1B/2 advance the bridge's *state commitment*, Circuit 4 proves a specific AN withdrawal event and triggers USDC payout on Ethereum.

### 11.1 Public-input layout (10 elements)

| Index | Field | Description |
|---|---|---|
| 0 | `tokenId` | Token identifier (0 = USDC) |
| 1 | `amount` | Withdrawal amount |
| 2 | `recipientHi` | Upper 10 bytes of EVM recipient |
| 3 | `recipientLo` | Lower 10 bytes of EVM recipient |
| 4 | `dstChainId` | Must equal `block.chainid` |
| 5 | `senderAccFr` | AN-side sender account |
| 6 | `dappFr` | AN bridge dApp id (immutable) |
| 7 | `accFr` | AN bridge account id (immutable) |
| 8 | `nullifier` | Replay protection |
| 9 | `finalRoot` | Dense-chain anchor; bridge checks `finalRoot ∈ _knownAnchors` |

The bridge verifies `finalRoot` **off-circuit** against anchors populated by prior `verifyBlock` calls. The circuit proves the event binds to that root.

### 11.2 Bridge-side plumbing

| Surface | Detail |
|---|---|
| `AckiNackiBridge.withdrawByProof(proof, pub)` | Permissionless payout. Verifies Circuit 4 proof, checks anchor, enforces nullifier, transfers USDC. |
| `IBridgeWithdrawalVerifier` / `BridgeWithdrawalAggregatorVerifier` | Production SHPLONK adapter |
| `_consumedNullifiers` | Mapping; replay reverts |

### 11.3 Relayer

`bridge-relayer-daemon` `daemon-bridge` watches withdrawal proof artefacts and submits `withdrawByProof`. Idempotent on already-consumed nullifiers.

Historical Phase A design (103 PI, `verifyEvent`): `_archive/docs/partner-qa/circuit4/`.

---

## 10. Quick Glossary

| Term | Definition |
|---|---|
| **Envelope** | The serialized AN block + metadata that the BK set signs. |
| **`block_id`** | 32-byte SHA-256 hash committed at offset 48 of the envelope; serves as the cross-circuit binding anchor. |
| **`bk_set_poseidon`** | Poseidon T=3 R_F=8 R_P=57 commitment over sorted `(signer_index, x_limbs)` tuples of the active BK set, padded to 300 entries. |
| **Primary attestation** | First-round attestation; ≥2/3 of BK set has signed; common-case finalization. |
| **Fallback attestation** | Second-round attestation; >1/2 of BK set has signed; used when Primary fails to reach quorum. |
| **`prevMaxLevelLayerHash`** | Poseidon root anchoring the previous-block chain — the bridge's "this is where the next proof must extend from" pointer. |
| **`numLayers`** | Active history-proof layer count, 1..=10. Inactive slots are zero-filled. |
| **Bound test data** | A synthetic block scenario where Circuit 1A/1B and Circuit 2 share `block_id` and `bk_set_poseidon` — generated by `crates/bridge-prover-orchestrator/src/bound_test_data.rs` and exported by `bin/export_bound_block_proofs.rs`. |
| **gnark wrapper** | Per-circuit Go module under `crates/bridge-prover-orchestrator/gnark-wrappers/` that wraps a Halo2 SHPLONK proof into a 256-byte Groth16 proof verifiable by an EVM pairing check. |
