# Audit Trail — v1 → v2 Bridge Architecture

**Status**: canonical. Tracks what changed in trust model, attack surface, and code surface
between the legacy single-circuit bridge (`LayerHashBridge.sol` + 13-PI `LayerHashesUpdateCircuit`)
and the v2 four-circuit bridge (`AckiNackiBridge.verifyBlock` + Circuits 1A/1B/2 + Phase 1.C
Circuit 3).

**Companion docs**:
- `docs/four_circuit_architecture.md` — architecture detail.
- `docs/bridge_verification.md` — invariant labels (DEP-#, LH-#, BK-#, OR-#, AC-#, FORK-#, CC-#, ZK-#).
- `docs/an_partner_integration_plan.md` — phase-by-phase roadmap.

This doc is **audit-prep**: an external reviewer should be able to read it and know exactly
which security assumptions changed, which carried over, and which are new — without reading
the rest of the doc tree.

---

## 1. Reduced (formerly trusted, now ZK-enforced)

| # | Legacy assumption | v2 enforcement | Where |
|---|---|---|---|
| R-1 | The relayer correctly identifies which AN block a layer-hash proof is for | `block_id` is committed by every circuit (envelope leaf-1, offset 48) and bound by SHA-256 inside the circuit. The bridge passes one `blockId` argument to both verifier calls — a wrong-block proof fails the gnark pairing. | `four_circuit_architecture.md` §2; CC-1 in `bridge_verification.md` §6.5 |
| R-2 | The relayer doesn't replay an old block | `storedLastSeenBlockSeqNo` is strict-monotonic. `verifyBlock` reverts with `BlockSeqNoNotMonotonic(supplied, stored)` on `blockSeqNo <= storedLastSeenBlockSeqNo`. | LH-6 / CC-5 |
| R-3 | The relayer doesn't inject a discontinuous chain | `prevMaxLevelLayerHash` must equal `storedPrevMaxLevelLayerHash`; `PrevAnchorMismatch` reverts otherwise. The new top-of-chain becomes the next call's anchor. | LH-3 / CC-6 |
| R-4 | The relayer doesn't mix Primary and Fallback proofs | `FinalizationType` enum routes to the correct verifier. Signing the wrong target_type fails the BLS check inside Circuit 1A/1B (target_type bytes constrained at offset 116 of `AttestationData`). | LH-1, `four_circuit_architecture.md` §3.1/§3.2 |
| R-5 | The relayer doesn't pad `layerHashes` with garbage in unused slots | `LayerHashTailNonZero(i)` reverts for any `layerHashes[i] != 0` where `i ≥ numLayers`. | LH-5 / CC-7 |
| R-6 | The owner doesn't unilaterally rotate the BK set via the timelocked admin path | The legacy `proposeBkSetCommitment` / `executeBkSetCommitment` / `cancelBkSetCommitment` triple was **deleted in Phase 4.2**. There is no admin path to rotate `storedBkSetCommitment` post-deployment until Phase 1.C ships Circuit 3. | BK-1..BK-5 (Phase 1.C target invariants) in `bridge_verification.md` §6.2 |
| R-7 | The relayer correctly tracks which BK-set committee is active for a given block | `bkSetCommitment` is committed by every circuit (envelope leaf-2, offset 96) and checked against `storedBkSetCommitment` at the bridge level (`BkSetCommitmentMismatch`). Both proofs must commit to the same committee. | LH-2 / CC-2 / CC-3 |
| R-8 | The ETH-side `Groth16DepositVerifier` adapter + the gnark-generated `Groth16Verifier` correctly wrap and verify the deposit-prover's Halo2 SHPLONK proof | **Retired in Phase 4.3 (2026-05-17).** The whole ETH-side adapter chain was removed; the deposit proof is now verified natively on the AN side via `VERHALO2SHPLONK` (K-13). This eliminates two attack surfaces simultaneously: (a) the R15 no-op `Define` stub finding, and (b) any EIP-170-driven simplifications to the wrapper. | Decision Log 2026-05-17 in `an_partner_integration_plan.md` |
| R-9 | The legacy refund-style `withdraw(...)` correctly composes block-hash oracle + ZK proof + treasury bookkeeping | **Retired in Phase 4.3 (2026-05-17).** The `withdraw()` function, `processedDeposits` mapping, `_pullFromAave` shortfall handling for users, and the `IBlockHeaderOracle` dependency for user-facing withdrawals are all gone. Future genuine ETH-side withdrawals will land alongside a burn-proof circuit + state-anchored verification (tracked in Phase 4 open design question). | Phase 4.3 section in §3 below |

**Net reduction**: 9 assumptions removed (7 in Phase 4.2 + 2 in Phase 4.3). Every "the relayer
must do X correctly" / "the ETH-side adapter must be honest" trust becomes either a "the
bridge reverts if X is wrong" enforcement, or simply doesn't exist anymore.

---

## 2. Retained (still trusted in v2)

| # | Assumption | Why we still need it | Open audit findings |
|---|---|---|---|
| K-1 | Halo2 SHPLONK soundness | Standard, well-studied. We don't have an alternative SNARK on the AN side. | None |
| K-2 | Groth16 / BN254 pairing soundness | Standard. Used for AN→ETH wraps only (v2). The v1 deposit-side use was retired in Phase 4.3 (2026-05-17). | None |
| K-3 | KZG trusted setup (`kzg_bn254_19.srs`) | Community-generated; verify checksum on download. Shared across all four AN→ETH circuits. | None |
| K-4 | gnark wrapping circuit honesty (per circuit) | One Groth16 wrapper per AN→ETH circuit under `crates/bridge-prover-orchestrator/gnark-wrappers/{circuit-1a,circuit-1b,circuit-2}/`, auto-generated from the Halo2 verifying key + a small shim. | **R15 (Phase 8 R&D track)** — as of 2026-05-17 each `circuit.go` `Define` is a no-op identity stub, so the on-chain Groth16 verifier does not yet cryptographically constrain the Halo2 SHPLONK proof. Tracked under `an_partner_integration_plan.md` Phase 8; mainnet `v2.0.0` is explicitly gated on closing it. |
| K-5 | `gosh-sha256-chip` correctness | Used by Circuit 1A/1B (envelope SHA-256) and Circuit 2 (layer-hash preimage SHA-256). Identical chip as v1; audit findings unchanged. | None outstanding |
| K-6 | `gosh-bls-verification` correctness, including the BLS12-381 G2 subgroup gap | Used by Circuit 1A (≥ 2/3 threshold) and Circuit 1B (> 1/2 threshold). | **BLS-1 / FORK-2** (medium): `load_private_g2_unchecked` skips on-curve and subgroup checks; calling code adds on-curve but not subgroup. G2 cofactor ≠ 1. **Carries over to v2 unchanged.** Severity: medium. Mitigation in next circuit revision. |
| K-7 | `gosh-dense-balanced-tree` correctness | Used by Circuit 2 for the Poseidon Merkle chain anchor verification. Identical chip as v1. | None outstanding |
| K-8 | `bridge-prover-lib` BK-set fetcher and BOC parser correctness | Used by the relayer to extract committee data and attestations from the AN node. Live-tested by the partner against shellnet. | None |
| K-9 | Acki Nacki BFT economic security | The bridge inherits whatever finality AN provides via the Primary ≥ 2/3 / Fallback > 1/2 thresholds. A 2/3+ Byzantine majority is a consensus failure, not a bridge bug. | Out of scope |
| K-10 | Genesis BK-set commitment honesty | Seeded once at construction via `VerifyBlockConfig.genesisBkSetCommitment`. A wrong genesis means the very first proof can't be made (no live attacker advantage; just a deployment redo). | None |
| K-11 | Bincode layout stability of `AckiNackiBlock` / `AttestationData` / `BlockKeeperSetChangeProofData` | Circuit 2 has hard-coded byte offsets (`BTREE_ENTRY_SIZE = 124`, `TARGET_TYPE_REL_OFFSET = 116`, etc.). Any AN node release that changes the bincode layout breaks proof generation. | **L-1** (legacy): "Bincode layout constants are fragile across AN node releases." Mitigation: CI integration tests against AN node tags (Phase 5.2). |
| K-12 | Block-hash oracle (Axiom V2 Core) — reserved | Unchanged from v1. Not used by either the AN→ETH path or the (currently deferred) burn-proof flow; the legacy refund-style `withdraw()` it serviced was retired in Phase 4.3 (2026-05-17). Preserved in code for the future burn-proof path. | None outstanding |
| K-13 | `ZKHALO2VERIFYWITHVK` TVM opcode soundness | The Phase 4.3 pivot routes the ETH→AN deposit proof through a new TVM opcode that does native Halo2 SHPLONK verification on the AN side. The opcode landed in `tvm-sdk` on 2026-05-22 at dispatch byte `0xC7 0x4A` (see `tvm-sdk` PR #240 + the `Halo2TvmBundle` decoder); CI / test coverage and a partner-side review are gating prerequisites for mainnet. | New in v2 (Phase 4.3, 2026-05-17). Tracked under `an_partner_integration_plan.md` Decision Log 2026-05-17 / 2026-05-22 + Phase 8. |

**Net retention**: 13 assumptions. K-12 retained but currently dormant; K-13 newly introduced by Phase 4.3 (replaces the retired ETH-side Groth16 deposit verifier — a net trust reduction once the opcode lands).

---

## 3. New in v2 (introduced by the four-circuit split)

The four-circuit architecture introduces **no new fundamental trust assumptions**. The
cross-circuit binding via `block_id` and `bk_set_poseidon` is verified inside the circuits
(SHA-256 of envelope leaves) and at the contract level (single argument flowing into both
verifier calls). It does not require any new trusted operator role.

What v2 *does* introduce is a **larger code surface**:

| Surface | Lines / files added | Audit responsibility |
|---|---|---|
| `AckiNackiBridge.verifyBlock` + state machine | ~85 lines + storage | Internal — covered by `AckiNackiBridgeVerifyBlockTest` (17) + `AckiNackiBridgeRelayerLoopTest` (6) |
| `IPrimaryVerifier.sol` + `PrimaryVerifier.sol` + `PrimaryGroth16VerifierGenerated.sol` | 3 files | The first two are internal (try/catch length-check shim); the generated one is gnark output, ZK-1 |
| `IFallbackVerifier.sol` + `FallbackAggregatorVerifier.sol` (+ `verifiers/FallbackAggregatorVerifier.bin`) | adapter + Yul `.bin` | Production 1B path — R15 SHPLONK aggregator (inner `K=21`). The retired `FallbackVerifier.sol` + `FallbackGroth16VerifierGenerated.sol` were deleted 2026-06-22. Aggregator Yul is `snark-verifier-sdk` output (analogous to ZK-1). |
| `ILayerHashesMovementVerifier.sol` + `LayerHashesMovementVerifier.sol` + `LayerHashesGroth16VerifierGenerated.sol` | 3 files | Same |
| `crates/bridge-prover-orchestrator/` | New Rust crate (excluded from main workspace) | Internal — bound test-data generator; per-circuit gnark wrappers under `gnark-wrappers/` |
| `crates/bridge-relayer-daemon/` | New Rust crate (excluded; Phase 5.1 done with mock sources) | Internal — replaces v1's "no relayer" placeholder. A byzantine relayer can stall but cannot forge state (assumption K-9 is unaffected). |

The retired v1 code surface (Phase 4.2 deletion):

| File | LoC retired | Replacement |
|---|---|---|
| `LayerHashBridge.sol` | ~250 | `AckiNackiBridge.verifyBlock` (~85 LoC; integrated into existing contract) |
| `LayerHashVerifier.sol` + `ILayerHashVerifier.sol` + `LayerHashGroth16Verifier.sol` + `LayerHashGroth16VerifierGenerated.sol` | ~32k LoC (mostly the auto-generated verifier) | `LayerHashesMovementVerifier.sol` + `ILayerHashesMovementVerifier.sol` + `LayerHashesGroth16VerifierGenerated.sol` (~33k LoC; same shape but different VK) |
| `BkSetRotationVerifier.sol` + `IBkSetRotationVerifier.sol` + `BkSetRotationGroth16Verifier.sol` | ~28k LoC | Deferred to Phase 1.C (Circuit 3) |
| `LayerHashBridge.t.sol` | 35 tests | Replaced by `AckiNackiBridgeVerifyBlockTest` (17) + `AckiNackiBridgeRelayerLoopTest` (6) |
| `LayerHashE2E.t.sol` | 14 tests with real proofs from 4 fixtures | Single-block bound real-proof test (`testHappyPathPrimary`) at HEAD; multi-block real-proof coverage deferred to Phase 5.3 |
| `layer-hashes-prover/` Rust crate | Halo2 → JSON exporter + gnark wrapper | `crates/bridge-prover-orchestrator/` + `gnark-wrappers/` |
| `bk-set-rotation-prover/` Rust crate | spec + Go gnark wrapper for 2-PI rotation | Deferred to Phase 1.C |

**Net code-surface impact (Phase 4.2)**: -49 Foundry tests (`LayerHashBridge.t.sol` + `LayerHashE2E.t.sol`), +23 Foundry tests (verify-block + relayer-loop + per-adapter sanity). 135 tests total after Phase 4.2.

**Phase 4.3 demolition (2026-05-17)** — legacy ETH-side deposit-verifier chain retired:

| Surface retired | LoC retired | Replacement |
|---|---|---|
| `AckiNackiBridge.withdraw(...)` + `processedDeposits` mapping + `isDepositProcessed(...)` view + `Withdrawal` event + 4 errors + `_verifier` ctor arg + `verifier` storage slot | ~60 lines inside `AckiNackiBridge.sol` | None on the ETH side. The ETH→AN deposit proof is now consumed on the AN side via `VERHALO2SHPLONK` (K-13). |
| `IAckiNackiVerifier.sol` + `Groth16DepositVerifier.sol` + `Groth16Verifier.sol` (gnark-generated for deposit-prover) + `DummyVerifier.sol` | ~5k LoC (mostly the gnark-generated verifier) | Same as above. |
| `deposit-prover/gnark-wrapper/` (Go module + R1CS + keys + generated `Groth16Verifier.sol`) + `deposit-prover/src/groth16_wrapper/` (Rust JSON adapter) | Go + Rust adapter trees | Same as above. |
| `crates/eth-frontend/src/withdrawal.rs` stub + `withdraw`/`processedDeposits`/`Withdrawal` ABI rows in `contract.rs` | ~30 lines | None — the eth-frontend ABI now covers `deposit()` + read-only views only. |
| 11 top-level + deposit-prover shell scripts (`test_e2e*.sh`, `check_e2e_prerequisites.sh`, `test_fuzz_e2e.sh`, `setup_gnark.sh`, `test_groth16_wrapper.sh`, `test_e2e_onchain.sh`, `test_e2e_sepolia.sh`, `test_multiple_proofs.sh`, `generate_and_deploy.sh`, plus 2 example binaries) | Several hundred lines of shell + Rust glue | Future relayer-style end-to-end runner (Phase 5.3, AN→ETH side) + AN-side native-verifier round-trip kit (planned with K-13). |
| `AckiNackiBridgeV2.t.sol` (14 tests) + `FuzzGroth16VerifierTest` (4) + `FuzzGroth16DepositVerifierTest` (3) + AAVE/verifyBlock/relayer tests' `PermissiveVerifier` rewrites + AAVE `withdraw`-flow tests | -26 Foundry tests across 3 retired test contracts and 3 streamlined ones | `FuzzAckiNackiBridgeDepositTest` (3 deposit-only fuzz tests). |

**Net code-surface impact after Phase 4.3**: 109 Foundry tests total across 12 suites (was 135 across 15 after Phase 4.2). `forge build`, `forge test`, and `cargo check --workspace --all-targets` all clean.

---

## 4. Cross-Circuit Soundness Argument

The headline v2 claim is:

> If the bridge accepts `verifyBlock(...)` with `(attestationProof, layerHashesProof, blockId, bkSetCommitment, blockSeqNo, numLayers, layerHashes, prevMaxLevelLayerHash)`, then there exists a real AN block such that:
> - its envelope's `block_id` (offset 48) equals `blockId`,
> - its active BK set Poseidon commitment equals `bkSetCommitment` AND that commitment equals the bridge's stored value,
> - its sequence number equals `blockSeqNo` AND is strictly greater than the bridge's previously seen seqno,
> - its layer-hash preimage hashes (SHA-256) to envelope leaf-0 and produces exactly `layerHashes[0..numLayers]` Poseidon roots,
> - its top-of-chain Poseidon root extends `prevMaxLevelLayerHash` (which equals the bridge's stored anchor) by a valid Merkle chain,
> - it has been BLS-attested by ≥ 2/3 of the BK set if `finType = Primary`, or > 1/2 if `finType = Fallback`.

The argument proceeds:

1. By K-2 (Groth16 soundness) and K-3 (KZG trusted setup), if `primaryVerifier.verifyPrimaryAttestation(attestationProof, blockId, bkSetCommitment, blockSeqNo, …)` returns `true`, then there exists a Halo2 transcript whose public inputs are exactly those values, and the Halo2 prover proved Circuit 1A's constraints over that transcript.
2. By K-1 (Halo2 SHPLONK soundness), the existence of such a transcript implies the existence of a witness satisfying Circuit 1A's constraints.
3. By construction of Circuit 1A (see `four_circuit_architecture.md` §3.6), that witness includes a real AN block envelope whose offset-48 SHA-256 leaf is `blockId`, whose offset-96 leaf is `bkSetCommitment`, whose offset-116 4-byte target_type is `Primary`, and whose attestation has been BLS-aggregated over signers committed by `bkSetCommitment` to ≥ 2/3 threshold.
4. The same argument applied to `layerHashesVerifier.verifyLayerHashesMovement(layerHashesProof, blockId, bkSetCommitment, numLayers, layerHashes, prevMaxLevelLayerHash)` and Circuit 2 yields: there exists a real AN block envelope whose offset-48 leaf is the same `blockId`, whose offset-0 SHA-256 leaf is the layer-hash preimage producing `layerHashes`, and whose Poseidon Merkle chain extends `prevMaxLevelLayerHash`.
5. Step 3 and step 4 commit to the **same** `blockId` and the **same** `bkSetCommitment` — because the bridge passes a single argument into both calls. By K-1+K-2 again, no two distinct AN blocks can produce a Halo2 proof whose `blockId` PI equals the `blockId` argument unless they share the same envelope (K-5 SHA-256 collision resistance for the 8-leaf tree, K-6 for the BK-set commitment).
6. The bridge separately enforces `bkSetCommitment == storedBkSetCommitment` (CC-3), `blockSeqNo > storedLastSeenBlockSeqNo` (CC-5), and `prevMaxLevelLayerHash == storedPrevMaxLevelLayerHash` (CC-6) — so the witness AN block is consistent with the bridge's record of the chain's history.

The argument requires **all of** K-1..K-7 to hold. The medium-severity K-6 finding (BLS-1 /
FORK-2: G2 subgroup gap) does NOT directly break this argument, but it weakens the BK
threshold guarantee in Circuit 1A/1B: an attacker who can find a BLS public key with a
non-trivial G2 subgroup contribution might be able to forge a valid-looking aggregate.
**Mitigation**: ship the next `gosh-bls-verification` revision that adds the G2 subgroup check
before mainnet launch. Tracked as a launch blocker in `docs/an_partner_integration_plan.md`
risk register R12.

---

## 5. What This Audit Trail Does NOT Cover

| Out of scope | Where it's covered |
|---|---|
| Detailed Halo2 chip-level audit | `docs/layer_hashes_circuit_audit.md` (legacy, but the chips are unchanged in v2) |
| Manual hands-on verification protocol | `docs/manual_verification_runbook.md` |
| Property-driven invariant reference | `docs/bridge_verification.md` |
| Per-circuit native-Halo2 proof verification flow | `docs/verifying_an_proof.md` |
| AAVE V3 yield bolt-on | `docs/aave_integration.md` |
| Phase-by-phase implementation plan | `docs/an_partner_integration_plan.md` |

---

## 6. Sign-off Checklist (pre-tag `v2.0.0-rc1`)

- [x] Phase 4.1 complete — `verifyBlock` additive, all tests green.
- [x] Phase 4.2 complete — legacy `LayerHashBridge.sol` and 49 tests demolished; net 135 tests at HEAD.
- [x] Phase 5.1 complete — relayer skeleton crate + Foundry loop test + Rust unit tests.
- [x] All v2 docs in sync: `four_circuit_architecture.md` (new), `integration_analysis.md` (§3 + §5 rewritten), `bridge_verification.md` (§5 + §6 rewritten, §6.5 added, §10 + §11 + §13 updated), `audit_trail_v2.md` (this doc).
- [ ] Phase 5.2 — live `BlockSource` against AN testnet (blocked on Q1 + Q2).
- [ ] Phase 5.3 — 10-block end-to-end against Anvil with real proofs (blocked on Phase 5.2).
- [ ] Phase 1.C — Circuit 3 (BK-set update) wiring + Solidity verifier + `bkSetUpdateProof` argument (blocked on partner signal).
- [ ] BLS-1 / FORK-2 medium audit finding addressed in next `gosh-bls-verification` revision (launch blocker for mainnet, optional for testnet).
- [ ] CI green on `v2.0.0-rc1`.

The `v2.0.0-rc1` tag itself is created by the human release manager once the remaining boxes
are checked.
