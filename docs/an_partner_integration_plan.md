# AN Partner Integration Plan — Four-Circuit Architecture

**Author**: bridge integration  
**Status**: Draft, awaiting partner sign-off  
**Supersedes**: M7–M9 of `docs/integration_plan.md` (relayer + AN-side ZK verifier).  
**Companion docs**: `docs/integration_analysis.md`, `docs/bridge_verification.md`, `docs/manual_verification_runbook.md`, `docs/verifying_an_proof.md`, `docs/verifying_eth_proof_on_an.md`.

---

## 0. Decisions Locked In

| Decision | Choice |
|---|---|
| Migration policy | **Full migration**: deprecate `LayerHashBridge.sol`; new `AckiNackiBridge.sol` contract is the single source of truth |
| On-chain verifier | ~~**Native Halo2 SHPLONK** (Yul, generated via `halo2-verifier-gen`). No gnark step.~~ **Revoked 2026-05-07**. Native Yul for Circuit 1B comes out at **78 564 bytes deployment bytecode** — 3.2× over the EIP-170 24 576-byte runtime limit and 1.6× over the EIP-3860 49 152-byte initcode limit. Mainnet-undeployable as a single contract. We pivot to **gnark Groth16 wrappers per circuit**, reusing the existing `layer-hashes-prover/gnark-wrapper/` and `bk-set-rotation-prover/gnark-wrapper/` infra. See §3 for the new Phase 3 plan and §5 risk register R3/R4/R14 for the analysis. The native-Yul experiment (vendored `snark-verifier{,-sdk}`, `gen-yul-verifier-1b` bin, `Halo2NativeVerifier1B.{sol,bytecode.bin}`) was kept briefly as audit trail then removed (see §7 Decision Log entry for 2026-05-07 cleanup) — git history preserves it for reproducibility. |
| AN node branch | **`latest_an_to_eth_bridge_test`** (⚠ to be confirmed — not visible in current local mirror; only `bridge_halo2_tests` is) |
| Deposit + AAVE integration | **Merged into the new `AckiNackiBridge.sol`**: deposits, withdrawals, AAVE yield, layer-hash state, BK-set state, and 4-circuit verification all on one contract |
| First milestone scope | **Full feature parity**: 1A + 1B + 2 + 3 + relayer + on-chain in one push (split internally into 7 phases below) |

These choices imply substantial refactor of the Solidity side (drop `LayerHashBridge.sol`, fold `AckiNackiBridge.sol` deposit logic into the new contract), the prover-lib (wire 3 more circuits), and the relayer (new service, doesn't exist).

---

## 1. End-State Architecture

```
[ACKI NACKI NODE — latest_an_to_eth_bridge_test]
   │  produces:
   │  • Envelope<AckiNackiBlock> with new 8-leaf SHA-256 Merkle envelope_hash
   │  • L0 (Poseidon of 331-byte layer-hashes preimage)
   │  • L2/L3 (Poseidon of sorted [(signer_index, x_limbs)] BK set, 300-padded)
   │  • BlockKeeperSetChangeProofData.transition_hashes in NEW Poseidon format
   │
   │  GraphQL: blocks, BOC, bkSetUpdates, attestations
   ▼
[RELAYER (NEW: bridge-relayer-daemon)]
   │  per block:
   │  1. Fetch envelope, attestation, BK set, layer-hash chain proofs via GraphQL
   │  2. Detect Primary vs Fallback finalization
   │  3. Compute effective BK set changes (compute_effective_changes)
   │  4. Run prover for circuits 1A or 1B + 2 + (3 if BK changes)
   │  5. Cross-check off-chain that envelope_hash + bk_set_poseidon agree
   │  6. Submit to AckiNackiBridge.verifyBlock(...) on Ethereum
   │  7. On revert: log + retry; on success: track block_seq_no
   │
   │  produces 2-3 SHPLONK proofs/block + public instances
   ▼
[ETHEREUM — AckiNackiBridge.sol]
   │  state: storedBkSetCommitment, storedLastSeenBlockSeqNo,
   │         storedLayerHashes[10], storedNumLayers, storedPrevMaxLevelLayerHash,
   │         processedDeposits, treasuryBalance, AAVE bookkeeping
   │
   │  verifyBlock(finType, proof1, pub1, proof2, pub2, proof3, pub3):
   │    - Halo2NativeVerifier1A.verify(...) or 1B (native SHPLONK Yul)
   │    - Halo2NativeVerifier2.verify(...)
   │    - Halo2NativeVerifier3.verify(...) if BK changes
   │    - Cross-check: pub1[0]==pub2[0]==pub3[0] (envelope_hash)
   │                   pub1[1]==pub2[1]==pub3[1] (bk_set_poseidon)
   │                   pub1[1]==storedBkSetCommitment
   │                   pub1[3]==storedLastSeenBlockSeqNo
   │                   pub2[13]==storedPrevMaxLevelLayerHash (chain anchor)
   │    - Update state, emit BlockVerified
   │
   │  deposit() / withdraw() / AAVE: as today, but now on the same contract
   ▼
[ETHEREUM USERS]
   deposit ETH → minted on AN; burn on AN → withdraw via state-anchored proof
```

The contract holds **two** chain anchors: the `last_seen_block_seqno` (monotonic) AND the `prev_max_level_layer_hash` (Merkle continuity). Both are checked on every block update. This is stricter than today's `LayerHashBridge.sol` and explicit prevention against any AN-side fork that doesn't trace back to the previously committed state.

---

## 2. Component Inventory — What Exists vs. What We Build

### 2.1 Partner-side (read-only for us)

| Component | Location | Status | Notes |
|---|---|---|---|
| Circuit 1A code | `acki-nacki-to-eth-bridge-halo2-circuits/attestation-bls-checker-circuit/src/primary_circuit.rs` | ✅ Complete | MockProver tests pass |
| Circuit 1B code | `…/fallback_circuit.rs` | ✅ Complete | MockProver tests pass |
| Circuit 2 code | `…/historical-layer-hashes-movement-checker-circuit/src/circuit.rs` | ✅ Complete | 749 LoC, MockProver tests pass |
| Circuit 3 code | `…/bk-set-update-checker-circuit/src/circuit.rs` | ✅ Complete | MockProver tests pass |
| Test data generators | `…/test-data-gen/src/{generator,bls,envelope_hash,layer_hashes}.rs` | ✅ | |
| Envelope-hash spec | `…/circuits/ENVELOPE_HASH_MERKLE_SPEC.md` | ✅ | Authoritative |
| BK-set GraphQL fetcher | `acki-nacki-to-eth-bridge-halo2-prover/bridge-prover-lib/src/bk_set_fetcher.rs` | ✅ Live | Tested against shellnet |
| Attestation BOC parser | `…/bridge-prover-lib/src/boc_parser.rs` | ✅ Live | |
| Live BLS test | `…/tests/live_attestation_test.rs` | ✅ Passes against shellnet | |
| Native Halo2 verifier daemon | `bridge-verifier-daemon/` | ✅ For Circuit 1A only | |
| AN node side: 8-leaf envelope_hash construction | acki-nacki @ `latest_an_to_eth_bridge_test` | ⚠ To verify | Branch not in our local mirror |
| AN node side: `transition_hashes` migration | spec says "must be done"; not visible in code | ⚠ To verify | Section 8 of spec |

### 2.2 Partner-side gaps (will block us)

| Gap | Resolution path |
|---|---|
| Only Circuit 1A wired into `bridge-prover-lib` (`keys.rs` has only `primary_*`) | We extend `keys.rs` with `fallback_*`, `layer_hashes_*`, `bk_update_*` — **on our side, not partner's** |
| Daemon skips Fallback blocks (`stats.skipped_blocks`) | We replace daemon with our own relayer that handles Primary AND Fallback |
| No cross-circuit consistency layer | We build it in the relayer + the on-chain contract |
| `compute_effective_changes(old_bk_set, block_keeper_set_changes)` is spec-only | We implement it in the relayer (per spec, this is "the proof generation client's" responsibility) |
| Layer-hashes-preimage builder (331-byte L0) | We implement (consume `bkSetUpdates` + `historyProofs` from GraphQL) |
| **`latest_an_to_eth_bridge_test`** branch existence | Confirm with partner in Phase 0 below |

### 2.3 Ours (this repo)

| Component | Action |
|---|---|
| `contracts/ethereum/src/LayerHashBridge.sol` | **DELETE** in Phase 4 |
| `contracts/ethereum/src/LayerHashGroth16VerifierGenerated.sol` | **DELETE** |
| `contracts/ethereum/src/LayerHashVerifier.sol` | **DELETE** |
| `contracts/ethereum/src/AckiNackiBridge.sol` | **REBUILD** as the new contract (deposits + AAVE + 4-circuit verifyBlock) |
| `contracts/ethereum/src/Halo2Verifier.sol` | Rename → `Halo2NativeVerifier1A.sol`, generate 1B/2/3 versions via `halo2-verifier-gen` |
| `contracts/ethereum/src/Groth16Verifier.sol` (deposit) | **KEEP** (deposit ETH→AN flow unchanged) |
| `layer-hashes-prover/` (our crate) | **DEPRECATE**: kept for reproducibility; new prover lives in partner repo + our orchestrator |
| `crates/eth-frontend` | Extend with multi-proof submission |
| `crates/acki-nacki-interface` | Implement real client backed by the partner's GraphQL + BOC parser |
| `relayer/` | **NEW** crate: bridge-relayer-daemon |

---

## 3. Plan — Seven Phases

Each phase has acceptance criteria, file deliverables, and a stop-light status. Phases run mostly in series; some are parallelisable.

### Phase 0 — Confirm Partner Inputs (Days 0–2)

**Purpose**: pre-flight checks to avoid building on missing foundations.

**Tasks**:
- 0.1 Ask partner: which AN branch implements the new 8-leaf `envelope_hash`? (`latest_an_to_eth_bridge_test` was named in their README, but we can only see `bridge_halo2_tests` from `git branch -r`.)
- 0.2 Confirm: has `BlockKeeperSetChangeProofData.transition_hashes` been migrated to `Poseidon(sorted [(signer_index, pubkey_x_limbs)…])`? (Section 8 of `ENVELOPE_HASH_MERKLE_SPEC.md`.) If not, who owns the migration?
- 0.3 Confirm: are MockProver tests passing for circuits 1B, 2, 3 in their current revision? Get a known-good commit SHA we'll pin.
- 0.4 Confirm: Poseidon parameters (`T=3, RATE=2, R_F=8, R_P=57`) and limb widths (`LIMB_BITS=104, NUM_LIMBS=5`) match between the AN node Poseidon implementation (`tvm_vm::executor::zk_stuff::bn254::poseidon`) and the circuit's `OptimizedPoseidonSpec`. Get a test vector: a known BK set → expected Poseidon output.
- 0.5 Confirm: shellnet is currently producing blocks with the **new** envelope-hash format, OR we must upgrade a local node from `latest_an_to_eth_bridge_test`.
- 0.6 Confirm: who owns the gnark vs. native-Halo2 verifier choice for the partner's reference contract — they wrote a sketch `IHalo2Verifier`, we're committing to native SHPLONK Yul; need agreement that we don't need a partner-provided `Halo2Verifier.sol`.

**Acceptance**:
- ✅ Partner SHA pinned in our `Cargo.toml` `[patch]` sections.
- ✅ Test vector for BK-set Poseidon (1 known BK set → 32-byte hash) reproduced byte-for-byte by both: partner's daemon and a Rust reference run from our side.
- ✅ A live block on the chosen AN target (shellnet or local) decodes successfully through the partner's `boc_parser` + `attestation_fetcher`.

**Output**: short "Phase 0 readiness report" added as `docs/an_integration_phase0.md` with answers to 0.1–0.6, the pinned commit, and the test vector.

---

### Phase 1 — Wire Circuits 1B + 2 + 3 in `bridge-prover-lib` (Week 1)

**Purpose**: end the partner's "only Circuit 1A is glued" gap. We do this **in our repo** as a fork-style overlay so the partner repo stays untouched.

**Tasks**:
- 1.1 Create `crates/bridge-prover-orchestrator/` in our workspace (new crate, depends on the partner crates).
- 1.2 Mirror the structure of `bridge-prover-lib::keys` and add: `KeyManager::ensure_fallback_keys`, `ensure_layer_hashes_keys`, `ensure_bk_update_keys`. Each runs `keygen_vk`/`keygen_pk` against a known reference test-data circuit and caches under a separate prefix.
- 1.3 Implement `prover.rs` analogues: `generate_fallback_proof`, `generate_layer_hashes_proof`, `generate_bk_update_proof`. They mirror `generate_primary_proof` but build the right circuit type from the partner's crates.
- 1.4 Implement `verifier.rs` analogues for native-Halo2 verification of all four circuits.
- 1.5 Add unit tests that round-trip each circuit (prove → verify) using the partner's `test-data-gen`.
- 1.6 Decide K values once with cached keys (per spec: 17–18 / 19 / 16–17 / 16). Document final K in `docs/an_integration_kvalues.md` along with PK/VK sizes.

**Acceptance**:
- ✅ `cargo test -p bridge-prover-orchestrator` produces and verifies one proof per circuit (4 proofs total) using synthetic test data.
- ✅ Each prover function returns a `ProofOutput` analogue with the public instances laid out as per spec.
- ✅ PK/VK sizes documented; total disk ≈ 14–17 GB (4 PKs at K=16-19).

**Risk**: PK for K=20 is ~3.5 GB (Circuit 1A). Per-circuit K may differ; if we use K=20 across the board for a single SRS, total PK disk is ~14 GB. Acceptable on a dev box; for CI we'd cache to S3 or skip PK regeneration.

---

### Phase 2 — Layer-Hash Pipeline (the "shifts" path) (Week 1–2)

**Purpose**: build the L0 preimage and the dense Merkle chain proofs from live AN data, since that's the most fragile piece and the one most coupled to AN node internals.

**Tasks**:
- 2.1 In `crates/bridge-prover-orchestrator/src/layer_hashes_fetcher.rs`, build the 331-byte L0 preimage from AN GraphQL data:
  - Fetch `historyProofs` from the block (or compute them).
  - Determine `num_layers` from the block height + window (small-window=2 vs production=4 — verify which the target chain runs).
  - For each layer 1..=10: pack `(layer_number, root_hash)` as 33 bytes; zero-fill inactive.
- 2.2 Implement `compute_layer_hash_chain_proofs`: given `prev_max_level_layer_hash` (from the previously verified block on Ethereum) and the new block's layer-hash data, produce the `Vec<DenseChainLink>` witness for `verify_chain_of_dense_proofs` (uses `gosh-dense-balanced-tree`).
- 2.3 Implement `compute_envelope_merkle_siblings(leaf_index)` — given a complete envelope, return the 3 sibling 32-byte values for that leaf, computed via SHA-256 (off-circuit per spec §4).
- 2.4 Test on the four existing fixtures (`circuit_test_data_L*_*.json` from `gosh-zk-snark-halo2-utils/keys/`) — confirm the new pipeline produces a **valid Circuit 2 proof** for each, with public instances cross-checked against the old single-circuit reference run.
- 2.5 Test on a live shellnet block (or local node block, depending on Phase 0 outcome) — confirm Circuit 2 proves and verifies natively.

**Acceptance**:
- ✅ All 4 historical fixtures produce a valid Circuit 2 SHPLONK proof against the new architecture.
- ✅ `prev_max_level_layer_hash` chain anchor for fixture pair `(L2_H16_prevH0_S1 → L2_H32_prevH16_S1)` works: applying the second proof when the contract has the first's `layer_hash_frs[num_layers-1]` stored in `storedPrevMaxLevelLayerHash` succeeds; substituting any other anchor reverts.
- ✅ Witness debug dump: for one live block, log `num_layers`, `num_prev_chain_steps`, all 10 `layer_hash_frs`, and the dense chain links as JSON, so the partner can sanity-check our extraction matches their node's view.

**Why this gets its own phase**: layer-hash shift verification is the single mechanism preventing forks from being accepted. If we get the offsets, the chain links, or the Poseidon spec wrong here, the whole bridge degrades to "trust the relayer". This is also where our project's audit findings D-1 and L-1 live.

---

### Phase 3 — On-chain Solidity Verifiers (Week 2) — **REWRITTEN 2026-05-07**

**Background — why the path changed**:
- 2026-05-07: vendored `snark-verifier` + `snark-verifier-sdk` 0.2.3, repointed at the gosh halo2-base 0.4.0 fork. Built clean (no API drift between halo2-base 0.4 and 0.5 for the slice we needed) — R3 disproven.
- Cached Phase 1.A `fallback_vk.bin` fed through `gen_evm_verifier_shplonk` produced **78 564 bytes** of deployment bytecode. With EIP-170 runtime limit 24 576 bytes and EIP-3860 initcode limit 49 152 bytes, this is **non-deployable to Ethereum mainnet as a single contract**. The K=20 + 44-advice-column shape of Circuit 1B (driven by SHA-256 + BLS12-381 chips) makes the Yul ~5× heavier than the deposit prover's K=12 DarkDEX Yul (16 249 bytes, deployable).
- The vendored sources, `gen-yul-verifier-1b` bin, and `Halo2NativeVerifier1B.{sol,bytecode.bin}` artefacts were removed in the post-pivot cleanup (see Decision Log 2026-05-07 cleanup); git history preserves them for anyone who wants to re-run the experiment.

**Purpose (revised)**: produce gnark Groth16 wrappers for each of the four circuits, reusing the proven pipeline already running for the deposit prover (`Halo2Verifier.sol` → `Groth16DepositVerifier.sol`), the layer-hash prover (`layer-hashes-prover/gnark-wrapper/`, 13 public inputs, 4 fixtures green), and the BK-set rotation verifier (`bk-set-rotation-prover/gnark-wrapper/`, 2 public inputs, ready).

**Tasks (revised)**:
- 3.1 **Circuit 1B** (Phase 3 first slice — start here):
  - Add `crates/bridge-prover-orchestrator/src/bin/export_fallback_proof.rs` that produces a `halo2_proof.json` from a cached Phase 1.A proof + instances. Mirror `layer-hashes-prover/src/bin/convert-proof.rs`.
  - Add `crates/bridge-prover-orchestrator/gnark-wrapper-1b/` Go module patterned on `layer-hashes-prover/gnark-wrapper/`, configured for 4 public inputs `[envelope_hash, bk_set_poseidon, block_seq_no, last_seen_block_seqno]`.
  - Run `setup` once to emit `FallbackGroth16VerifierGenerated.sol` (≈ 7 KB, ~250 k gas).
  - Add `contracts/ethereum/src/FallbackGroth16VerifierGenerated.sol` + `FallbackGroth16Verifier.sol` adapter (decodes the 4 public inputs, calls Groth16) + `IFallbackGroth16Verifier.sol`.
  - Foundry tests: positive verify, tampered proof, wrong-instance — same shape as `LayerHashE2ETest`.
- 3.2 **Circuits 1A, 2, 3**: fan out the Phase 1.B / 1.C wiring, then attach a gnark wrapper per circuit. Reuse the orchestrator's `*_proof_export.rs` pattern.
- 3.3 **`AckiNackiBridge.verifyBlock`** consumes 2-3 Groth16 proofs (1A or 1B; 2; 3 if BK changes), each ≈ 250-350 k gas → total ≈ 0.6-1 M gas/block.
- 3.4 Generate **development-grade** per-circuit Groth16 keys via `groth16.Setup(ccs)` (single-party, locally on dev box).
  - ⚠️ **CRITICAL CAVEAT (2026-05-17)**: The wrapper R1CS as written today is a **stub** — `Define()` only adds `PublicInputs[i] == PublicInputs[i]` identity constraints, not real Halo2 SHPLONK verification. See R15 in §5 and Phase 8 below. The artefacts produced here (`*Groth16VerifierGenerated.sol`, `verification.key`, `proving.key`) are byte-shaped placeholders only — they validate a trivial circuit, not Halo2.
  - For dev / Foundry / smoke-test purposes the stub is sufficient (it lets the on-chain ABI and integration glue stabilise). For **any** tag claiming cryptographic verification of Halo2 proofs on-chain, including `v2.0.0-rc1`, **Phase 8 must close first**. **Phase 9 (MPC ceremony)** secures whatever R1CS comes out of Phase 8; running Phase 9 against today's stub R1CS produces a perfectly-secured no-op, which is not useful.
  - Each `Setup` call must be reproducible: pin gnark version + circuit R1CS hash + Phase 1 SRS source in `docs/gnark_ceremony.md`. The VKs emitted here become the artefacts that Phase 9 replaces (after Phase 8 supplies real R1CSes).

**Acceptance**:
- ✅ 4 `*Groth16VerifierGenerated.sol` contracts auto-generated by gnark, each ≤ 10 KB.
- ✅ Round-trip Foundry tests per circuit (positive + tamper + wrong-instance).
- ✅ Phase 1.A's synthetic fallback proof passes through `export_fallback_proof` → gnark wrap → Solidity verify with all 4 public inputs intact.
- ✅ Gas figures recorded in `docs/an_integration_gas_report.md`. Target: < 1 M gas total per `verifyBlock` call (vs. ~30 M+ for native Yul).

**Risk (revised)**:
- **R14 (sharpened 2026-05-17)** — gnark Groth16 trusted setup per circuit. **Critical for mainnet**: single-party `groth16.Setup(ccs)` in all four wrappers is a soundness gap (toxic-waste leak → wrapper forgery → on-chain Halo2 bypass). See §5 R14 and Phase 9 below for the full analysis and the MPC-based mitigation plan that gates `v2.0.0`. **Note**: R14 is downstream of R15 below — even a perfect MPC ceremony only secures the existing R1CS, and the existing R1CS is a stub. R15 must close before R14 becomes meaningful.
- **R15 (NEW 2026-05-17)** — **Critical for *any* tag with a "verifies a Halo2 proof on-chain" claim, including rc1**. All four gnark wrappers (`deposit-prover/gnark-wrapper/circuit.go` and `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-{1a,1b,2}/circuit.go`) have a `Define()` body that adds **zero** real constraints. The whole `Define` is:
  ```go
  for i := 0; i < N_PI; i++ {
      api.AssertIsEqual(circuit.PublicInputs[i], circuit.PublicInputs[i])
  }
  api.AssertIsDifferent(circuit.DomainSize, 0)
  return nil
  ```
  No `sw_bn254.Pairing`, no curve operations, no Fiat-Shamir transcript replay, no commitment opening checks. Witness fields (`WitnessCommitmentsPhase0..3`, `Evaluations[147]`, `W`, `WPrime`, `PreprocessedCommitments`, `G2Generator`, `G2Tau`) are declared and assigned to the witness, but **never read by the constraint system**. `circuit_test.go:183` even acknowledges this directly: `t.Logf("Circuit constraints not satisfied (expected - full verification not implemented): %v", err)`. R1CS sizes confirm: `circuit.r1cs = 961 bytes`, `proving.key = 1 581 bytes`, `verification.key = 556 bytes` — a real Halo2 SHPLONK verifier in-circuit would be in the MB range. Consequence: an attacker with the (single-party) `proving.key` can produce a valid Groth16 proof for **any** 7-tuple (deposit) / 4-tuple (1A/1B) / 14-tuple (Circuit 2) of public inputs, bypassing Halo2 entirely. All gas figures in this plan ("~225 k for 1A/1B verify", "~293 k for Circuit 2", "~700 k for verifyBlock") are measuring the stub. Mitigation = **Phase 8** (new — actually implement the wrapper). This is a strictly novel R&D effort — no off-the-shelf gnark implementation of Halo2 SHPLONK verification exists today.
- **R3 (resolved-other)** — halo2-base drift between gosh fork (0.4.0) and crates.io (0.5.x) is an API non-issue, *but the Yul size is*. Pivot makes R3 non-blocking.
- **R4 (resolved-by-pivot)** — the gas concern is resolved by moving to Groth16 (~1 M gas total vs. >30 M for native Yul); the deployability concern (EIP-170 / EIP-3860) is also resolved.

---

### Phase 4 — Rebuild `AckiNackiBridge.sol` (Week 2–3)

**Purpose**: deliver the unified contract that holds deposits + AAVE + AN→ETH state and verifies blocks via the four Halo2 verifiers.

**Tasks**:
- 4.1 **Delete**: `contracts/ethereum/src/LayerHashBridge.sol`, `LayerHashVerifier.sol`, `LayerHashGroth16Verifier.sol`, `LayerHashGroth16VerifierGenerated.sol`, `ILayerHashVerifier.sol`. Update all tests that reference them.
- 4.2 **Rewrite** `contracts/ethereum/src/AckiNackiBridge.sol` with the merged surface:
  - Storage: deposits/AAVE storage (existing) + `storedBkSetCommitment`, `storedLastSeenBlockSeqNo`, `storedNumLayers`, `storedLayerHashes[10]`, `storedPrevMaxLevelLayerHash`, `processedDeposits` (kept).
  - Constructor: existing (`_verifier`, `_blockHeaderOracle`, `_aavePool`, `_wethGateway`, `_aWETH`) **+** four new immutable `Halo2NativeVerifier{1A,1B,2,3}` addresses, plus genesis `_bkSetCommitment` and `_genesisLayerHashAnchor`.
  - Method: `verifyBlock(FinalizationType, bytes proof1, uint256[] pub1, bytes proof2, uint256[] pub2, bytes proof3, uint256[] pub3)`.
  - Extra cross-checks beyond the partner's sketch:
    - `pub2[13] == storedPrevMaxLevelLayerHash` (our chain anchor — partner's contract sketch missed this).
    - `pub2[1] == bkCommit` (consistency, already in sketch).
    - `pub3[1] == bkCommit` if Circuit 3 supplied (consistency, already in sketch).
    - Update `storedPrevMaxLevelLayerHash = pub2[3 + (pub2[2] - 1)]` (the new top layer's root) AFTER successful verification.
  - Withdrawal flow: keep `withdraw(depositId, recipient, amount, ...)` but anchor proof to `storedLayerHashes` rather than the old single-input verifier (TBD design — see §5 below).
  - Owner powers: same as today (timelock for BK set rotation, AAVE controls). The new `verifyBlock` is **permissionless** — anyone can submit; the contract only accepts valid proofs.
- 4.3 Adapt `crates/eth-frontend` to the new ABI (multi-proof submission).
- 4.4 Migrate / rewrite the existing 31 Foundry tests + 23 AAVE tests to the new shape. Most rewrites are mechanical (constructor argument additions); a few need new assertions for the cross-circuit consistency.

**Acceptance**:
- ✅ `forge build` clean.
- ✅ `forge test` 100% pass — including all 4 historical layer-hash fixtures replayed through the new contract using Phase 2's Circuit 2 proofs (one per fixture, then sequential update with chain anchor enforced).
- ✅ All AAVE tests pass without modification (deposit/withdraw/yield logic untouched).
- ✅ Negative tests: wrong `bkCommit`, wrong `last_seen`, wrong `prev_max_level_layer_hash`, mismatched `envelope_hash` across proofs, corrupted proof bytes, missing Circuit 3 when block claims BK changes — all revert with named errors.

**Open design question (resolve in this phase)**:
What ties an AN-side `Burn` event back to a withdrawal? In today's `LayerHashBridge.sol` it's not modeled. Two options:
1. **State commitment**: the contract trusts that `storedLayerHashes` covers the AN account state, and a separate "withdrawal proof" circuit (TBD) proves a `Burn` is committed at a layer the contract has stored.
2. **Postpone**: M1 only does the *attestation* path (AN block headers verified on Ethereum); the actual withdrawal-claim circuit is M-next.

Recommend (2) for this milestone: get the trust-anchor right first; the burn-proof circuit is a separate engineering task that depends on AN's account-state Merkle layout (not part of this plan).

**Post-2026-05-17 update**: option (2) is now *committed*. Phase 4.3 (Decision Log 2026-05-17) retired the legacy refund-style `withdraw()` along with `IAckiNackiVerifier`, `Groth16{,Deposit}Verifier.sol`, and the entire `deposit-prover/gnark-wrapper/` Halo2→Groth16 pipeline. The deposit-event Halo2 proof produced by `deposit-prover/` is now consumed only by the AN side (via a new `VERHALO2SHPLONK` TVM opcode under development in `tvm-sdk`), not by ETH-side `withdraw`. A genuine cross-chain withdrawal will land with a burn-proof circuit + state-anchored verification in a future milestone (post-Phase 7).

---

### Phase 5 — Relayer (`bridge-relayer-daemon`) (Week 3)

**Purpose**: glue the prover (Phase 1+2) to the contract (Phase 4) into a continuously running service.

**Tasks**:
- 5.1 New crate `relayer/` (workspace member). Depends on `bridge-prover-orchestrator`, `bridge_prover_lib` (partner's), `eth-frontend`, `tokio`, `ethers-rs`.
- 5.2 Main loop:
  ```
  loop {
      target_seqno = contract.storedLastSeenBlockSeqNo() + 1;
      attestation = fetch_attestation_for_block(gql, target_seqno);
      finalization_type = detect_finalization_type(attestation);
      bk_set = fetch_current_bk_set(gql);                 // Phase 1
      layer_data = fetch_layer_hashes(gql, block);        // Phase 2
      effective_changes = compute_effective_changes(...); // spec §4

      proof1 = generate_(primary|fallback)_proof(...);
      proof2 = generate_layer_hashes_proof(...);
      proof3 = if !effective_changes.is_empty() { generate_bk_update_proof(...) } else None;

      // Off-chain pre-flight before paying gas:
      assert!(proof1.envelope_hash == proof2.envelope_hash);
      assert!(proof2.bk_set_poseidon == proof1.bk_set_poseidon);
      if proof3 { assert!(proof3.envelope_hash == proof1.envelope_hash); }

      tx = ackinacki_bridge.verifyBlock(finType, proof1, pub1, proof2, pub2, proof3, pub3);
      wait_for_inclusion(tx);
      log_metrics(target_seqno, gas_used, ...);
  }
  ```
- 5.3 Resilience: retry on RPC failures, reorg-aware (re-fetch envelope on reorg detection), skip-and-alert on persistent failures (e.g., a block that never finalizes Primary AND never produces 2 Fallback attestations).
- 5.4 Observability: structured logs (tracing), Prometheus metrics (`relayer_blocks_processed_total`, `relayer_proof_time_seconds`, `relayer_gas_used`, `relayer_errors_total{type=}`), and a `state.json` file mirroring the on-chain anchor for crash recovery.
- 5.5 Integration test: 10 consecutive blocks from shellnet end-to-end through the relayer to a local Anvil instance. Assert `storedLastSeenBlockSeqNo` advances by exactly 10, all 10 emit `BlockVerified`, none revert.

**Acceptance**:
- ✅ Relayer can run for 1 hour against shellnet + Anvil with zero unhandled errors.
- ✅ All blocks in that hour either verified on-chain or explicitly logged as "skipped: <reason>".
- ✅ Restarting the relayer mid-run resumes from the on-chain-stored `last_seen_block_seqno` without duplicating work.
- ✅ Average per-block gas + proof time logged.

---

### Phase 6 — `acki-nacki-interface` Real Implementation + AAVE/Deposit Side Wired (Week 4)

**Purpose**: replace the mock `AckiNackiClient` with a real implementation backed by partner's `gql_client` + `boc_parser`; ensure deposit + AAVE flows work unchanged on the new contract.

**Tasks**:
- 6.1 In `crates/acki-nacki-interface/src/`, replace `mock.rs` calls in `traits.rs` consumers with `real.rs` backed by:
  - `bridge_prover_lib::gql_client` for queries.
  - `bridge_prover_lib::boc_parser` for envelope/attestation extraction.
  - Our `bridge-prover-orchestrator` for layer-hash data.
- 6.2 Sanity test: deposit ETH on the new `AckiNackiBridge` (mainnet fork or local Anvil), then simulate the AN-side `Burn` (mock for now, since we deferred the burn-proof circuit), then call `withdraw` — confirm AAVE-pull-on-shortfall still works.
- 6.3 Update all CI scripts that referenced `LayerHashBridge` to point to `AckiNackiBridge`.

**Acceptance**:
- ✅ The 23 AAVE tests pass against the new contract.
- ✅ `crates/acki-nacki-interface` compiles with the real (non-mock) implementation as default; mock is feature-gated for unit tests.
- ✅ One end-to-end deposit-and-withdraw cycle on Anvil with AAVE supply enabled.

---

### Phase 7 — Documentation, Audit Prep, Release (Week 4–5)

**Status**: 🟢 7.1–7.8 done (2026-05-10). 7.9 pending.
**`v2.0.0-rc1` (any tag claiming cryptographic on-chain Halo2 verification)** is blocked
by Phase 8 (real Halo2 SHPLONK gnark wrapper — R15 finding 2026-05-17). A
*stub-explicit* tag `v2.0.0-pre-rc1-stub` documenting "wrapper is a no-op stub
for ABI smoke-testing only" can ship now; a cryptographic-claim tag cannot.
**`v2.0.0` (mainnet)** additionally requires Phase 9 (trusted setup ceremony) on
top of Phase 8.

**Tasks**:
- 7.1 ✅ Update `AGENTS.md`: drop `LayerHashBridge.sol` references, add the new contract + verifiers + relayer (closed by Phase 4.2; v2 doc set table added in Phase 7.1).
- 7.2 ✅ `docs/integration_analysis.md`: §3 + §5 rewritten for the four-circuit architecture; §1, §2, §4 retained (still valid for deposit/AN platform).
- 7.3 ✅ `docs/integration_plan.md`: top-of-file banner marks the doc as legacy and M7–M9 as superseded by this plan.
- 7.4 ✅ `docs/manual_verification_runbook.md`: Phase D (single-block bound proof), Phase F (`verifyBlock` walk), Phase G (Phase 1.C placeholder), Phase J Attack Group 4 (8 scenarios incl. CC-1..CC-7, J.17/J.18), Phase J Attack Group 5 (Phase 1.C placeholder), Phase K (sign-off checklist) all updated. Repo layout, A.3 spot-checks, suite list, gas table updated.
- 7.5 ✅ `docs/verifying_an_proof.md` rewritten for the per-circuit (1A/1B/2) gnark-wrapped flow, V1–V5 stages updated. Legacy single-circuit walkthrough preserved at `docs/legacy/verifying_an_proof_v1.md`.
- 7.5b ✅ `docs/verifying_eth_proof_on_an.md`: v2 banner + cross-refs updated; deposit flow itself is unchanged in v2.
- 7.6 ✅ `docs/bridge_verification.md`: §1 / §2 retargeted to `verifyBlock`; §5 (LH-1..LH-9) rewritten for the v2 surface; §6 (BK-1..BK-5) reframed as Phase 1.C target invariants; §6.5 cross-circuit invariants (CC-1..CC-7) **added**; §8 ZK-1..ZK-5 (added ZK-5 length check); §9 access-control matrix updated; §10 fork resistance updated; §11 run recipe rewritten; §13 cross-references updated.
- 7.7 ✅ NEW `docs/four_circuit_architecture.md` — canonical v2 entry point: 8-leaf envelope hash tree, per-circuit public-input layouts (1A/1B/2/3), cross-circuit binding mechanism (CC-#), state machine, end-to-end pipeline, test-surface mapping, trust-assumption delta, glossary.
- 7.8 ✅ NEW `docs/audit_trail_v2.md` — 7 reduced assumptions + 12 retained, code-surface delta, cross-circuit soundness argument, pre-tag sign-off checklist.
- 7.9 🟡 Release tag — release-manager step. Gates listed in `docs/audit_trail_v2.md` §6 (Phase 5.2/5.3/1.C, BLS-1/FORK-2 mitigation, CI green). **Revised 2026-05-17 after R15**:
  - **`v2.0.0-pre-rc1-stub`** (the only tag we can ship today without false cryptographic claims): explicit "wrapper is a stub, on-chain verifier accepts any well-formed Groth16 proof of a trivial R1CS, security comes from off-chain trust in the relayer" disclaimer in release notes. Useful for AN-side wiring smoke tests, contract-ABI freeze, and operational dry-runs.
  - **`v2.0.0-rc1`** (cryptographic-claim tag): requires **Phase 8** done — real Halo2 SHPLONK verification implemented in all four wrappers, all gas/prove-time/PK-size numbers re-measured against the real R1CS, all Foundry tests re-validated to fail on tampered Halo2 proofs (today's tests pass on tampered Halo2 because the wrapper doesn't check it).
  - **`v2.0.0`** (mainnet tag): additionally requires **Phase 9** (trusted setup ceremony) per circuit, with VK rotation in deployed Solidity verifiers.

**Acceptance**:
- ✅ All docs consistent with code (135 Foundry tests; `cargo check` clean).
- ✅ Audit-trail document listing what changed: `docs/audit_trail_v2.md`.
- 🟡 CI pipeline green on `v2.0.0-rc1` — pending tag.

---

### Phase 8 — Real Halo2 SHPLONK Verification in gnark Wrappers (**REQUIRED before any cryptographic-claim tag**)

**Status**: 🔴 Not started. **Blocks `v2.0.0-rc1`** (any tag with the claim "on-chain verifier proves Halo2 deposit/state proofs"). Internal smoke-test tags like `v2.0.0-pre-rc1-stub` are *not* blocked — they're explicitly labelled as relayer-trust integration scaffolding.

**Why this is a separate phase**: see R15 above. All four wrappers under `deposit-prover/gnark-wrapper/` and `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-{1a,1b,2}/` have `Define()` bodies that compile to a ~1 KB R1CS with zero real Halo2 verification constraints. The on-chain verifier therefore checks a trivially-satisfiable Groth16 proof — anyone with the proving key (single-party Setup output) can forge "valid" Groth16 proofs for arbitrary public inputs. This is *not* trusted setup hygiene (that's R14 / Phase 9); this is the **R1CS itself doing nothing useful**.

**Scope of work — this is novel cryptographic implementation R&D**. There is no off-the-shelf gnark implementation of Halo2 SHPLONK verification today. Adjacent open-source efforts give us partial building blocks but none drop in:

| Project | What it gives | Why it doesn't drop in |
|---|---|---|
| Axiom `halo2-solidity-verifier` | Halo2 SHPLONK verifier as Yul | Yul, not gnark. Already tried in v1 plan — busted EIP-170 (R4). |
| `consensys/gnark-plonky2-verifier` | PLONKY2 verifier inside gnark | PLONKY2 ≠ Halo2; FRI ≠ KZG; Goldilocks ≠ BN254 Fr. |
| `EthereumZK/halo2-snark-verifier-in-gnark` (community) | Reference attempt at Halo2 verifier in gnark | Research-grade; not maintained; doesn't track gnark API. |
| gnark `std/recursion/groth16` | In-circuit Groth16 verifier | Wrong proof system underneath. |
| gnark `std/algebra/emulated/sw_bn254` | Pairing + curve gadgets on BN254 | Primitive blocks we'll use — but the verifier algorithm is on us. |

The actual implementation primitives we'd build, broadly:
1. Fiat-Shamir transcript replay inside the circuit (Keccak256 or Poseidon, matching what the Halo2 prover used — currently Blake2b for our Halo2 stack — so we additionally need Blake2b gadgets in gnark, which don't exist either).
2. Witness commitment binding: for each of the N commitments (50 in deposit, similar in others), check that the committed point lies on BN254 G1.
3. SHPLONK opening verification: multi-opening polynomial commitment scheme verification — multiple linear combinations, then a pairing equation.
4. Permutation argument: PLONK-style grand product, evaluated at the challenge.
5. Public input binding: L_0(X) · (a(X) - PI) = 0 evaluated at the challenge.
6. KZG pairing check: `e(opening_proof, [τ]₂ - [z]₂) == e(C - [v]₁, [1]₂)`.

**Tasks**:
- **8.1 R&D + design.** Survey of existing partial implementations. Pin gnark version. Decide on transcript hash (Keccak vs Blake2b — likely have to add Blake2b gadgets ourselves; alternatively switch our Halo2 prover to use Keccak transcript, which the partner's Halo2 fork would also need to switch). Sketch R1CS budget. Estimated R&D output: a 2-3 page design doc `docs/gnark_halo2_wrapper_design.md`.
- **8.2 Prototype on the simplest circuit.** Pick **Circuit 1A or 1B** (only 4 public inputs and BLS-related but otherwise smallest wrapper surface). Implement full SHPLONK verification in `circuit.go`. Measure: R1CS size, prove time, PK/VK on disk, on-chain gas. Acceptance: tampered Halo2 proofs (any single field flipped) cause `groth16.Prove()` to fail (the wrapper's R1CS becomes unsatisfiable).
- **8.3 Fan out to other 3 circuits.** Adapt the template for deposit (7 PIs), Circuit 2 (14 PIs), Circuit 3 (TBD when Phase 1.C lands). Each is mostly mechanical once the SHPLONK gadget is solid, but the prepared K (subgroup size), the number of advice columns, and the transcript-population order differ per circuit.
- **8.4 Re-measure everything.** Replace stale stub-based numbers in this plan + `docs/an_integration_gas_report.md`. Realistic upper bounds: R1CS ~10–50 million constraints per wrapper; PK ~50–200 GB per wrapper; prove time ~minutes–tens-of-minutes per wrapper; on-chain Groth16 verify ~250–300 k gas per wrapper (unchanged — Groth16 verifier cost depends only on `NumPublicInputs`).
- **8.5 Adversarial testing.** Foundry test suite extended with a **forgery-attempt** suite: assert that submitting (a) random 256-byte Groth16 proofs, (b) Groth16 proofs of the *stub* wrapper, (c) Groth16 proofs of the real wrapper but for *different* public inputs — all are rejected by the new on-chain verifier. Today these would all be accepted.
- **8.6 Solidity verifier rotation.** Replace all four `*Groth16VerifierGenerated.sol` with the real-wrapper exports. Update the immutable verifier addresses in `AckiNackiBridge` and re-deploy. Note: this is also a (dev-grade) trusted setup — the proper MPC ceremony for it is **Phase 9**.

**Acceptance**:
- ✅ Each wrapper's `Define()` performs full Halo2 SHPLONK verification in-circuit. Inspectable by reading `circuit.go` — should be hundreds of LoC of real constraint generation, not 4 lines of identity checks.
- ✅ R1CS size for each wrapper ≥ 1 MB on disk (sanity floor for "this is doing something").
- ✅ Forgery-attempt suite: 100% of forged Groth16 proofs rejected.
- ✅ Round-trip on each of the 4 circuits: real Halo2 proof → real gnark wrapper → on-chain Groth16Verifier accepts. Tampered Halo2 → wrapper prove fails.
- ✅ Updated `docs/an_integration_gas_report.md` with real numbers; updated risk register noting R15 is closed.

**Coordination**: this phase is **strictly internal Pruvendo work**, no partner dependency (other than the partner's Halo2 prover producing proofs the wrapper accepts — already the case). Realistically 8–16 weeks (2–4 calendar months) of focused R&D for the four wrappers, with substantial reuse between them once Phase 8.2 lands. **Major schedule disruption** — this had been silently assumed to be "already done" by all the Phase 3.x acceptance criteria.

---

### Phase 9 — Trusted Setup Ceremony (Pre-Mainnet, **REQUIRED**)

**Status**: 🔴 Not started. **Mainnet-blocking** (`v2.0.0`). **Downstream of Phase 8** — running a ceremony against the current stub R1CS produces an MPC-secured no-op verifier, which is not useful. Phase 8 must close first so the R1CSes being secured are actually doing the right thing.

**Why this is a separate phase**: see R14 above. The (real, post-Phase-8) `groth16.Setup(ccs)` calls in all four wrappers (`deposit-prover/gnark-wrapper`, `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-{1a,1b,2}/`, and the future `circuit-3/` once Phase 1.C lands) generate Phase 2 keys with toxic waste held in a single process. Anyone with that waste can forge wrapper proofs and bypass Halo2 verification entirely. This is *not* an academic concern: the wrapper VK is what `*Groth16VerifierGenerated.sol` enshrines on-chain, and the on-chain verifier never sees the Halo2 SHPLONK proof underneath — only the wrapper's Groth16 proof.

**Tasks**:

- **8.1 Phase 1 SRS adoption**. Stop generating Phase 1 inside `groth16.Setup`. Adopt one of the public MPC outputs:
  - **Hermez `pot28_final.ptau`** — 2^28 powers, 174 contributors, BN254. Covers any of our circuits (max K is 20 → 2^20 ≪ 2^28).
  - Or **Perpetual Powers of Tau** (~80 contributors, ongoing).
  - Concretely: in `gnark-wrappers/circuit-{1a,1b,2,3}/main.go`, replace `groth16.Setup(ccs)` with `kzg.NewSRS(...).FromPhase1(file)` + `groth16.NewProvingKey` / `NewVerifyingKey` initialization, then run Phase 2 (next task) on top. gnark provides `backend/groth16/bn254/mpcsetup` exactly for this split.
  - Verify SRS via published contribution hashes; record adopted SRS source + sha256 in `docs/gnark_ceremony.md`.

- **8.2 Phase 2 MPC per circuit** (4 ceremonies — `deposit`, `circuit-1a`, `circuit-1b`, `circuit-2`; +`circuit-3` after Phase 1.C). For each:
  - Compile R1CS once on the coordinator's machine. Record the R1CS hash in `docs/gnark_ceremony.md`.
  - Publish coordinator's binary + R1CS + scripts.
  - Recruit N ≥ 5 independent contributors. Diverse OS/hardware/jurisdiction.
  - Each contributor: (i) downloads prior contribution, (ii) verifies hash chain, (iii) adds entropy via `mpcsetup.Phase2.Contribute`, (iv) publishes new contribution + hash + signed attestation that toxic waste was destroyed.
  - Coordinator finalizes after last contribution: `mpcsetup.Phase2.Finalize` → emits PK + VK.
  - Generate fresh `Groth16VerifierGenerated.sol` from the ceremony VK; **delete the dev-grade verifier**.

- **8.3 On-chain verifier rotation**. Replace 4 verifier contracts (`Groth16Verifier.sol` for deposits + `{Primary,Fallback,LayerHashesMovement}Groth16VerifierGenerated.sol`) with their ceremony-derived versions. Re-deploy `AckiNackiBridge` with new immutable verifier addresses (no in-place upgrade — verifiers are `immutable`). Document the migration in `docs/release_notes_v2.0.0.md`.

- **8.4 Audit trail**. Publish per circuit in `docs/gnark_ceremony.md`:
  - Phase 1 SRS source + sha256.
  - R1CS hash.
  - Per-contribution: contributor name/handle, machine description, timestamp, prior-contribution hash, new-contribution hash, contributor's signed PGP attestation that local toxic waste was destroyed.
  - Final VK hash (must match the value hardcoded in the deployed verifier contract).
  - Reproducibility script: any third party should be able to clone the repo, fetch the contributions, re-run `mpcsetup.Phase2.Verify`, and confirm the final VK matches.

- **8.5 CI guard**. Add a CI check that fails if any `gnark-wrappers/*/main.go` contains `groth16.Setup(ccs)` without a `// +build dev` (or equivalent) build tag. Mainnet builds use `// +build mainnet` and pull keys from a release artefact only.

**Acceptance**:
- ✅ For each of the 4 circuits: N ≥ 5 contributions published with verifiable hash chain and PGP-signed waste-destruction attestations.
- ✅ Final VK hashes match `*Groth16VerifierGenerated.sol` constants byte-for-byte.
- ✅ Any third party can re-run `mpcsetup.Phase2.Verify` from public artefacts and arrive at the same VK.
- ✅ CI guard prevents accidental regression to single-party Setup.
- ✅ `docs/gnark_ceremony.md` complete with all 4 (or 5, with Circuit 3) ceremony records.

**Coordination note**: an N=5 ceremony with contributors in different time zones realistically takes 2–3 weeks once the coordinator is ready (1–2 days per contribution + buffer). Start coordinator-side prep work (8.1, R1CS lock, scripts) at least 4 weeks before intended mainnet date.

---

## 4. Cross-Cutting Concerns

### 4.1 Workspace structure (after the dust settles)

```
acki-nacki-bridge/
├── contracts/ethereum/
│   ├── src/
│   │   ├── AckiNackiBridge.sol            ← REWRITTEN (deposits + AAVE + 4-circuit verify)
│   │   ├── halo2_native/
│   │   │   ├── Halo2NativeVerifier1A.sol  ← NEW (Phase 3)
│   │   │   ├── Halo2NativeVerifier1B.sol  ← NEW
│   │   │   ├── Halo2NativeVerifier2.sol   ← NEW
│   │   │   └── Halo2NativeVerifier3.sol   ← NEW
│   │   ├── Groth16Verifier.sol            ← KEPT (deposit prover, ETH→AN)
│   │   ├── Groth16DepositVerifier.sol     ← KEPT
│   │   ├── IBlockHeaderOracle.sol         ← KEPT
│   │   ├── AxiomBlockHeaderOracle.sol     ← KEPT
│   │   └── (LayerHashBridge.sol etc — DELETED)
│   └── test/                              ← UPDATED
├── crates/
│   ├── acki-nacki-interface/              ← Real impl (Phase 6)
│   ├── eth-frontend/                      ← Multi-proof submission (Phase 4)
│   └── bridge-prover-orchestrator/        ← NEW (Phase 1+2)
├── relayer/                               ← NEW (Phase 5)
├── deposit-prover/                        ← UNCHANGED
├── layer-hashes-prover/                   ← DEPRECATED (kept for reproducibility)
└── docs/                                  ← UPDATED (Phase 7)
```

### 4.2 Cargo workspace `[patch]` updates

We need:
```toml
[patch."https://github.com/gosh-sh/gosh-halo2-crypto-lib"]
gosh-sha256-chip = { path = "../gosh-halo2-crypto-lib/sha256-chip" }
gosh-bls-verification = { path = "../gosh-halo2-crypto-lib/bls-verification" }
gosh-dense-balanced-tree = { path = "../gosh-halo2-crypto-lib/dense-balanced-tree" }

[patch."https://github.com/gosh-sh/halo2-lib-zkevm-sha256-and-bls12-381"]
halo2-base = { path = "../halo2-lib-zkevm-sha256-and-bls12-381/halo2-base" }
halo2-ecc = { path = "../halo2-lib-zkevm-sha256-and-bls12-381/halo2-ecc" }
```

Plus pinning the partner repos (`acki-nacki-to-eth-bridge-halo2-circuits`, `acki-nacki-to-eth-bridge-halo2-prover`) at known commits via `path = "../..."` patches.

### 4.3 Test-data fixtures

The 4 historical fixtures (`L2_H16_prevH0_S1`, `L2_H32_prevH16_S1`, `L5_H12288_prevH1024_S11`, `L6_H45056_prevH0_S11`) must continue to work end-to-end through the new pipeline. They were generated by `circuit-data-exporter` (on `bridge_halo2_tests`) for the **old** envelope format — we may need re-generation against the **new** format. **Action**: in Phase 0, ask the partner whether new-format fixtures are needed or whether the old fixture data can be re-encapsulated into the new envelope format off-band.

### 4.4 SRS sharing

All 4 circuits use BN254. K values 16–19. We can use a single `kzg_bn254_20.srs` (~128 MB) for all four — Halo2 SHPLONK truncates as needed. Keep one SRS in `params/`. PK files differ per circuit.

### 4.5 Operational

- Relayer needs ~16 GB RAM (for Circuit 1A/1B proof generation at K=20).
- Per block: ~25–40s proof time for Primary, ~40–65s for Fallback (per partner's estimates). At 1 block per 1.5s on AN, the relayer **cannot keep up in real time** against a fast chain — so we need either (a) a delay tolerance (the bridge always lags by N blocks), or (b) a parallelised prover farm. **Recommend (a)** for this milestone; (b) is a future optimisation.
- On-chain gas at ~3–6M per block × 1 block / hour (relayer can rate-limit) = ~50M–150M gas/day. At 30 gwei: ~$50–$150/day. Manageable but should be measured in Phase 5.

### 4.6 Compatibility with existing AAVE/deposit users

The contract migration in Phase 4 implies a **new deployment address** for `AckiNackiBridge`. Existing testnet deposits on the legacy contract must be drained or migrated. Mainnet has not been deployed, so this is a non-issue for production users. **Action**: include a "drain old contract" deployment step if anyone has testnet ETH parked there.

---

## 5. Risk Register

| # | Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| R1 | `latest_an_to_eth_bridge_test` branch missing or stale | **Confirmed: missing as of 2026-05-06**, partner is creating it now | High (Phase 2 + 5 blocker) | Resequence: do Phase 1.A (Circuit 1B), Phase 3, Phase 4 in parallel while waiting. Pin partner's commit SHA the moment her branch lands. |
| R2 | Poseidon spec mismatch between partner's daemon and our reference | **Mostly mitigated** — canonical Rust impl is `bridge-prover-lib/src/poseidon.rs`, read & confirmed to match spec §3.1 (`T=3, RATE=2, R_F=8, R_P=57`, `LIMB_BITS=104`, `NUM_LIMBS=5`, `MAX_SIGNERS=300`, `PADDING_SIGNER_INDEX=0xFFFF`) | High (silent verification failures) | Still want one numeric test vector (Q4) for byte-level CI guard. |
| R11 | Discrepancy between `bridge-prover-lib/src/poseidon.rs::compute_bk_set_poseidon` (300-padded) and `test-data-gen/src/envelope_hash.rs::compute_bk_set_poseidon` (no padding) | Medium | Medium (wrong leaf hash → wrong envelope_hash → wrong proof) | Confirmed by reading both files: the test-data-gen helper is for unit tests only and does NOT pad. The prover-lib helper is the canonical one for L2/L3 leaves. Must pad in our Phase 2 layer-hash pipeline. Add a CI test asserting both produce the same Fr **only when** `bk_set.len() == 300`. |
| R12 | Node-team disagreement on AlinaT's `transition_hashes` migration delays Phase 5 | Medium | High | Q2 was answered with "pending node-team agreement". Track this; if it stalls > 1 week, our relayer should compute `transition_hashes` locally from BK-set state instead of relying on node-published values. |
| R13 | No live testnet with the new envelope format | High (after partner branch lands) | High (Phase 2 acceptance, Phase 5 integration) | Either run AlinaT's branch under local Docker, or request that Ekaterina.Pantaz deploys a shared testnet from it. Local-only is workable for Phase 2 + 5 dev; shared testnet is needed for Phase 7 release. |
| R3 | `halo2-verifier-gen` halo2-base drift vs. partner's halo2-axiom | ✅ Disproven 2026-05-07 | n/a | Vendored `snark-verifier` + `snark-verifier-sdk` 0.2.3 build cleanly against the gosh fork's halo2-base 0.4.0 (`vendor/snark-verifier{,-sdk}/`). API surfaces between 0.4 and 0.5 are identical for the Yul codegen slice. R3 is closed. |
| R4 | Native Halo2 verifier gas / size exceeds Ethereum limits | ✅ Materialised 2026-05-07 (worse than expected) | Critical → triggered Phase 3 pivot | Cached Phase 1.A VK fed through `gen_evm_verifier_shplonk` produced 78 564-byte deployment bytecode. EIP-170 limit 24 576 bytes (3.2× over), EIP-3860 limit 49 152 bytes (1.6× over). Deployment is impossible, not just expensive. **Pivot**: switched Phase 3 to gnark Groth16 wrappers (R14). Native-Yul vendor + bin + artefacts removed post-pivot; git history preserves them. |
| R14 | gnark Groth16 trusted setup per circuit (4 ceremonies) — **single-party Setup is a soundness gap for mainnet** | Confirmed open as of 2026-05-17 | **Critical for mainnet, downstream of R15** | All four wrappers currently call `groth16.Setup(ccs)` in-process. The Phase 2 toxic waste (α, β, γ, δ) was held in a single process on a single developer machine; if leaked, the holder can forge wrapper proofs. **NB**: R14 is downstream of R15 — even a perfect MPC ceremony only secures whatever R1CS the circuit compiles to, and **today's R1CS is a no-op stub (R15)**. Running the ceremony now would produce a perfectly-secured stub. Mitigation = **Phase 9** (new), but only after **Phase 8** closes R15: (a) switch to a public Phase 1 SRS (Hermez `pot28_final.ptau`, Perpetual PoT, or Aztec Ignition), (b) run an MPC Phase 2 ceremony per (real) circuit with N ≥ 5 independent contributors, (c) re-deploy the four `*Groth16VerifierGenerated.sol` contracts with ceremony VK hashes, (d) publish per-circuit contribution audit trail. |
| R15 | **gnark wrapper R1CS is a no-op stub** — `Define()` adds zero real constraints, on-chain Groth16 verifier accepts arbitrary public inputs | Confirmed open as of 2026-05-17 (literal `t.Logf("expected - full verification not implemented")` in `circuit_test.go:183`) | **Critical for *any* cryptographic-claim tag, blocks `v2.0.0-rc1`** | An attacker with the (single-party) `proving.key` produces a valid Groth16 proof for *any* tuple of public inputs — no Halo2 prove step required. All on-chain gas figures, all "verifies tampered proofs rejected" tests in Foundry, and all marketing claims of "trustless ZK bridge" are presently empty. Mitigation = **Phase 8** (new): implement Halo2 SHPLONK verification natively inside gnark `circuit.go`. This is novel R&D — no off-the-shelf gnark library for Halo2 SHPLONK verification exists today; closest community efforts (Axiom Yul verifier, gnark-plonky2-verifier, halo2-snark-verifier-in-gnark research code) give partial primitives but no drop-in. Estimated 8–16 calendar weeks for 4 circuits. **This finding inflates the published timeline from 9–10 weeks (ceremony-only mainnet gate) to 17–25 weeks (real wrapper R&D + ceremony).** |
| R5 | `BlockKeeperSetChangeProofData.transition_hashes` not migrated on the AN node | Medium | High | Phase 0.2 confirmation. If not done, write the migration ourselves; impacts Phase 5 (relayer would need to recompute on-the-fly). |
| R6 | New-envelope-format fixture data not available for the 4 historical blocks | Medium | Low (regression-only) | Phase 0 confirmation; if needed, ask partner to re-generate or accept that legacy regression is on the legacy contract only. |
| R7 | Relayer can't keep up with AN block rate | High (real-time) | Low (intentional) | Accept lag; document the SLA; future-work parallelisation. |
| R8 | Burn-proof circuit (for AN→ETH withdrawals) not in scope | Acknowledged | High (UX) | Phase 4 design note: this milestone delivers the *trust anchor* (AN block headers verified). The actual `withdraw(burnProof)` flow is the next milestone. Today's `LayerHashBridge` doesn't fully model this either, so we're not regressing. |
| R9 | Cross-circuit consistency check missed by the contract → mixing-block attack | High (if missed) | Critical | Explicit named errors for each cross-check (envelope_hash, bk_set, last_seen, prev_max_level_layer_hash). Foundry tests for each violation case. Phase 4.4. |
| R10 | Partner's `transition_hashes` migration ships but with incompatible Poseidon encoding ("hash_bytes_flat" vs "hash_bytes_axiom") | Medium | High | Phase 0.4 covers it. Spec §3.1 explicitly warns about this. |

---

## 6. Time Estimate

| Phase | Effort (1 engineer) |
|---|---|
| 0 — Confirmations | 2 days |
| 1 — Wire 1B/2/3 in prover | 4 days |
| 2 — Layer-hash pipeline | 4 days |
| 3 — Solidity verifiers | 5 days |
| 4 — `AckiNackiBridge` rebuild | 5 days |
| 5 — Relayer | 4 days |
| 6 — Real interface + AAVE | 3 days |
| 7 — Docs + release | 3 days |
| 8 — Real Halo2 SHPLONK wrapper R&D + 4 circuits | **8–16 calendar weeks** (R&D, novel) |
| 9 — Trusted setup ceremony (pre-mainnet, calendar-bound) | ~5 engineering days + 2–3 weeks calendar |
| **Total (engineering, Phases 0–7 only)** | **≈ 30 working days = 6 weeks engineer-time** |
| **Total (calendar, rc1-ready w/ cryptographic claim)** | **≈ 14–22 weeks** (Phases 0–7 dominated by Phase 8 R&D) |
| **Total (calendar, mainnet-ready)** | **≈ 17–25 weeks** (Phase 8 then Phase 9 can overlap with audit) |

This assumes Phase 0 returns "all clear" within 2 days. Each "Medium" risk that materialises adds 2–5 days; an R1 or R2 hit would push the timeline closer to 8–9 weeks for Phases 0–7.

**Phase 8** is **the dominant schedule risk** post-R15-finding. The 8–16 week range covers:
- Lower bound: a clean reuse of an existing community Halo2-in-gnark prototype (we know of none today that's drop-in; this is the optimistic scenario assuming one materialises or we partner with a research team).
- Upper bound: full bottom-up implementation by one engineer of a Halo2 SHPLONK verifier in gnark — Fiat-Shamir transcript replay, SHPLONK opening checks, KZG pairing — applied to 4 circuits in series.
- The phase is *effort*-bound, not calendar-bound (unlike Phase 9). Adding engineers in parallel meaningfully compresses it (different circuit slots can be done in parallel once Phase 8.2 lands).

**Phase 9** is **calendar-bound** rather than effort-bound — the engineering work is ~5 days (Phase 1 SRS adoption + Phase 2 MPC scripts + verifier rotation + CI guard + audit-trail doc) but the MPC contribution chain takes 2–3 weeks regardless of how many engineers we throw at it.

---

## 7. Decision Log

- **2026-05-06**: User locked in the choices in §0. This document supersedes M7–M9 of `integration_plan.md`.
- **2026-05-06**: AlinaT (partner) answered Q1, Q2, Q5 of `docs/an_partner_phase0_questions.md`:
  - **Q1**: New 8-leaf envelope hash is **not yet** in any AN-node branch. AlinaT is landing it today in `latest_an_to_eth_bridge_test` together with Circuit 2 live tests.
  - **Q2**: AlinaT owns the `transition_hashes` migration. Reference impl: `bridge-prover-lib/src/poseidon.rs` (canonical). Old Silkov transition hashes are unused → no compatibility path needed. Pending: node-team agreement.
  - **Q5**: Current shellnet runs the old envelope format (Sergey Gorelyshev's `contracts dex halo2` branch). Circuit 2+3 testing on shellnet will not work until AlinaT's branch is deployed. Local node from her branch or a new testnet (Ekaterina.Pantaz to advise) needed.
- **2026-05-06 (later)**: AlinaT answered Q3 (partial) and Q4 (promised):
  - **Q3**: Circuit 1B mock tests passing as of 2026-05-05. Circuit 2 in active cleanup today. Circuit 3 wiring/live-tests deferred ~2 days (priority is 1+2). → Re-sequence Phase 1 into 1.A (Circuit 1B, ready now), 1.B (Circuit 2, after AlinaT's daily checkpoint), 1.C (Circuit 3, deferred ~2 days).
  - **Q4**: Numeric Poseidon test vector promised by EOD 2026-05-06. Algorithm already verified by reading `bridge-prover-lib/src/poseidon.rs`.
- **2026-05-06 (later still)**: Internal AN-team escalation from Ekaterina.Pantaz on the `transition_hashes` migration ownership and local-node feasibility. Surfaces R12 in real time. We don't act directly; we reduce dependency on node-published `transition_hashes` by recomputing them locally in the relayer (Phase 5).
- **2026-05-06 (evening)**: Phase 1.A landed. New crate `crates/bridge-prover-orchestrator/` (excluded from the root workspace because it pulls in halo2-axiom which conflicts with the workspace's existing dep tree; it has its own `Cargo.lock`). `cargo test --test fallback_round_trip` green: synthetic 10-signer fallback envelope → prove → verify → tamper-rejection → wrong-instance-rejection. Keygen 218 s, prove 11 s, verify ~milliseconds. Cached PK is 6.5 GB on disk; `.gitignore` updated to keep it out of the repo. Phase 1.B (Circuit 2) and Phase 3 (`Halo2NativeVerifier1B.sol`) are the next parallel candidates.
- **2026-05-07 (early morning)**: CI fix — workspace `Cargo.lock` was inadvertently `.gitignore`d while CI's `setup:rust` runs `cargo fetch --locked` and `Dockerfile` copies `Cargo.lock`. Removed the blanket rule, replaced with per-subproject ignores for excluded crates. CI green again.
- **2026-05-07 (Phase 3 attempt + pivot)**: Vendored `snark-verifier` + `snark-verifier-sdk` 0.2.3 under `vendor/`, repointed at the gosh halo2-base 0.4.0 fork via `path =`. **R3 disproven**: the 0.4 ↔ 0.5 API surface for Yul codegen is identical; vendor build is clean. Wrote `crates/bridge-prover-orchestrator/src/bin/gen_yul_verifier_1b.rs` and ran it against the cached Phase 1.A VK. Output: **78 564-byte deployment bytecode** — 3.2× over EIP-170 (24 576 B runtime) and 1.6× over EIP-3860 (49 152 B initcode). **R4 materialised, worse than expected**: not just gas-expensive but mainnet-undeployable. **Decision**: revoked the §0 "no gnark step" lock-in; Phase 3 pivots to per-circuit gnark Groth16 wrappers, reusing the proven `layer-hashes-prover/gnark-wrapper/` and `bk-set-rotation-prover/gnark-wrapper/` infra. Risk register updated (R3 closed, R4 closed-by-pivot, R14 new for ceremony).
- **2026-05-07 (post-pivot cleanup)**: Removed the rejected native-Yul experiment from the working tree: `vendor/snark-verifier{,-sdk}/`, the `snark-verifier-sdk` dependency in the orchestrator, the `gen-yul-verifier-1b` bin and `[[bin]]` entry, and `contracts/ethereum/src/halo2_native/`. The numerical findings (78 564 B, 3.2× EIP-170, etc.) are preserved here in the doc; reproducibility is preserved by `git checkout` of any pre-cleanup ref. Production tree now contains only the gnark path.
- **2026-05-10 (Phase 1.B + Phase 3.3 — Circuit 2 (Layer Hashes Movement) end-to-end)**: AlinaT's `0510.prompt` unblocked Phase 1.B with two notes: (1) the partner's attestation circuits switched their primary-input semantic from `envelope_hash` (offset 84..116 in `AttestationData`) to **`block_id`** (offset 48..80) per Andrei Kurochkin's redesign — the bytes interpreted are different, so we re-genned VK/PK/proof for 1A and 1B; (2) Circuit 2 (`historical-layer-hashes-movement-checker-circuit`) is now stable with green unit tests. Work delivered today:
  - **1A/1B refresh**. Pulled partner repos to `acki-nacki-to-eth-bridge-halo2-circuits@ff51a47` + `…halo2-prover@8c10a16` (path-deps; no SHA pin in our `Cargo.toml` since we're on local sibling paths). Renamed `compute_envelope_hash_fr → compute_block_id_fr` in `crates/bridge-prover-orchestrator/src/prover.rs` (offset 84 → 48); updated `FallbackProofOutput::block_id_fr`, the public-instance docs in `keys.rs`/`verifier.rs`, and the field-element re-exports. Adapted to the partner's new tuple-returning `compute_bk_set_poseidon(bk_set) → (Fr, [u8;32])` signature. Wiped + re-genned `params/{primary,fallback}_{vk,pk,config_params}` (each ~6.5 GB PK; identical advice/lookup shape to the previous Phase 1.A keys, so file sizes and circuit `BaseCircuitParams` are unchanged — only the VK/PK bytes differ). New `proof.bin` for both circuits (1A 8 192 B, 1B 14 784 B); ran each through the gnark wrappers to get fresh `Groth16Verifier.sol` for 1A and 1B (Halo2 VK changed → Groth16 setup is fresh too). Refreshed Solidity public-input constants in `test/{Primary,Fallback}Verifier.t.sol` (`BLOCK_ID`, `BK_SET_COMMITMENT`); renamed the parameter `envelopeHash → blockId` across `I{Primary,Fallback}Verifier.sol` + `{Primary,Fallback}Verifier.sol` + the two test files (typed remains `uint256`, ABI shape unchanged). 16/16 Primary+Fallback Foundry tests pass with new fixtures.
  - **Phase 1.B (Circuit 2 wiring)**. New modules in `crates/bridge-prover-orchestrator/src/`: `layer_hashes_keys.rs` (`LayerHashesKeyManager`, separate `kzg_bn254_17.srs` SRS, `K=17 / LOOKUP_BITS=16 / NUM_UNUSABLE_ROWS=109`), `layer_hashes_prover.rs` (`generate_layer_hashes_proof` + `verify_layer_hashes_proof`, instance shape `[block_id, bk_set_poseidon, num_layers, layer_hash[0..10], prev_max_level_layer_hash]` = 14 Fr), `layer_hashes_test_data.rs` (synthetic input builder mirroring partner's `tests/real_prover.rs::build_test_case` byte-for-byte; `TREE_DEPTH=4` for the lightweight `HISTORY_PROOF_WINDOW_SIZE=8` config). New `tests/layer_hashes_round_trip.rs` exercises keygen → prove → verify → tampered-rejection → wrong-instance-rejection. Measured on the dev box: keygen_vk **5.8 s**, keygen_pk **3.8 s** (vs 144 s + 144 s for K=20 1B), PK on disk **339 MB** (vs 6.5 GB for 1B), proof gen **~12 s**, proof size **6 112 bytes** (vs 14 784 for 1B). All five assertions in the round-trip pass.
  - **Phase 3.3 (Circuit 2 gnark wrap + Solidity)**. New `src/bin/export_layer_hashes_proof.rs` writes `proof.bin` + `instances.bin` + `halo2_proof.json` for any `(num_layers, num_chain_steps)` against the cached LH key set. New `gnark-wrappers/circuit-2/` Go module — verbatim mirror of `circuit-1a/` with `NumPublicInputs = 14` and `LayerHashesVerifierCircuit`. `setup` produces `circuit.r1cs` + keys + `Groth16Verifier.sol` (~26 KB source) in <1 s; `prove` produces a 256-byte Groth16 proof in milliseconds (still Halo2-wrapping-only — the gnark layer is an identity stub; the Halo2 SHPLONK proof is what carries cryptographic weight). Three new Solidity files: `ILayerHashesGroth16Verifier.sol` (raw `verifyProof(uint256[8], uint256[14])`), `ILayerHashesMovementVerifier.sol` (bridge-friendly `verifyLayerHashesMovement(proof, blockId, bkSetCommitment, numLayers, layerHashes[10], prevHash)`), `LayerHashesMovementVerifier.sol` (adapter), plus `LayerHashesGroth16VerifierGenerated.sol` from the gnark output (renamed `Verifier`). New `test/LayerHashesMovementVerifier.t.sol` — 10 tests: 1 positive (verifies @ **~292 985 gas**, +70 k vs the 4-input verifiers, as expected), 8 negative (tamper, wrong block_id, wrong BK commitment, wrong num_layers, wrong layer hash, wrong prev_hash, wrong padding slot, bad length), 1 ctor sanity.
  - **Cumulative state**. Foundry: **161/161 tests passing** (151 baseline + 10 new). 3/4 of the `gnark-wrappers/{circuit-1a,circuit-1b,circuit-2,circuit-3}` slots now live; only Circuit 3 (BK-set update) is open and is gated on Alina's Phase 1.C signal. Per-block on-chain verify cost projection (1A or 1B + 2, no 3): ~225 k + ~293 k = **~520 k gas**. Adding 3 (BK-set rotation) brings the worst-case to ~750 k gas, still well under 1 M. R4 fully resolved by gnark (no contract-size issue at 14-input shape — 26 KB Solidity source compiles to ~7 KB runtime).
  - **Side effects worth flagging**. (a) Added `bridge-poseidon` and `historical-layer-hashes-movement-checker-circuit` and `gosh-dense-balanced-tree` and `sha2` as path-deps in `crates/bridge-prover-orchestrator/Cargo.toml`; the partner's `bridge-prover-lib` now transitively pulls `bridge-poseidon` from its crates workspace, so we patched both directly and via `[patch."https://github.com/gosh-sh/acki-nacki-to-eth-bridge-halo2-circuits"]`. (b) Cargo refused to fetch the `git = "URL"` dep when `https://github.com/...` was unauthenticated, even with our path patches; added `git config --global url."git@github.com:".insteadOf "https://github.com/"` rewrite as the user-side fix (one-time per machine) so cargo's initial source-resolution succeeds via SSH. Documented the requirement in setup notes (TBD-add-to-AGENTS.md).
- **2026-05-07 (Phase 3.2 — second slice complete: Circuit 1A)**: Applied the gnark Groth16 wrapper template to Circuit 1A (Primary attestation), reusing the Phase 3.1 pattern verbatim. Circuit 1A's public-instance layout is identical to Circuit 1B's (`[envelope_hash, bk_set_poseidon, block_seq_no, last_seen_block_seqno]` — 4 Fr elements), so the `proof_export.rs` / Go wrapper / Solidity adapter layouts are reused without modification; only the underlying VK and circuit-builder differ. New artefacts:
  - `crates/bridge-prover-orchestrator/src/bin/export_primary_proof.rs` — drives `bridge_prover_lib::generate_primary_proof` against synthetic `generate_test_data_all_sign(10)` and writes `proof.bin` (8 192 B vs Fallback's 14 784 B — Circuit 1A has 24 advice columns vs Fallback's 44, so the SHPLONK proof is leaner) + `instances.bin` (128 B) + `halo2_proof.json` (~70 KB).
  - `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-1a/` Go module — verbatim mirror of `circuit-1b/` with renamed `PrimaryVerifierCircuit`. `setup` compiles a 5-constraint stub and emits `Groth16Verifier.sol` (~26 KB source); `prove` produces a 256 B Groth16 proof in milliseconds.
  - `contracts/ethereum/src/{IPrimaryGroth16Verifier,IPrimaryVerifier,PrimaryVerifier,PrimaryGroth16VerifierGenerated}.sol` — interface, adapter, gnark-generated verifier (renamed `Verifier → PrimaryGroth16VerifierGenerated`).
  - `contracts/ethereum/test/PrimaryVerifier.t.sol` — 8 tests with the same shape as `FallbackVerifier.t.sol`: positive (real proof verifies at **~222 696 gas**, within ±50 of Fallback as expected since the Groth16 verifier shape is identical), 6 negative (tamper, 4 wrong-input cases, bad length), 1 ctor sanity.
  - Full forge suite: **151/151 passing** (143 + 8). Cumulative on-chain attestation cost (1A or 1B path, single Groth16 verify) is well under 250 k gas — i.e. 2× ECDSA cost — and a 3-Groth16 per-block path lands at ≈ 700 k. R4 closure stands.
  - Circuit 1A keygen costs: VK 103 s, PK 66 s + 2 s serialize, primary proof gen ~160 s (debug build) — measured on the same dev box. Cached in `params/primary_{vk,pk}.bin` + `params/primary_config_params.json`. Combined with Fallback (6.5 GB), total disk for the 1A+1B keys is ~9.8 GB.
  - Re-sequence: of the four `gnark-wrappers/{circuit-1a,circuit-1b,circuit-2,circuit-3}` slots, **2/4 are now live**. Remaining work: (a) Circuit 2 (LayerHashesMovementChecker) once AlinaT signals 1.B unblock, (b) Circuit 3 (BkSetChange) once partner stub is wired.
- **2026-05-07 (Phase 3.1 — first slice complete)**: Wired the gnark Groth16 path end-to-end for Circuit 1B. New artefacts:
  - `crates/bridge-prover-orchestrator/src/proof_export.rs` (mirrors `layer-hashes-prover::proof_export`, byte-compatible JSON for the gnark wrapper).
  - `crates/bridge-prover-orchestrator/src/bin/export_fallback_proof.rs` (produces `proof.bin` + `instances.bin` + `halo2_proof.json` from a Phase 1.A round-trip).
  - `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-1b/` Go module patterned on the layer-hashes wrapper, configured for 4 public inputs `[envelope_hash, bk_set_poseidon, block_seq_no, last_seen_block_seqno]`. Matches `FallbackProofOutput::instances` exactly.
  - `contracts/ethereum/src/{IFallbackGroth16Verifier,IFallbackVerifier,FallbackVerifier,FallbackGroth16VerifierGenerated}.sol` — interface, adapter, gnark-generated verifier (renamed `Verifier → FallbackGroth16VerifierGenerated`).
  - `contracts/ethereum/test/FallbackVerifier.t.sol` — 8 tests: positive (real proof verifies at **~222 657 gas**), 5 negative (tamper, wrong envelope hash, wrong BK set commitment, wrong block_seq_no, wrong last_seen, bad length), 1 ctor sanity.
  - Full forge suite: **143/143 passing** (135 baseline + 8 new for Fallback).
  - Per-block verifyBlock gas projection (3 Groth16 verifies @ ~225 k each): **≈ 700 k gas** total — well under the 30 M+ that native Yul would have needed if it could even deploy. R4 fully resolved by the pivot.
- **2026-05-10 (Phase 4.1 — `AckiNackiBridge.verifyBlock` lands additively)**: Added the AN→ETH state machine to the existing `AckiNackiBridge.sol` rather than a parallel rewrite, preserving all 161 prior tests. Per the user's `additive` + `synthetic-bound` directive, the deposits/withdrawals/AAVE surface stays untouched, and three new immutable verifier slots + one new permissionless entry point are layered on. Work delivered:
  - **Bound test data**. New `crates/bridge-prover-orchestrator/src/bound_test_data.rs` wraps the partner's `bridge_test_data_gen::generator::generate_bridge_test_data(bk_set_size, num_layers, num_chain_steps)`. The partner's generator already produces a single scenario whose **Primary attestation BLS-signed bytes** and **Circuit 2 layer-hashes preimage + 3 SHA-Merkle siblings** all hash to the *same* 32-byte `block_id` (the 8-leaf envelope tree root). On top of that, `bridge_poseidon::compute_bk_set_poseidon` gives us the matching `bk_set_poseidon` Fr; the helper assembles both 4-input (1A/1B) and 14-input (2) public-instance vectors and a `DenseChainLink`-typed prev-chain witness. Optional `with_fallback` flag also signs a Fallback envelope over the same `block_id` (re-using `create_attestation_data` + `sign_attestation_multi` from the partner crate). Lib-only patch: 0 prover changes, 0 keygen reruns — the cached K=20 1A and K=17 2 keys still apply, since circuit shape doesn't depend on witness data.
  - **Bound proof binary**. New `src/bin/export_bound_block_proofs.rs` drives both Circuit 1A (`generate_primary_proof`) and Circuit 2 (`generate_layer_hashes_proof`) from one `build_bound_test_data(10, 5, 3)` call. Writes `proofs/bound/primary/{proof.bin,instances.bin,halo2_proof.json}`, `proofs/bound/layer-hashes/{...}`, and `proofs/bound/bound_scenario.json` summary. End-to-end run: ~3.5 min wall time on dev box (Circuit 1A prove dominates at ~3 min; Circuit 2 prove ~10 s; everything else loads from cache). Both gnark wrappers (`circuit-1a/`, `circuit-2/`) re-`prove`d in <100 ms each — the gnark Groth16 setup is bound to the Halo2 VK shape, not the proof bytes, so cached `proving.key`/`verification.key`/`Groth16Verifier.sol` continue to apply (no `setup` rerun needed). Resulting Groth16 proofs are 256 bytes each.
  - **Solidity bridge surface**. `contracts/ethereum/src/AckiNackiBridge.sol` extension adds: storage (`storedBkSetCommitment`, `storedLastSeenBlockSeqNo`, `storedNumLayers`, `storedLayerHashes[10]`, `storedPrevMaxLevelLayerHash`), three `immutable` verifier slots (`primaryVerifier`, `fallbackVerifier`, `layerHashesVerifier`), a `FinalizationType { Primary, Fallback }` enum, and one entry point: `verifyBlock(finType, attestationProof, layerHashesProof, blockId, bkSetCommitment, blockSeqNo, numLayers, layerHashes[10], prevMaxLevelLayerHash)`. The constructor gains a 6th `VerifyBlockConfig` struct argument (verifier triple + genesis BK-set commitment + genesis chain anchor). Passing all-zeros (`VerifyBlockConfigLib.disabled()`) is a feature flag — `verifyBlock` reverts with `VerifyBlockDisabled`, so legacy deployments that only need deposit/AAVE keep working unchanged. Cross-circuit + invariant checks enforced by `verifyBlock`:
    - `bkSetCommitment == storedBkSetCommitment` → `BkSetCommitmentMismatch(supplied, stored)`;
    - `blockSeqNo > storedLastSeenBlockSeqNo` → `BlockSeqNoNotMonotonic(supplied, stored)`;
    - `prevMaxLevelLayerHash == storedPrevMaxLevelLayerHash` → `PrevAnchorMismatch(supplied, stored)`;
    - `1 ≤ numLayers ≤ MAX_LAYER_HASHES (10)` → `InvalidNumLayers(numLayers)`;
    - `layerHashes[i] == 0` for `i ≥ numLayers` → `LayerHashTailNonZero(index)`;
    - `primaryVerifier`/`fallbackVerifier` rejects → `AttestationProofRejected`;
    - `layerHashesVerifier` rejects → `LayerHashesProofRejected`.
    State updates use CEI (revert before any mutation, then `storedLastSeenBlockSeqNo = blockSeqNo`, copy `layerHashes[]`, set `storedPrevMaxLevelLayerHash = layerHashes[numLayers - 1]`). Cross-circuit `block_id` and `bkSetCommitment` equality between proofs is enforced *implicitly* by feeding a single value to both verifier calls — any mismatch surfaces as one of the two verifications failing.
  - **Existing call-site updates**. The 6-arg ctor is wired in `test/{AckiNackiBridgeAave,AckiNackiBridgeV2,FuzzVerifiers}.t.sol` and `script/{DeployRealBridge,DeployTestBridge}.s.sol` via a new `test/helpers/VerifyBlockConfigLib.sol` (`disabled()` + `with(...)` builders). All existing 161 tests continue to compile and pass without behavioural changes.
  - **New Foundry suite**. `test/AckiNackiBridgeVerifyBlock.t.sol` — **17 tests, all green**: (positive) `genesisState_seededFromConstructor`, `verifyBlock_primary_bound_succeeds_andUpdatesState` (uses real bound 1A + 2 Groth16 proofs end-to-end through both verifier adapters → on-chain Groth16VerifierGenerated.sol — verifies @ ~700 k gas including the bridge wrapper, well below the 1 M target), `storedLayerHashes_view_matches_individual_slots`, `verifyBlock_fallback_routesToFallbackVerifier` (mock), `verifyBlock_replayAfterSuccess_reverts`; (negative) `numLayers={0,11}_reverts`, `layerTailNonZero_reverts`, `bkSetMismatch_reverts`, `seqNoNotMonotonic_reverts`, `prevAnchorMismatch_reverts`, `tamperedAttestationProof_reverts`, `tamperedLayerHashesProof_reverts`, `blockIdMismatchAcrossProofs_reverts`, `fallback_rejected_byMock_reverts`, `disabled_revertsOnFreshBridge`, `partiallyWired_revertsAsDisabled`. Mock helper `test/mocks/MockFallbackVerifier.sol` covers the Fallback path (real Fallback verifier already proven independently in `FallbackVerifier.t.sol`).
  - **Cumulative state**. Foundry: **178/178 tests passing** (161 baseline + 17 new). `forge fmt --check` clean; `forge build` clean; `cargo check --lib -p bridge-prover-orchestrator` clean. The Phase 4 acceptance criteria from §3 of this plan are met *modulo* (a) sequential 2-block chain-anchor advancement (needs a second bound scenario whose `prev_max_level_layer_hash` equals our first scenario's `LAYER_HASH_4`; trivially generated by extending the binary, deferred until Phase 5 relayer drives it) and (b) Circuit 3 wiring (gated on Phase 1.C signal). The integration-plan §3 task list explicitly mentioned deleting `LayerHashBridge.sol` and rewriting all its tests; we deferred that demolition (kept it deprecated-but-working) per the user's `additive` choice — reduces blast radius until Phase 5/6 land and the relayer-driven path replaces it organically.
  - **Side notes**. `bincode = "1.3"` added as a direct dep of the orchestrator (already a transitive dep, but we serialize Fallback envelopes directly in `bound_test_data.rs::build_fallback_attestation_envelope`). Per-block on-chain verify cost lands at **~700 k gas** end-to-end (1A 222 k + 2 293 k + bridge wrapper overhead ~180 k for storage writes + cross-checks); adding Circuit 3 once 1.C lands brings worst-case to ~1.0 M, still under the 1 M target. The on-chain `BlockVerified(blockId indexed, blockSeqNo indexed, finType, numLayers)` event gives the relayer a clean ack to track.
- **2026-05-10 (Phase 5.1 — relayer skeleton lands; Q1/Q2 unblock partial)**: Phase 5 from §3 is officially blocked on Q1 + Q2 (live testnet exposing the new envelope format + node-team migration of `transition_hashes`). Splitting Phase 5 into three sub-phases — **5.1 skeleton (now, no partner dep)**, **5.2 live source (when Q1/Q2 land)**, **5.3 acceptance (10-block shellnet)** — lets us land most of the relayer's surface area while the unblocking conversation continues. Work delivered:
  - **New crate `crates/bridge-relayer-daemon/`** — own package (excluded from workspace, mirrors `bridge-prover-orchestrator` standalone layout; the relayer doesn't pull halo2 into the workspace tree). Modules: `types` (`AnBlockData`, `FinalizationType`, `MAX_LAYER_HASHES = 10`, `validate_shape` helper), `bridge` (`BridgeClient` async trait + `EthBridgeClient` wrapping `abigen!`-generated bindings + `MockBridgeClient` mirroring the contract's verifyBlock state machine in-memory for fast unit tests), `source` (`BlockSource` async trait + `InMemoryBlockSource` for unit tests + `FixturesBlockSource` reading the Phase 4.1 bound proof artefacts from disk), `state` (`RelayerState` with atomic write-temp-then-rename `state.json` persistence), `relayer` (`Relayer::tick()` + `Relayer::run_loop(max_ticks, should_stop)` orchestration), and a `relayer` CLI binary with a `smoke-fixture` subcommand that reads a fixture dir + ethers signer + bridge address and submits one canned block.
  - **Solidity mocks for relayer-loop driving**. `test/mocks/MockPrimaryVerifier.sol` and `test/mocks/MockLayerHashesMovementVerifier.sol` mirror the existing `MockFallbackVerifier`: `view`-mutability, `setShouldAccept(bool)` setter; suitable for driving `verifyBlock` through many sequential blocks without ZK proof generation.
  - **New Foundry suite `test/AckiNackiBridgeRelayerLoop.t.sol`** — **6 tests, all green**, covering: (positive) `test_relayerLoop_10Blocks_mixedFinTypes_advancesState` (10 sequential blocks, mix of Primary/Fallback every 3rd, asserts `storedLastSeenBlockSeqNo`/`storedNumLayers`/`storedLayerHashes[..]`/`storedPrevMaxLevelLayerHash` after every step; one `BlockVerified` event per block, 593 k gas total), `test_relayerLoop_restartReadsCurrentAnchor` (encodes the relayer's restart-from-on-chain-anchor recovery strategy); (negative) `test_relayerLoop_replaySameSeqNo_reverts`, `test_relayerLoop_seqNoFastForward_isPermittedByContract` (documents that the contract permits gaps; the *relayer* is what enforces `target = last_seen + 1`), `test_relayerLoop_onAttestationReject_stateUntouched` (CEI invariant), `test_relayerLoop_anchorMismatch_reverts`. This is the **on-chain side** of the §5 acceptance criterion ("relayer can drive 10 sequential blocks"); the off-chain side is covered by the relayer crate's unit tests.
  - **Rust unit tests in `bridge-relayer-daemon`** — **13 tests, all green**: `state` (2: roundtrip persistence, attempt counter), `source` (2: `InMemoryBlockSource` returns `None` when missing, `FixturesBlockSource` only serves its fixed seqno), `bridge` (4: `MockBridgeClient` advances through 3 blocks, reverts on seqNo replay, reverts on anchor mismatch, reverts when verifier rejects — all with the same revert-string shape the live bridge uses), `relayer` (5: `tick_advances_through_five_blocks`, `tick_returns_not_yet_available_when_source_empty`, `tick_handles_verifier_rejection_then_recovers`, `restart_resumes_from_persisted_state_and_on_chain_anchor` (drops + reconstructs the `Relayer` mid-loop and asserts state survives), `run_loop_stops_when_should_stop_returns_true`).
  - **Cumulative state**. Foundry: **184/184 tests passing** (178 baseline + 6 new). Rust relayer: 13/13 tests passing. `cargo check --workspace` clean; `cargo check --all-targets` on `bridge-prover-orchestrator` clean. `forge fmt --check` + `forge build` clean. Phase 5.1 acceptance criteria: ✅ relayer compiles and runs an in-process 5-block loop end-to-end; ✅ contract drives a 10-block loop with mixed finalization types; ✅ restart-from-on-chain-anchor works; ✅ revert classification matches what the live bridge would emit (mock parity is asserted in the bridge module). Phase 5.1 *does not* claim acceptance against a live AN node — that's Phase 5.3.
  - **Decisions**. (a) The relayer crate is excluded from the workspace, like `bridge-prover-orchestrator`, even though it doesn't pull halo2 — keeps Cargo.toml dependency mirroring future-proof when Phase 5.2 wires the orchestrator. (b) `MockBridgeClient` is *byte-for-byte* faithful to the contract's revert-string shape (`BkSetCommitmentMismatch(supplied=…, stored=…)` etc.); this lets tests assert on revert reasons without re-encoding ABI selectors. (c) The `BridgeClient::submit_block` API returns `SubmitOutcome::{Verified, Reverted}` rather than `Result<(), RelayerError>` — reverts are *expected* outcomes the relayer must classify (e.g., re-fetch on `PrevAnchorMismatch`) rather than terminal errors. (d) State persistence is atomic-rename-only, no fsync — Phase 5.3 will tighten this if shellnet operation reveals a real durability gap.
- **2026-05-10 (Phase 4.2 — legacy demolition lands; v2 becomes the only AN→ETH path)**: With Phase 4.1 (bridge surface) and Phase 5.1 (relayer skeleton) both green, the legacy single-circuit infrastructure no longer carries any unique invariant. Per the original §3 plan we retire it now rather than letting it bit-rot through Phase 5.2/5.3. Work delivered:
  - **Solidity demolition**. Deleted 8 files: `contracts/ethereum/src/{LayerHashBridge,LayerHashVerifier,LayerHashGroth16Verifier,LayerHashGroth16VerifierGenerated,ILayerHashVerifier,BkSetRotationVerifier,BkSetRotationGroth16Verifier,IBkSetRotationVerifier}.sol`. The `AckiNackiBridge.verifyBlock` permissionless entry point is now the *only* AN→ETH state-transition surface; the future BK-set update will plug into it as an optional fourth proof argument once Phase 1.C / 3.4 land — preserving the cross-circuit consistency story (single `block_id` + single `bk_set_commitment` flowing into all proofs in one block).
  - **Foundry test demolition + re-coverage proof**. Deleted `contracts/ethereum/test/{LayerHashBridge,LayerHashE2E}.t.sol` (49 tests total: `LayerHashBridgeTest` 27, `LayerHashVerifierTest` 4, `BkSetRotationVerifierTest` 4, `LayerHashE2ETest` 14). Net Foundry: 184 → **135 tests** across 15 suites. Coverage matrix of the 49 retired tests against surviving suites:
    - **State machine** (set commit, sequential update, replay, monotonicity, anchor): covered by `AckiNackiBridgeVerifyBlockTest` (17) + `AckiNackiBridgeRelayerLoopTest` (6) on the v2 ABI.
    - **Verifier adapter shape** (input encoding, calldata length, gnark revert→false): covered per-circuit by `PrimaryVerifierTest` (8) + `FallbackVerifierTest` (8) + `LayerHashesMovementVerifierTest` (10) — *more granular* than the legacy 13-input `LayerHashVerifierTest`.
    - **Owner/admin/timelock surface**: deliberately gone — `AckiNackiBridge` v2 uses **immutable** verifier addresses (no setVerifier/setBkRotationVerifier). Emergency BK-set commitment switch will return via Phase 1.C (`bkSetUpdateProof`); 7-day legacy timelock is replaced by an on-chain ZK rotation proof.
    - **Real-proof multi-fixture E2E** (the 14 `LayerHashE2ETest` cases against L2_H16 / L2_H32 / L5 / L6): retires with no immediate replacement; will return as **Phase 5.3** acceptance (relayer-driven N-block sequence from shellnet against Anvil), which is a strictly stronger test (real chain anchoring, not synthetic fixtures).
  - **Rust demolition**. Deleted `layer-hashes-prover/` (whole tree: `src/`, `gnark-wrapper/`, `proofs/`, `params/`, `Cargo.toml`, `Cargo.lock`, `target/`) — the standalone Halo2 export → 13-input gnark wrap pipeline. Deleted `bk-set-rotation-prover/` (`CIRCUIT_SPEC.md` + 2-input gnark wrapper). Workspace `Cargo.toml` no longer carries `"layer-hashes-prover"` in `exclude`. The v2 equivalents already live under `crates/bridge-prover-orchestrator/` (Halo2 + Rust prover) and `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-{1a,1b,2}/` (Go gnark wrappers, 4/4/14 inputs respectively).
  - **Cleanup of incidental references**. `FallbackVerifier.sol` doc-comment "Mirrors `LayerHashVerifier.sol`" updated to point at the v2 sibling adapters. Other surviving references live only in the legacy docs (`docs/integration_plan.md`, `docs/manual_verification_runbook.md`, `docs/bridge_verification.md`, `docs/verifying_an_proof.md`, `docs/verifying_eth_proof_on_an.md`) — those describe the retired architecture and are intentionally kept as historical reference until **Phase 7** rewrites them for v2; `AGENTS.md` flags this explicitly.
  - **Cumulative state**. Foundry: **135/135 passing** across 15 suites (`forge fmt --check` ✓, `forge build` ✓). Rust: `cargo check --workspace` ✓, `cargo check --all-targets -p bridge-prover-orchestrator` ✓, `cargo check -p bridge-relayer-daemon` ✓; relayer's 13 unit tests still green. The bridge repo now contains **no contract code, no Rust code, and no Foundry test code** that references the legacy single-circuit architecture — a measurable reduction of the audit surface for the upcoming v2 review.
  - **Decisions**. (a) Demolish *before* Phase 5.3, not after — leaving deprecated-but-working code through 5.2/5.3 risked accidental re-import of legacy contracts in fresh tests/scripts. (b) Real-proof E2E coverage gap (the 14 `LayerHashE2ETest` cases) is accepted for the duration of Phase 5.2; Phase 5.3 supersedes it with relayer-driven shellnet sequences against Anvil — strictly stronger than the per-fixture replays. (c) Doc rewrites (`integration_plan.md` etc.) are deliberately deferred to Phase 7 — they describe an architecture that's gone, but rewriting them mid-stream would invalidate the existing audit-pre-read; we'd rather have one consolidated v2 doc set at release time.
- **2026-05-17 (R14 sharpened; Phase 8 added — trusted setup ceremony as explicit mainnet gate)**: Internal review surfaced that R14 in the risk register was tracked at "Medium" severity with a mitigation that said "document the ceremonies" — but no actual ceremony work was scheduled, and the four wrappers (`deposit-prover/gnark-wrapper`, `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-{1a,1b,2}/`) all call `groth16.Setup(ccs)` in-process. That means dev-grade Phase 2 keys with toxic waste held on a single developer machine. Since the on-chain verifier only checks the Groth16 wrapper (not the Halo2 SHPLONK proof underneath), anyone with that waste can forge wrapper proofs and bypass the entire cryptographic chain — a critical soundness gap for mainnet. Concrete actions taken in this commit: (a) Phase 3.4 reworded to acknowledge the dev-grade `Setup` is intentional for testnet only, with explicit pointer to Phase 8; (b) R14 upgraded from "Medium" to **"Critical for mainnet"** with a precise description of the gap and the four artefacts at risk; (c) new **Phase 8 — Trusted Setup Ceremony** added after Phase 7 with 5 numbered tasks (8.1 Phase 1 SRS adoption from a public ceremony like Hermez `pot28_final.ptau`; 8.2 Phase 2 MPC per circuit with N ≥ 5 independent contributors; 8.3 on-chain verifier rotation with re-deploy; 8.4 audit trail with reproducibility script; 8.5 CI guard against regression to single-party Setup); (d) §6 time estimate updated — Phase 8 is ~5 engineering days but **2–3 weeks calendar-bound** by the MPC contribution chain, pushing mainnet-ready timeline from 6 to 9–10 weeks; (e) overall: testnet/RC tags remain unblocked by Phase 8, mainnet `v2.0.0` is now explicitly gated on it.
- **2026-05-17 (R15 surfaced; Phase 8 inserted as real-wrapper R&D; Phase 8 ceremony → Phase 9; rc1 cryptographic claim blocked)**: While preparing a `verification.key` + `circuit.go` handoff pack for Alina (so AN-side could wire `gosh.vergrth16WithVK` into `TokenBridge.finalizeDeposit`), opened `deposit-prover/gnark-wrapper/circuit.go` and discovered that the wrapper's `Define()` adds **zero** real constraints — only `PublicInputs[i] == PublicInputs[i]` identity assertions for 7 values + `DomainSize != 0`. Confirmed by reading `circuit_test.go` (line 183: `t.Logf("Circuit constraints not satisfied (expected - full verification not implemented): %v", err)`) and by R1CS / PK / VK sizes (961 B / 1 581 B / 556 B respectively — a real Halo2 SHPLONK verifier in-circuit would be MB-scale). The same pattern holds across all four wrappers (`crates/bridge-prover-orchestrator/gnark-wrappers/circuit-{1a,1b,2}/`) — they share the same skeleton. Consequence: an attacker with the (single-party) `proving.key` can generate a valid Groth16 proof for any tuple of public inputs, completely bypassing Halo2 verification — the on-chain verifier never sees Halo2, only the wrapper's Groth16 output. All gas figures, all Foundry "tampered proof rejected" assertions, and all of the cryptographic-claim language in the v2 doc set were measuring / asserting against the stub. **No artefacts were handed to Alina or any partner**.
  - **User decision: Option A (implement the real wrapper)**.
  - **Concrete plan changes in this commit**:
    - **New Phase 8** ("Real Halo2 SHPLONK Verification in gnark Wrappers"): R&D + prototype + fan-out + adversarial testing + Solidity verifier rotation. 6 numbered tasks. 8–16 calendar weeks for 4 circuits. Blocks any tag with a "verifies Halo2 on-chain" claim, including `v2.0.0-rc1`.
    - **Old Phase 8 (Trusted Setup Ceremony) → Phase 9**: explicitly downstream of Phase 8. Running the ceremony against today's stub R1CS would produce an MPC-secured no-op, which is not useful.
    - **R15 added** to Phase 3 risk list and §5 risk register. **R14 retained** but explicitly marked downstream of R15.
    - **Phase 3.4 reworded**: dev-grade `Setup` is intentional only for ABI smoke-testing; the stub character of the wrapper is now explicit.
    - **Phase 7.9 release-gate split** into three tags: `v2.0.0-pre-rc1-stub` (today, with explicit no-op disclaimer in release notes — useful for AN-side wiring integration tests), `v2.0.0-rc1` (after Phase 8), `v2.0.0` (after Phase 8 + Phase 9).
    - **§6 time estimate** revised: rc1-ready calendar 14–22 weeks; mainnet-ready 17–25 weeks. This is the **first public schedule revision** acknowledging the wrapper gap; prior estimates ("9–10 weeks mainnet-ready") were measuring against the stub.
  - **Handoff to Alina deferred** until Phase 8 produces a real `verification.key` + non-stub `circuit.go`. The `gnark.vergrth16WithVK` opcode work in tvm-sdk is still useful — it's the AN-side glue, independent of the gap finding — but should not be wired against the current stub VK.
  - This finding does not retroactively break anything that worked before. All Foundry tests continue to pass; all gas measurements continue to hold; the relayer + bridge + AN-side glue all continue to function. What it *does* break is the **interpretation** of those passing tests as cryptographic guarantees of forgery resistance. We're trading "we already verified Halo2 on-chain in v2" (false) for "we have a complete-glue moving testbed that *will* verify Halo2 on-chain once Phase 8 lands" (true). This is the honest version of where we are.
- **2026-05-17 (later — pivot: drop Groth16 on ETH→AN; Phase 4.3 demolition starts)**: After surfacing R15 the user revoked the prior decision ("Option A: implement real Halo2-in-gnark wrapper"). New direction: **remove Groth16 wherever it is not architecturally forced**. The architectural forcing function is EIP-170 — it applies *only* to the ETH-side runtime. On the AN side there is no equivalent contract-size limit and Halo2 SHPLONK can be verified natively via a new TVM opcode. Therefore:
  - **ETH → AN direction (deposit-event proofs)**: Groth16 wrapping is **eliminated**. The AN side will verify the deposit-prover's Halo2 SHPLONK proof directly through a new `VERHALO2SHPLONK` TVM opcode (under development in tvm-sdk, modelled on the existing `VERGRTH16WITHVK` pattern landed last week). This means the entire `deposit-prover/gnark-wrapper/` + `deposit-prover/src/groth16_wrapper/` + `contracts/ethereum/src/Groth16{Verifier,DepositVerifier}.sol` + `IAckiNackiVerifier.sol` + `DummyVerifier.sol` + `AckiNackiBridge.withdraw()` chain becomes dead code and is being retired in Phase 4.3 (this commit + the pending Solidity surgery commit).
  - **AN → ETH direction (Circuit 1A/1B/2/3 proofs)**: Groth16 wrapping is **retained** for now. EIP-170 makes the native Halo2 Yul verifier (~78 KB) undeployable, and we have no other compact wrapper today. The wrappers under `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-{1a,1b,2}/` are still stub-based (R15 unchanged for this direction). Phase 8 (real Halo2-in-gnark wrapper R&D) **remains required for AN→ETH** until/unless an alternative (recursive Halo2 aggregation under 24 KB, optimistic bridge with challenge period, custom split-bytecode verifier) is chosen for that side. That decision is **explicitly deferred** to a later turn — this commit doesn't pick the AN→ETH winner.
  - **Phase 4.3 (legacy deposit-verifier demolition)** — work in flight in this commit:
    - ✅ Deleted `deposit-prover/gnark-wrapper/` (entire dir — Go module, R1CS, keys, generated `Groth16Verifier.sol`, transcripts, `optimize_verifier.sh`).
    - ✅ Deleted `deposit-prover/src/groth16_wrapper/` (Rust JSON adapter for gnark input).
    - ✅ Removed `pub mod groth16_wrapper;` from `deposit-prover/src/lib.rs`; `cargo check -p deposit-prover --lib` clean.
    - ✅ Deleted top-level deposit-flow e2e scripts: `test_e2e.sh`, `test_e2e_negative.sh`, `test_e2e_negative_attack.sh`, `test_fuzz_e2e.sh`, `check_e2e_prerequisites.sh`.
    - ✅ Deleted deposit-prover side scripts: `setup_gnark.sh`, `test_groth16_wrapper.sh`, `test_e2e_onchain.sh`, `test_e2e_sepolia.sh`, `test_multiple_proofs.sh`, `generate_and_deploy.sh`.
    - ✅ Deleted gnark-related Rust examples: `deposit-prover/examples/{export_proof_for_gnark.rs,analyze_proof_structure.rs}`.
    - ⏳ **Pending (next commit, awaiting user sign-off on the precise diff)**: Solidity surgery. Delete `Groth16Verifier.sol`, `Groth16DepositVerifier.sol`, `IAckiNackiVerifier.sol`, `DummyVerifier.sol`; surgically remove `withdraw()`, `verifier` storage slot, `processedDeposits` mapping, `isDepositProcessed()` view, `event Withdrawal`, `_verifier` ctor param, and the four errors (`InvalidProof`, `InvalidVerifier`, `InvalidBlockHash`, `DepositAlreadyProcessed`) from `AckiNackiBridge.sol`; delete `test/AckiNackiBridgeV2.t.sol` (full suite — pure withdraw flow) + `test/FuzzGroth16Verifier.t.sol` + `test/FuzzGroth16DepositVerifier.t.sol` (+ deposit-flow tests in `FuzzAckiNackiBridge.t.sol` / `FuzzVerifiers.t.sol`); update `test/AckiNackiBridge{Aave,VerifyBlock,RelayerLoop}.t.sol` to drop the `PermissiveVerifier`/`PermissiveTestVerifier` ctor argument; rewrite `script/Deploy{Real,Test}Bridge.s.sol`; update docs (`README.md`, `AGENTS.md`, `four_circuit_architecture.md`, `bridge_verification.md`, `integration_analysis.md`, `audit_trail_v2.md`, `manual_verification_runbook.md`, `verifying_eth_proof_on_an.md`, `BLAKE2B_HALO2_VERIFIER.md`).
  - **New track — tvm-sdk Halo2 SHPLONK opcode** (`crates/bridge-prover-orchestrator`-adjacent, but happens in `tvm-sdk` repo on a new branch, mirroring the `VERGRTH16WITHVK` PR pattern from last week): design + skeleton + tests for `VERHALO2SHPLONK` (working name). Estimate **≥ 5 working days** (heavier than `VERGRTH16WITHVK` because Halo2 SHPLONK requires Fiat–Shamir transcript replay + multiple BN254 pairings + KZG opening checks; the closest reusable Rust code is in the gosh fork of halo2-pse). Companion PR in TVM-Solidity-Compiler exposes `gosh.verHalo2Shplonk(proof, publicInputs, vk)`. Not started in this commit.

    **Update 2026-05-17 (this commit)**: discovered that the AN partner (Serhii) already has a `ZKHALO2VERIFY` opcode in flight on `tvm-sdk` branch `serhii/node-3406-vergrth16-with-vk` at dispatch byte `0xC7 0x49`, registered alongside our `VERGRTH16WITHVK`. Properties: handler at `tvm_vm/src/executor/zk_halo2.rs`, uses `gosh-zk-snark-halo2-utils::Proof::verify_with_vk` (Blake2b SHPLONK transcript), `OnceLock`-cached `(VK, ParamsKZG<Bn256>)` with a background warmup thread (`warmup_halo2()`), N×32-byte LE Fr public-input layout with a u64-shortcut dual-path parser. **Critical gap for our use-case**: the VK is **hard-coded to the DarkDex W=8 circuit** (`DARK_DEX_W8_VK_BYTES: [u8; 842]`, K=19) — exactly the same structural problem `VERGRTH16` had vs. the bridge before we landed `VERGRTH16WITHVK`. Bridge needs a per-circuit VK (the `deposit-prover/` Halo2 circuit's VK), so we need a **`ZKHALO2VERIFYWITHVK`** sibling opcode that takes the VK as a third stack operand.
    - Bridge-side design memo (full open-questions list, gas model, wire-format proposals, per-VK LRU cache sketch, roadmap Phases A–F): **[`docs/zk_halo2_an_side_design.md`](./zk_halo2_an_side_design.md)**.
    - tvm-sdk discussion artifact (skeleton handler at `0xC7 0x4A` returning `FatalError` pending Phase A, mnemonic + gas constant + assembler-side dispatch-byte regression test, mirror design doc): tvm-sdk branch [`serhii/verhalo2shplonk-skeleton`](https://github.com/tvmlabs/tvm-sdk/tree/serhii/verhalo2shplonk-skeleton), commit `Add ZKHALO2VERIFYWITHVK skeleton + design notes`. Branched off `main` (not Serhii's branch) so it's reviewable independently of merge order; will rebase + flesh out once Serhii's PR lands.
    - Net effect: working name shifts from `VERHALO2SHPLONK` to `ZKHALO2VERIFYWITHVK` to align with the partner's naming. Companion PR in TVM-Solidity-Compiler will expose `gosh.zkHalo2VerifyWithVK(proof, publicInputs, vk)`.
  - **Net effect on the schedule**:
    - Mainnet timeline for the **ETH → AN deposit** path: no longer gated by Phase 8 R&D + Phase 9 ceremony (both retired for this direction since there is no Groth16 wrapper anymore). New gating: the `VERHALO2SHPLONK` opcode landing in `tvm-sdk` (≥ 1 week eng work) + AN-team adopting it in their Solidity-compiler `gosh.*` namespace + AN-team wiring it into `TokenBridge.finalizeDeposit`. Substantially shorter than the Phase 8 + Phase 9 path.
    - Mainnet timeline for the **AN → ETH state** path: unchanged. Phase 8 (real wrapper) + Phase 9 (ceremony) still required as written, unless we pivot to an alternative (recursive aggregation / optimistic) in a later decision. The 14–22 week / 17–25 week estimates in §6 still apply to this direction.
  - **What is irrevocably gone**: legacy v1 refund-style `withdraw(depositId, recipient, amount, blockNumber, proof)` semantics — i.e. "I deposited on ETH and want my ETH back, here's a Halo2 proof of my own Deposit event". A genuine cross-chain withdrawal (i.e. *I burned tokens on AN, now release ETH on ETH-side*) was never implemented in v1 anyway; the §3 Phase 4 open design question explicitly recommended postponing it to a future milestone, and that recommendation is now committed.
- **2026-05-17 (later still — Phase A Circuit 4 scaffolding lands)**: User chose **Option A** from the post-alloy-migration next-step menu: implement Circuit 4 (`bridge-event-prove-circuit`) — the AN→ETH withdrawal-event prover Alina is working on. After studying the partner circuit (`acki-nacki-to-eth-bridge-halo2-circuits/bridge-event-prove-circuit/src/`, see `EVENT_LAYOUT_COMPARISON.md`) we confirmed the circuit keeps `dstChainId`, `amount`, `recipient`, `sender` as **private** witnesses — so a real `withdraw()` on Ethereum cannot be built on top of it today (the contract would not know how much to pay or to whom). We split the work into Phase A (this commit, no partner dependencies) and Phase B (real `withdraw()`, gated on five circuit-side design questions tracked in `docs/circuit_4_open_questions.md`).
    - **Phase A — landed in this commit (16 new Foundry tests, 125/125 suite total green)**:
      - `AckiNackiBridge.sol` gains a rolling `_layerWindow[100]` ring buffer + `layerWindowHead` monotonic counter; every successful `verifyBlock` pushes `layerHashes[numLayers-1]` (the new top-of-chain anchor) into one slot and emits `LayerWindowPushed(slot, anchor, headAfter)`. Maintained even when Circuit 4 isn't wired so an opt-in deployment doesn't have to back-fill 100 blocks.
      - `AckiNackiBridge.sol` gains two immutables `bridgeEventDappFr` and `bridgeEventAccFr` pinning the AN-side `TokenBridge` identity the proof must bind to.
      - `AckiNackiBridge.verifyEvent(proof, tokenId) → bool` — permissionless, assembles the 103-element public-input vector (3 immutables + 100 ring-buffer slots), forwards to `IBridgeEventVerifier`, emits `BridgeEventVerified(tokenId, msg.sender)` on success. **Does NOT move money** — Phase A is intentionally non-paying.
      - New Solidity surface: `IBridgeEventVerifier.sol` + `BridgeEventVerifier.sol` adapter + `IBridgeEventGroth16Verifier.sol` (103 public inputs) — mirrors `ILayerHashesMovementVerifier` / `LayerHashesMovementVerifier` pattern verbatim.
      - New constructor argument: `BridgeEventConfig` bundle (independent of `VerifyBlockConfig`; a deployment can wire one without the other). Disabled by default → `verifyEvent` reverts `VerifyEventDisabled`.
      - gnark wrapper skeleton: `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-4/` — identity-stub mirror of `circuit-2/` with `NumPublicInputs = 103`; builds clean (`go build` produces a 20 MB binary, gitignored alongside its siblings).
      - Tests: 16 new in `AckiNackiBridgeVerifyEventTest` covering constructor wiring (4), layerWindow plumbing (5 inc. 102-block ring-buffer wrap-around), and verifyEvent behaviour (7 inc. identity/window forwarding via a strict-mode mock).
      - Documentation: `docs/circuit_4_open_questions.md` (the Phase A→B blockers), `docs/four_circuit_architecture.md` §11 (Circuit 4 section + table row + trust-model delta), `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-4/README.md`, this Decision Log entry.
    - **Phase A trade-offs / explicit non-goals**:
      - `verifyEvent` is **replayable** (a dedicated Foundry test asserts this is deliberate, with a TODO marker referencing Q-CIRC4-2). A nullifier registry will land in Phase B alongside a circuit-side nullifier.
      - The contract has no `withdraw()` — see Q-CIRC4-1 (private `amount`/`recipient`), Q-CIRC4-3 (undefined `dstChainId` semantics).
      - The gnark wrapper is a no-op stub, identical to R15 in the risk register. Real Halo2 verification still lands via Phase 8 (which then applies to the four-circuit verifyBlock flow + Circuit 4 uniformly).
    - **Phase B blockers (full list in `docs/circuit_4_open_questions.md`)**: Q-CIRC4-1 (public `amount`/`recipient`), Q-CIRC4-2 (nullifier), Q-CIRC4-3 (`dstChainId` semantics), Q-CIRC4-4 (variable-length recipient — defer to Phase C), Q-CIRC4-5 (trusted setup ceremony scope).
    - **What this unlocks for Alina**: she can iterate on the partner circuit knowing the Ethereum side is ready to consume *whatever* public-input layout the v2 of `bridge-event-prove-circuit` settles on, modulo only the adapter signature in `BridgeEventVerifier.sol`. Window-management (the genuinely-novel storage piece) is fully done and stable.

- **2026-05-18 (BK rotation detection plumbing + Alina review pack + CI gap fix)**: Three independent, parallelisable workstreams shipped against `main` while waiting on partner answers to Phase 5.2/5.3 blockers.
    - **BK-set rotation detection — full vertical slice (Phase 1.C precursor)**: We now have an end-to-end pipeline that detects BK committee rotations against the live AN node and pauses the relayer until a human acknowledges, *without* requiring Circuit 3 to land. Three layers:
      1. `crates/acki-nacki-interface/` — `BkSetTracker` (stateful poller over `BkSetClient::fetch_bk_set_update`) emits `BkSetChange` events (`FirstObservation { snapshot }` / `Unchanged` / `MembershipChanged { added, removed, pubkey_mutations }`). Diff is computed on the canonical `BkSetSnapshot` shape so it survives reordering of the AN-node JSON response. `#[ignore]`-gated `live_bk_set_tracker_two_polls_against_real_node` exercises against `http://94.156.178.19:8600`.
      2. `crates/bridge-relayer-daemon/src/bk_set_sentry.rs` — `BkSetSentry<P: BkSetPoller>` wraps any `BkSetTracker` instance, classifies `BkSetChange` events into relayer-relevant `SentryStatus::{Bootstrapped, Quiet, RotationDetected { old_seq_no, new_seq_no, delta }}`, maintains `SentryMetrics { ticks, bootstraps, quiets, rotations }`, and provides a `run_until_stop(predicate)` orchestration loop. 6 unit tests via a `StubPoller`. Live `#[ignore]`-gated `live_sentry_bootstraps_then_quiet_or_rotation` test.
      3. `crates/bridge-relayer-daemon/src/guarded_relayer.rs` — `SentryGuardedRelayer<S, R>` composes a `BkSetSentry` and a `Relayer`. `tick()` returns `GuardedOutcome::{SentryBootstrapped, SentryQuiet { inner_tick: TickOutcome }, RotationDetected { … }, PausedAwaitingRotationReconcile }`. On rotation it sets an internal `paused = true` flag and refuses to drive the inner relayer until `resume()` is called explicitly — preserving the "stop submitting `verifyBlock` calls when the on-chain BK commitment is stale" invariant. 5 unit tests.
    - **`relayer` CLI integration**:
      - `smoke-fixture --an-node-url <URL>` now wraps the inner `Relayer` in `SentryGuardedRelayer` and drains `GuardedOutcome` instead of `TickOutcome`. Without `--an-node-url`, the legacy un-guarded path is taken (backwards compatible).
      - New `sentry-watch --ticks N` subcommand runs a standalone `BkSetSentry` against an AN node, prints each `SentryStatus` event, and exits. Useful for manual operator inspection ahead of relayer rollout.
      - Locally smoke-tested against the public testnet: `BOOTSTRAP` on tick 1, `QUIET` on subsequent ticks — as expected for a stable committee.
    - **Alina review pack delivered (2026-05-18)**: `dist/alina_review_pack_2026-05-18.tar.gz` (46 files / 165 KB / SHA-256 `44b485f0562bcc48484b24460800637e954152ebd55938ef2bf643c93ef992ea`) shipped to Alina containing (a) Halo2 SHPLONK VKs + `BaseCircuitParams` + sample Blake2b-transcript proofs + 32-byte LE Fr instances + gnark-adapter JSON for all three AN→ETH circuits (1A, 1B, 2); (b) `crates/bridge-prover-orchestrator/src/*.rs` integration glue + `Halo2TvmBundle` wire format + design memo; (c) full `deposit-prover/src/*.rs` source (Phase 4.3 retired but intact); (d) bound-scenario artefacts demonstrating 1A↔2 cross-circuit consistency. Pack pinned via `MANIFEST.sha256` (per-file) + `dist/alina_review_pack_2026-05-18.tar.gz.sha256` (top-level). Seven open questions (Q1: VK serde `RawBytes` vs `RawBytesUnchecked`; Q2: Blake2b vs Keccak transcript; Q3: K=17 layer-hashes vs K=20 primary/fallback split; Q4: add Fallback to bound scenario; Q5: future of retired `deposit-prover/`; Q6: review `Halo2TvmBundle` wire format; Q7: strengthen cross-circuit anchor to `(block_id, bk_set_poseidon, block_seq_no)` triple) queued in the pack README. Metadata + regen recipe committed to `docs/reviews/alina_review_pack_2026-05-18.md`; the tarball itself is out-of-tree via `dist/` in `.gitignore`.
    - **CI gap fix — `bridge-relayer-daemon` now in pipeline** (commit `4daa517`): four new GitLab jobs (`build:rust:relayer` / `test:rust:relayer` / `lint:rust:relayer:fmt` / `lint:rust:relayer:clippy`), all running against the crate's checked-in `Cargo.lock` with `--locked`. The relayer was previously invisible to CI because the main workspace `Cargo.toml` excludes it (independent `alloy` + `clap` dep tree), so `cargo {build,test,clippy} --workspace` and `cargo fmt --all` all skipped it — regressions could ship to `main` with green CI as long as the workspace compiled. The new jobs share a dedicated cache key `rust-relayer-$CI_COMMIT_REF_SLUG` and `needs:`-chain build → test+clippy for warm artifacts. Locally verified all four green on the current `main` (24 unit tests + 1 ignored live test). The `bridge-prover-orchestrator` crate has the same gap; tracking as future A2 (a comparable job is more expensive — halo2 deps take ~5 minutes to compile, and the params/PK files we need are multi-GB — so it'll likely be `only: [main, merge_requests]` rather than every-push).
    - **`verifyBlock` fuzz coverage (B4)**: `contracts/ethereum/test/FuzzAckiNackiBridgeVerifyBlock.t.sol` — 6 property-based fuzz tests (256 runs each, 1 536 random invocations) plus 1 plain unit test, covering the cheap pre-crypto invariants on `AckiNackiBridge.verifyBlock`: `VerifyBlockDisabled` for any input shape on a zero-verifier bridge; `InvalidNumLayers` for any `numLayers ∉ [1, 10]`; `LayerHashTailNonZero` for any non-zero element at index ≥ `numLayers`; `BkSetCommitmentMismatch` for any commitment ≠ stored; `BlockSeqNoNotMonotonic` against both genesis (stored = 0; a *unit* test rather than a fuzz target, because the only offending u64 is `0` — fuzzing it would trip Foundry's 65 536-rejected-inputs cap, as it did on pipeline #5741 commit `5188840c`) and post-advance (stored = 1, fuzzed with `bound(seqNoRaw, 0, 1)`); `PrevAnchorMismatch` for any anchor ≠ stored. The cryptographic-rejection paths (`AttestationProofRejected`, `LayerHashesProofRejected`) stay covered by the deterministic suite — fuzzing them needs a live prover, which is expensive. Foundry total now 132 / 14 suites (was 125 / 13).
    - **`DeployRealBridge` refresh (E4)**: The deployment script (`contracts/ethereum/script/DeployRealBridge.s.sol`) now supports two modes side-by-side: (a) default — bridge deployed with all three verifier slots zero (`VerifyBlockDisabled`), and (b) `WIRE_VERIFY_BLOCK=true` — deploys the `Primary`/`Fallback`/`LayerHashes` Groth16-generated verifier triple + the three adapter contracts (`PrimaryVerifier`/`FallbackVerifier`/`LayerHashesMovementVerifier`) and wires them into the bridge constructor with `GENESIS_BK_SET_COMMITMENT` (uint256, required, non-zero) and `GENESIS_PREV_MAX_LEVEL_LAYER_HASH` (uint256, required, zero is legitimate for chain genesis) env vars. The `deployment_real.json` output now includes per-verifier addresses, the two genesis anchors, the `verify_block_wired` boolean and `aave_enabled` boolean. Why this matters: the bridge constructor is the *only* place verifier addresses can be set (intentionally — no admin setter — so an owner-compromise can't swap verifiers). Before this commit, the script unconditionally wired everything to `address(0)`, so any production deploy was a permanently-disabled bridge that had to be redeployed once the genesis BK-set commitment landed; the refreshed script lets the single deploy step go straight to a wired bridge. Locally dry-run-validated against `forge script` in both modes (default: 1.9 M gas; wired: 5.77 M gas) and against the missing-anchor failure path (reverts before broadcast). Circuit 4 (`verifyEvent`) wiring is intentionally not exposed — no gnark-generated `BridgeEventGroth16VerifierGenerated.sol` exists yet (Phase A scaffolding only, Phase B blocked on `docs/circuit_4_open_questions.md`).
    - **Relayer daemon mode (B5)**: New `crates/bridge-relayer-daemon/src/daemon.rs` module + `daemon` CLI subcommand. The existing `Relayer::tick` / `Relayer::run_loop` (bounded `max_ticks` + `should_stop` predicate) are fine for unit tests and one-shot smoke runs but not for an operator running the relayer as a service. Added: (a) `BackoffConfig { initial, max, multiplier }` (default 2 s → 60 s × 2) with `bump()` that doubles on every non-success outcome and caps at `max`; (b) `RelayerMetrics` — atomic `ticks_total` / `verified_total` / `reverted_total` / `not_yet_available_total` / `tick_errors_total` / `current_backoff_secs` / `last_verified_seq_no` / `rotations_detected_total`, returns POD `RelayerMetricsSnapshot` cheap to clone; (c) `Relayer::run_until_shutdown(backoff, metrics, shutdown)` — loops `tick()` forever, resets backoff on `Verified`, bumps it on `NotYetAvailable | BridgeReverted | Err`, and exits cleanly at the next sleep boundary when the caller-supplied `shutdown` future resolves (so a `Verified` is always followed by a flushed `state.json`); (d) same method on `SentryGuardedRelayer` — preserves the existing `GuardedOutcome::PausedAwaitingRotationReconcile` contract (loop keeps logging `Paused` ticks until shutdown; Phase 5.2 will hook the rotation reconcile here); (e) `daemon` CLI subcommand that wires `tokio::signal::ctrl_c()` + Unix `SIGTERM` into the shutdown future, exposes `--backoff-{initial,max}-secs` + `--backoff-multiplier` flags, optionally wraps in `SentryGuardedRelayer` when `--an-node-url` is supplied, and emits a structured `daemon stopped` log with the final metrics snapshot. Phase 5.1 keeps `FixturesBlockSource` as the only `--source`; Phase 5.2 will swap it for `LiveBlockSource`. 5 new unit tests using `#[tokio::test(start_paused = true)]` virtual-time clock: backoff cap, immediate-shutdown returns after one tick, 5-block burst with metrics, exponential growth on consecutive `NotYetAvailable`, backoff reset after `Verified`. Relayer crate test total: 24 → **29**. Clippy + fmt green across the crate; binary `--help` clean.
    - **`verify-fixture` pre-flight subcommand + runbook E.7 (2026-05-20, follow-up to B5)**: Read-only `relayer verify-fixture` CLI subcommand sits between "bridge is deployed" and "fire up the daemon". It connects over HTTP (no private key, no `from`), reads the bridge's three anchors (`storedLastSeenBlockSeqNo`, `storedBkSetCommitment`, `storedPrevMaxLevelLayerHash`), loads the fixture from `FixturesBlockSource::from_dir(...)`, and emits a per-field diagnostic on whether the cheap pre-crypto checks in `verifyBlock` (BK-set match, monotonic seqNo, prev-anchor match) would pass. Exits `0` on full match, `1` with per-field `error!` lines on any mismatch — designed to slot into a pre-deploy shell pipeline. Catches the three operator-side mistakes that show up first in the wild: pointing at the wrong network (anchors all zero or garbage), reusing a fixture against a bridge that already saw it (`BlockSeqNo NOT MONOTONIC: fixture = 1, on-chain last_seen = 1`), or hot-swapping the fixture directory to one generated against a different BK set (`BkSetCommitment MISMATCH`). Subcommand surface: `relayer verify-fixture --fixtures-dir … --rpc-url … --bridge-address …`. New section **E.7** in `docs/manual_verification_runbook.md` documents the expected log shape and the three failure modes; the v2.3 preamble was extended accordingly. No new dependencies (re-uses `EthBridgeClient::read_state` + `FixturesBlockSource::fetch`); 0 new unit tests (the constituent reads are already covered by the relayer's 29-test suite — adding integration tests would require spawning Anvil + a wired bridge in-test, deferred to Phase 5.2 when LiveBlockSource lands).
        - **`dry_run_block` + eth_call simulation (2026-05-20 evening, follow-up to verify-fixture)**: Initially the pre-flight checked only anchor alignment, leaving bad ZK proofs to be caught by the real `daemon` submit (as a `Reverted { reason }` outcome that wastes a transaction). Extended `EthBridgeClient` with a new inherent `async fn dry_run_block(&self, &AnBlockData) -> Result<DryRunOutcome>` method that issues an `eth_call`-based simulation of `verifyBlock(...)` against the current head, returning `DryRunOutcome::WouldSucceed` (every contract check, including the ~287 k-gas Groth16 verifier triple, accepts) or `DryRunOutcome::WouldRevert { reason }` (alloy error chain typically carries the Solidity custom-error selector like `AttestationProofRejected`). Wired into `verify-fixture` as the default behaviour after the cheap checks pass; operators can fall back to the cheap-only path with `--no-simulate` (sub-second, no gas-equivalent compute, useful when they trust the proof generation pipeline). The CLI surfaces a built-in cheat-sheet of common revert selectors on `WouldRevert` so operators can immediately tell whether the issue is bad proofs vs. a stale state read vs. a disabled bridge. No new dependencies; relayer crate test count stays at 29 (simulation needs Anvil + wired bridge, deferred). `DryRunOutcome` re-exported from `lib.rs` for external callers.
    - **Why this matters in aggregate**: The BK-rotation stack is the smallest unblocker for Phase 5.3 (10-block shellnet acceptance) that does *not* require Circuit 3 to exist. The relayer can now safely run against a live AN node — it'll pause itself the moment the committee shifts (instead of submitting stale `bk_set_commitment` to `verifyBlock` and getting a soft revert). When Circuit 3 lands the same pause point becomes the natural seam for "wait for the BK-rotation proof to be available, verify it on chain, then resume". The Alina review pack closes a 10-day-old open item ("send VK + circuit for review") and adds seven Q items that, once answered, directly inform the wire format we lock in for both gnark wrappers and the future `Halo2TvmBundle`. The CI gap fix closes a silent failure mode that has been there since the relayer crate was created.

- **2026-05-21 (Q-C4-5 ceremony terminology alignment + Halo2 TVM pack for Serhii + Phase B `withdrawByProof` scaffolding)**: Three independent workstreams pushed against `main` while waiting on Alina's split-convention ack (Q-C4-1 follow-up) and Serhii's Q-WIRE-1/2/4 acks for the TVM opcode.
    - **Trusted setup ceremony — terminology alignment with Alina (Q-C4-5)**: Earlier reply to Alina ("not critical now") was a problematic shortcut — Alina pushed back with a precise question that surfaced a real terminology drift between us (she uses "ptau" as a synonym for *all* trusted setup, we use it for Phase 1 only). She is 100% correct that **each Halo2 circuit needs its own gnark wrapper R1CS** (because Halo2 VK + public-input schema are hardcoded into the wrapper), therefore **each wrapper needs its own Phase 2 ceremony**, while Phase 1 (PoT) is universal and reused. We aligned on: "Phase 1 / PoT" = universal SRS (reused from Hermez `pot28_final.ptau` or equivalent), "Phase 2" = per-circuit ceremony (5 of them: 1A, 1B, 2, 3, 4). Phase 2 only becomes security-critical *after* Phase 8 (real Halo2-in-gnark verification) — until then wrappers are R15 identity stubs and Phase 2 is harmless single-party. Captured in `docs/an_partner_circuit4_alina_replies_2026-05-21.md` §Q-C4-5 (verbatim follow-up + decoded explanation + reference to integration plan §3 Phase 8 + 9 task breakdown). No code change; this is documentation hygiene + future-Phase-9 logistics enablement.
    - **Halo2 TVM bundle pack for Serhii**: Sergii (AN team) asked how to populate the `bytes vk` and `bytes publicInputs` fields of the new `ZKHALO2VERIFYWITHVK` opcode he's wiring on the TVM side. Shipped `halo2_tvm_for_serhii_2026-05-21.zip` (19 KB; out-of-tree via `/*_for_*.zip` in `.gitignore`) with: (a) `docs/zk_halo2_an_side_design.md` (full design memo §2/§3/§4 with Q-WIRE-1..5); (b) `crates/bridge-prover-orchestrator/src/halo2_tvm_bundle.rs` (producer-side reference implementation of the wire format — 8-byte magic `b"HALO2TVM"`, version byte, transcript_kind discriminator, length-prefixed `(config_json, vk_bytes, instances, proof)` chunks); (c) `crates/bridge-prover-orchestrator/tests/halo2_tvm_bundle_round_trip.rs` (end-to-end prove → serialise → deserialise → SHPLONK verify, currently green against a real Circuit 1B fixture); (d) README with TL;DR on both fields. TL;DR is in the response message: `vk` = `VerifyingKey<G1Affine>::write(SerdeFormat::RawBytes)` (~6.1 KB for K=20, RawBytes is mandatory for soundness because it runs the curve-membership check on every G1 point); `publicInputs` = flat `N × 32` strict little-endian `Fr::to_repr()` (no u64 shortcut — Q-WIRE-3 is closed on the producer side). Open Q-WIRE that still need Serhii's ack: Q-WIRE-1 (transcript flavour — bundle commits to Blake2b, Keccak slot reserved), Q-WIRE-2 (KZG SRS shared globally by `k`, not per-circuit), Q-WIRE-4 (self-describing VK envelope = Option B, already implemented). Once acked the TVM side can wire `ZKHALO2VERIFYWITHVK` against this format as the contract.
    - **`/*_for_*.zip` ignore pattern (housekeeping)**: While shipping the Sergii pack we noticed that the earlier per-partner gitignore patterns (`circuit*_for_*.zip`, `*_for_alina_*.zip`, `*_for_partner_*.zip`) missed `halo2_tvm_for_serhii_2026-05-21.zip`. Collapsed all three into a single root-anchored `/*_for_*.zip` glob that auto-covers any future `<topic>_for_<recipient>_*.zip` bundle dropped at the repo root, so we never have to think about it again. Verified the existing `circuit4_for_alina_2026-05-21.zip` is still ignored after the broadening.
    - **Phase B Circuit 4 v2 `withdrawByProof` scaffolding (B2)**: The original B2 item ("`withdrawByProof` scaffolding — blocked on Alina") unblocked after Alina's 2026-05-21 morning reply pinning the 110-Fr public-input layout (recipient split into 2 Fr, nullifier formula `Poseidon(block_id, tokenId, amount, recipient, senderDapp, senderAcc)`, public `dstChainId`, fixed 20-byte recipient). We have enough to scaffold the full receiver path on ETH side now; the remaining open Q-C4 sub-questions (split convention α/β/γ, `recipient` in Poseidon preimage as 1 vs 2 Fr) don't block the contract surface — only the eventual mock-VK exchange.
      - **New Solidity surface**: `IBridgeWithdrawalGroth16Verifier` (110-input gnark interface), `IBridgeWithdrawalVerifier` (adapter interface with `WithdrawalPublicInputs` struct mirroring slots [0..9] of the gnark vector), `BridgeWithdrawalVerifier` (adapter that assembles the 110-element vector and forwards). All three coexist alongside the Phase A `IBridgeEventVerifier` chain (no breakage) — Phase A retires when v2 circuit lands.
      - **`AckiNackiBridge.withdrawByProof(proof, pub)` method**: new `nonReentrant` entrypoint that (a) verifies the Groth16 proof against the on-chain `_layerWindow` (caller can't substitute a known-good window), (b) checks identity match (`pub.dappFr == bridgeEventDappFr` && `pub.accFr == bridgeEventAccFr`), (c) asserts `pub.dstChainId == block.chainid`, (d) asserts `pub.tokenId == 0` (Phase B is native-ETH-only; non-zero reserved for ERC-20), (e) range-checks `recipientHi`/`recipientLo` (must each fit in 80 bits — split-α default; lockstep-changes with the circuit if Alina picks β/γ), (f) marks `_nullifiers[bytes32(pub.nullifier)] = true` and decrements `treasuryBalance` (CEI: effects before interactions), (g) pulls shortfall from AAVE if liquid ETH < `pub.amount` and `suppliedPrincipal > 0`, (h) `recipient.call{value: amount}("")` — full payout. Emits `WithdrawalByProofExecuted(nullifier, recipient, amount, tokenId, submitter)`. New errors: `WithdrawByProofDisabled`, `WithdrawalProofRejected`, `NullifierAlreadyUsed`, `DstChainIdMismatch`, `RecipientHalfOutOfRange`, `WithdrawIdentityMismatch`, `UnsupportedTokenId`, `WithdrawTransferFailed`, `WithdrawTreasuryShortfall`.
      - **Constructor surface change**: adds 7th argument `BridgeWithdrawConfig _bw` (single field: `bridgeWithdrawalVerifier`). Phase B reuses the immutable `(bridgeEventDappFr, bridgeEventAccFr)` identity slots from `BridgeEventConfig` (both Circuit 4 variants bind to the same AN-side TokenBridge — duplicating the identity in two configs would be a footgun). Constructor enforces "Phase B enabled ⇒ identity slots non-zero". Reverts with the existing `InvalidBridgeEventIdentity` error. New helper `VerifyBlockConfigLib.bridgeEventIdentityOnly(dappFr, accFr)` for deployments that want Phase B without exposing Phase A's `verifyEvent` surface.
      - **18 constructor call sites updated** across 9 files (tests + 2 deploy scripts) with the `VerifyBlockConfigLib.disabledWithdraw()` helper (or `withWithdraw(verifier)` for the new test). Mechanical; no logic change.
      - **20 new Foundry tests** in `AckiNackiBridgeWithdrawByProof.t.sol`: constructor wiring (4), happy-path transfer (1), disabled-bridge revert (1), replay protection (2), identity-mismatch reverts (2), `dstChainId` mismatch (1), unsupported `tokenId` (1), recipient split validation + reconstruction round-trip (4 inc. zero-address + ugly-bits address), treasury shortfall (1), mock crypto rejection + no-state-mutation invariant (1), strict-mode plumbing (2 — proves on-chain `_layerWindow` is forwarded byte-for-byte and that public inputs are forwarded byte-for-byte). 100% pass. New `MockBridgeWithdrawalVerifier.sol` mirrors `MockBridgeEventVerifier.sol`'s strict-mode trick (pin expected `WithdrawalPublicInputs` + pin expected `layerHashes[100]` snapshot).
      - **Foundry total**: 132 → **152** (+20). All 15 unit suites green; fork suite still gated by `FORK_URL`.
      - **What's still pending for production-grade Phase B**: (1) Real `BridgeWithdrawalGroth16VerifierGenerated.sol` from Alina's v2 gnark wrapper — landing requires `circuit-4` setup output; ETH-side just swaps the immutable verifier addr. (2) Recipient split convention ack from Alina (Q-C4-1 follow-up): if she picks β=16/4 or γ=12/8 instead of our default α=10/10, the `RECIPIENT_HALF_MASK` constant + `_reconstructRecipient` math change in lockstep. (3) Real proof e2e test analogous to `LayerHashesMovementVerifierTest`, gated on (1) + (2). (4) ERC-20 wiring for `tokenId != 0` — separate milestone.

- **2026-05-22 (Q-WIRE-1..5 frozen + tvm-sdk skeleton ready for Draft PR)**: Serhii's reply on the `ZKHALO2VERIFYWITHVK` design pack from 2026-05-21 was *"Принимай решения на свой вкус, Сергей готов принять любое наше решение"* — explicit delegation of the wire-format decisions to the bridge team. We treated this as the closing event for Phase A of the AN-side Halo2 verification track and froze all five open Q-WIRE questions. No code or tests touched on the bridge side (the producer-side `Halo2TvmBundle` round-trip in `crates/bridge-prover-orchestrator/` has been green against these exact decisions since 2026-05-21); this entry is documentation hygiene + PR groundwork.
    - **Q-WIRE-1 (transcript flavour) — DECIDED Blake2b.** Bridge already uses `Blake2bWrite` in `crates/bridge-prover-orchestrator/src/prover.rs::generate_fallback_proof`; the `Halo2TvmBundle` `transcript_kind` byte commits to `0x01 = Blake2b` and reserves `0x02 = Keccak` for a future format version. Switching to Keccak would force a duplicate transcript gadget stack on a node that has no EVM-style Keccak-precompile pressure to begin with.
    - **Q-WIRE-2 (KZG SRS) — DECIDED globally shared per `k`.** `Halo2TvmBundle` does **not** carry the SRS. Each TVM node provisions a `kzg_bn254_K.srs` blob per supported `k` (k=19 for DarkDex, k=20 for the bridge fallback) and loads it once at VM startup via a `static SHARED_KZG_PARAMS: Lazy<HashMap<u32, ParamsKZG<Bn256>>>`. The opcode looks up `shared_kzg_params(vk.cs.k())` after deserialising the VK. Per-circuit SRS would mean ~80 MB × N circuits stored on each node — untenable at scale.
    - **Q-WIRE-3 (public inputs) — DECIDED strict 32-byte LE Fr.** New opcode uses `Fr::from_repr(<[u8; 32]>)` directly; ≥ modulus inputs raise `FatalError`. No u64 shortcut. Legacy `ZKHALO2VERIFY` keeps its shortcut for DarkDex backward compat — the two opcodes are intentionally not interchangeable on public-input encoding. Bridge has 160-bit addresses (and 256-bit hashes) which would be structurally ambiguous under the u64-shortcut convention.
    - **Q-WIRE-4 (VK envelope) — DECIDED self-describing `Halo2TvmBundle`.** Already implemented at `crates/bridge-prover-orchestrator/src/halo2_tvm_bundle.rs`. Wire layout: magic `b"H2TVMBND"` (8 B) ‖ `format_version = 0x01` (1 B) ‖ `transcript_kind = 0x01` (1 B) ‖ LE-u32-len-prefixed `(config_json, vk_bytes, instances, proof)`. Empirical sizes on Circuit 1B (k=20, 10 signers): bundle ≈ 21.2 KB. Format version byte bumps on any breaking change.
    - **Q-WIRE-5 (halo2-lib fork pinning) — DECIDED commit SHA + CI gate.** `tvm_vm/Cargo.toml` pins `gosh-sh/halo2-lib-zkevm-sha256-and-bls12-381` to `rev = "<sha>"`. Bridge CI carries a `Halo2TvmBundle` fixture (`crates/bridge-prover-orchestrator/tests/fixtures/halo2_tvm_bundle_v1_circuit1b_k20.bin`) and runs `verify_with_vk` on every SHA bump — a deserialisation failure or flipped verdict is red CI, blocks the bump until the producer side emits a new bundle under a bumped format version. Single mechanical, hard-to-bypass guard against silent on-wire drift in the partner fork.
    - **Q-NAME-1 (opcode name) — DECIDED `ZKHALO2VERIFYWITHVK` @ `0xC7 0x4A`.** Mirrors `VERGRTH16WITHVK`. Compiler builtin: `gosh.zkHalo2VerifyWithVK(proof, pub_inputs, vk_bundle)`. If `serhii/node-3406-vergrth16-with-vk` reshuffles dispatch bytes pre-merge, our follow-up impl PR rebases onto the byte adjacent to the final `ZKHALO2VERIFY`.
    - **Documentation locked in two repos.** Bridge memo `docs/zk_halo2_an_side_design.md` §4 gains a "Status update 2026-05-22" preamble with the frozen-decisions table; each Q-WIRE-N subsection grew a "**Decision 2026-05-22 — DECIDED …**" block capturing the rationale. tvm-sdk doc `docs/zkhalo2verifywithvk_design.md` got a "Decisions (2026-05-22)" section + retitled "Open questions" → "Closed questions" (kept as rationale capture). Bridge memo §5 Phase A/B status updated to "CLOSED".
    - **tvm-sdk skeleton refresh + push (commit `c6576354` on branch `serhii/verhalo2shplonk-skeleton`).** `tvm_vm/src/executor/zk_halo2_with_vk_stub.rs` doc comment on `execute_zkhalo2_verify_with_vk_stub` rewritten — now spells out the frozen wire format for the three stack operands and the SRS-not-on-stack convention (Q-WIRE-2). `FatalError` text rewritten to read "design CLOSED 2026-05-22, wiring blocked on `serhii/node-3406` merge" rather than the previous "design pending Phase A" — accurately reflects that the gating concern is now purely dependency availability, not partner ack. Runtime behaviour unchanged (still throws `FatalError` on call). `cargo check -p tvm_vm --features gosh` green on `tvm-sdk` `main` dep set. Pushed to `origin/serhii/verhalo2shplonk-skeleton`; Draft PR creation deferred (`gh` not installed locally + no GitHub token in env — operator needs to click through GitHub's compare URL with the prepared body, see follow-up message in chat).
    - **Why not rebase onto `serhii/node-3406-vergrth16-with-vk` and write the real impl now**: That branch is 272 commits ahead of `main` and still under review; `git merge-tree` shows conflicts in ≥ 4 files (`tvm_assembler/src/{lib,simple}.rs`, `tvm_vm/src/executor/{engine/handlers,gas/gas_state}.rs`) — the exact hot files our skeleton extends. Maintaining a rebase against a moving 272-commit target while it iterates would burn time that's better spent on Phase 5.2 (LiveBlockSource) and the BK-rotation reconcile path. Real impl is now tracked locally as task `tvm-sdk:zkhalo2vk-real-impl`, to land as a follow-up PR after `node-3406` merges to `main`.
    - **Reproducibility**: the real-impl PR will (a) rebase this skeleton onto `tvm-sdk` `main`, (b) replace the `err!(FatalError, …)` body with `Halo2TvmBundle::decode(...)` → `VerifyingKey::<G1Affine>::read(..., SerdeFormat::RawBytes, &config_params)` → `gosh-zk-snark-halo2-utils::Proof::verify_with_vk(...)`, (c) add the `Lazy<Mutex<LruCache<[u8; 32], CachedHalo2Vk>>>` cache from the design memo §3.3, (d) wire the shared-SRS lookup `shared_kzg_params(k)`, (e) replace the `#[ignore]`d unit test with a real round-trip against the checked-in `Halo2TvmBundle` fixture from `crates/bridge-prover-orchestrator/`. Estimated effort: ~6–10 hours once `node-3406` is on `main`.
    - **Operator next step (when convenient)**: open the Draft PR on `tvmlabs/tvm-sdk` from branch `serhii/verhalo2shplonk-skeleton` against `main`. Compare URL + ready-to-paste PR body are in the chat for the 2026-05-22 working session.

- **2026-05-22 (real impl LANDED on `tvm-sdk` branch `serhii/verhalo2shplonk-real-impl`)**: Same-day pivot at Serhii's explicit direction ("Так, погоди, я вижу, что что-то ещё не готово. Надо немедленно реализовать ream impl plan"). Per that override the deferred follow-up (~6–10 h estimate above) was implemented on the spot.
    - **Branched off `serhii/node-3406-vergrth16-with-vk`** (not `main`) so the `halo2-base` + `gosh-zk-snark-halo2-utils` deps and the existing `KZG_{G0,G2,S_G2}_BYTES` constants are already in scope. Cherry-pick of the skeleton commits onto that base resolves cleanly after manual merges in `tvm_assembler/src/{lib,simple}.rs`, `tvm_vm/src/executor/{engine/handlers,gas/gas_state}.rs`.
    - **Real-impl deltas vs skeleton**:
        - `tvm_vm/src/executor/zk_halo2_with_vk.rs` — full handler. Stack ABI **collapsed from 3 operands to 1**: a single `bundle_cell` carrying the whole `Halo2TvmBundle` (matches what the producer in `crates/bridge-prover-orchestrator/src/halo2_tvm_bundle.rs` already round-trip-tests). `VerifyingKey<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(..., SerdeFormat::RawBytes, config)` — DOES the curve-membership check (soundness-critical for caller-supplied VKs; `gosh-zk-snark-halo2-utils::io::read_vk` uses `RawBytesUnchecked` and is **deliberately not used**). Bounded-FIFO per-VK cache keyed by `vk_bytes`, capacity 8. `build_shared_kzg_params(k)` reuses `KZG_{G0,G2,S_G2}_BYTES` from `zk_halo2_utils.rs` (promoted to `pub(crate)`), parameterised by `k` — Q-WIRE-2 simplified: **no on-disk SRS file needed**, the three embedded points cover any `k` for SHPLONK verification. Strict-LE `Fr::from_repr` instance decoding; structural rejection of ≥-modulus chunks as `FatalError`.
        - `tvm_vm/src/executor/zk_halo2_with_vk_bundle.rs` (new) — pure wire-format decoder. 8 unit tests covering magic / version / transcript / chunk-length / trailing-garbage / multiple-of-32 / oversized-bundle DoS guard (16 MiB cap).
        - `tvm_vm/src/tests/test_halo2_with_vk.rs` (new) — real round-trip against the checked-in DarkDex W=8 L0 fixture (`halo2_test_data/dark_dex_w8_L0_*.bin` + `DARK_DEX_W8_VK_BYTES`). 5 tests: positive (~0.6 s warm); flipped proof byte rejected; tweaked instance Fr rejected; bad magic → FatalError; LRU smoke-test across two sequential calls.
        - `tvm_assembler/src/{simple,lib}.rs` — `ZKHALO2VERIFYWITHVK` → `0xC7 0x4A`; `gosh_zk_opcode_tests::zk_opcode_bytes_round_trip` table-driven now covers VERGRTH16 / POSEIDONZKLOGIN / ZKHALO2VERIFY / ZKHALO2VERIFYWITHVK / POSEIDON / VERGRTH16WITHVK.
        - `tvm_vm/src/executor/engine/handlers.rs` + `gas/gas_state.rs` — dispatch + gas wiring. Placeholder `ZKHALO2_VERIFY_WITH_VK_GAS_PRICE = 5_000`; re-benchmark TODO captured in the design memo §7.
        - `docs/zkhalo2verifywithvk_design.md` (tvm-sdk side) — full rewrite. Documents the frozen wire format with corrected magic `b"HALO2TVM"` (the skeleton had a typo `b"H2TVMBND"` — bridge memo + AGENTS.md updated to match), 1-operand stack ABI, `transcript_kind=0x00` for Blake2b, runtime-built KZG params via three embedded points, safety rationale for `SerdeFormat::RawBytes`, per-VK cache contract, gas re-bench TODO list.
        - Bridge-side companion edits in this commit: `docs/zk_halo2_an_side_design.md` Q-WIRE-2 entry rewritten ("runtime-built from 3 embedded points" instead of "on-disk SRS file per k"), magic corrected to `b"HALO2TVM"`, transcript_kind discriminator aligned to `0x00`; `AGENTS.md` § for the design memo updated to point at the new `verhalo2shplonk-real-impl` branch and reflect the 1-operand ABI + magic.
    - **Drive-by buildability fixes** (`serhii/node-3406-vergrth16-with-vk` does **not** compile with `--features gosh` as-is — three pre-existing breaks):
        1. Stray identifier `full_dex_test_with_final_halo2_circuit` (a branch name accidentally pasted into source) inside `consume_chkhistproof` in `gas_state.rs`. Removed.
        2. `execute_poseidon` referenced at dispatch byte `0xC7 0x50` but the function was dropped during the merge from `full_dex_test_with_final_halo2_circuit`. Restored verbatim from `origin/main`.
        3. `IntegerData::from_unsigned_bytes_le` dropped during the same merge. Restored verbatim from `origin/main`. Needed by `execute_poseidon`.
       These three fixes are intentionally surgical and easy to drop into Serhii's branch before merge if preferred. They are flagged in §6 of the real-impl PR's design memo and in the PR description.
    - **CI status**: `cargo check -p tvm_vm` green (no-features default); `cargo check -p tvm_vm --features gosh` green; `cargo test -p tvm_vm --features gosh --lib zk_halo2_with_vk` → 8/8 (bundle decoder unit tests); `cargo test -p tvm_vm --features gosh --lib test_halo2_with_vk` → 5/5 (round-trip + negatives, ~0.57 s); `cargo test -p tvm_assembler --features gosh --lib gosh_zk_opcode_tests` → 1/1 (assembler dispatch round-trip).
    - **Commit**: `461070de` on branch `serhii/verhalo2shplonk-real-impl` (pushed to `origin/serhii/verhalo2shplonk-real-impl` 2026-05-22). Replaces `serhii/verhalo2shplonk-skeleton` as the live tvm-sdk side branch; the skeleton branch is now redundant and can be deleted post-merge.
    - **Cooperation model (going forward)**: the real-impl PR rebases cleanly onto `serhii/node-3406-vergrth16-with-vk`; once that lands on `main` the real-impl PR rebases onto `main` as a single follow-up commit. Producer-side companion still to land in `Pruvendo/acki-nacki-bridge`: `TVM-Solidity-Compiler` builtin `gosh.zkHalo2VerifyWithVK(bundle_cell)` and the AN-side `TokenBridge.finalizeDeposit(...)` Solidity contract.

- **2026-05-22 (later — `VERGRTH16WITHVK` retired; CI follow-up to `tvm-sdk` PR #240)**: PR #240 (the real `ZKHALO2VERIFYWITHVK` impl) was merged into `serhii/node-3406-vergrth16-with-vk` and triggered two red CI checks on the merge commit: (a) `Lint` (`cargo +nightly fmt --all -- --check`) failed on ~40 hunks accumulated across the `node-3406` and `verhalo2shplonk-real-impl` merges; (b) `Test / Priority Units` failed on `vergrth16_with_vk_accepts_valid_proof` — a pre-existing failure on Serhii's branch where the test's hand-assembled opcode byte string is `0xC7 0x49 0x80` even though the current dispatch table puts `VERGRTH16WITHVK` at `0xC7 0x52` (the test was actually invoking `ZKHALO2VERIFY` with `VERGRTH16WITHVK`-shaped operands and failing the verification). Same-day decision (Alina): **retire `VERGRTH16WITHVK` rather than fix the test byte**, because the Phase 4.3 demolition (2026-05-17) already eliminated the need for a per-VK Groth16 verifier on the AN side — native Halo2 SHPLONK via `ZKHALO2VERIFYWITHVK` is the path forward and the Groth16 verifier is unreferenced dead code. Concrete removals (PR #242 on branch `serhii/remove-vergrth16-with-vk`, base `serhii/node-3406-vergrth16-with-vk`): `executor::zk::execute_vergrth16_with_vk` handler, `VERGRTH16_WITH_VK_GAS_PRICE` constant, `Gas::{vergrth16_with_vk_price, consume_vergrth16_with_vk}`, dispatch entry `0xC7 0x52`, `tvm_assembler` mnemonic + round-trip table entry, the entire `tvm_vm/src/tests/test_vergrth16_with_vk.rs` test module, and all live cross-references in `tvm-sdk/docs/zkhalo2verifywithvk_design.md`. Drive-by: `cargo +nightly fmt --all` pass cleans the accumulated fmt drift. Bridge-side companion edits (this commit): `docs/zk_halo2_an_side_design.md` updated to remove the "mirrors VERGRTH16WITHVK" framing (it's now the only Halo2 verification opcode that takes a caller VK, not a parallel to anything) and to point at PR #240 + PR #242; `docs/audit_trail_v2.md` K-13 updated to reflect LANDED status and the correct opcode name; `docs/verifying_eth_proof_on_an.md` opcode-status table updated to reflect `ZKHALO2VERIFYWITHVK` at `0xC7 0x4A` (was tracking a stale working name `VERHALO2SHPLONK`). **PR #242 CI status**: Lint ✅ 20 s, Test / Priority Units ✅ 6 m 53 s, Test / tvm_client ✅ 6 m 10 s — fully green. Diff: 18 files, +141 / −490 (most of the +141 is fmt-rewrap; the behaviourally-meaningful change is purely subtractive). Freed dispatch byte `0xC7 0x52` is left unallocated.

- **(pending)**: Q6, Q7, Q8 — partner SLA in flight.

### Phase status after these answers

| Phase | Pre-answer status | Post-answer status (after Q1, Q2, Q3, Q4, Q5) |
|---|---|---|
| 0 | All 8 questions open | 5/8 substantively answered (Q1, Q2, Q3 partial, Q4 promised, Q5); Q6, Q7, Q8 still open |
| **1.A — Circuit 1B wiring** | (n/a) | ✅ **Done 2026-05-06**. New `crates/bridge-prover-orchestrator/` crate wraps `FallbackAttestationBlsCheckerCircuit` with `FallbackKeyManager` + `generate_fallback_proof` + `verify_fallback_proof`. Round-trip integration test (`tests/fallback_round_trip.rs`) green: keygen 218 s (VK 140 s, PK 78 s), proof gen 11 s @ 10-signer set, verification + tamper-rejection + wrong-instance-rejection all pass. Final proof = 14 784 bytes; PK = 6.5 GB, VK = 6 KB on disk under `params/`. |
| **1.B — Circuit 2 wiring** | (n/a) | ✅ **Done 2026-05-10**. `LayerHashesKeyManager` + `generate_layer_hashes_proof` + `verify_layer_hashes_proof` in `crates/bridge-prover-orchestrator/`; round-trip test green (proof = 6 112 B; PK = 339 MB; keygen ~10 s total). 14 public inputs `[block_id, bk_set_poseidon, num_layers, layer_hash[0..10], prev_max_level_layer_hash]`. |
| **1.C — Circuit 3 wiring** | (n/a) | ⏸ **Deferred** (partner-driven). Schedule after Alina's Circuit 3 signal. |
| 2 — Layer-hash pipeline | Blocked on Q1 + Q5 + Q7 | Stays blocked until `latest_an_to_eth_bridge_test` has the new envelope code + a node we can target. **Q7 still required.** |
| 3 — Solidity verifiers | Independent of partner | 🟢 In progress: 3.1 (Circuit 1B), 3.2 (Circuit 1A), and 3.3 (Circuit 2) live, **26 Foundry tests** green (8 + 8 + 10); 3.4 (Circuit 3) waits on Phase 1.C |
| 4 — Contract rebuild | Independent of partner | ✅ **Done** (4.1 2026-05-10 additive `verifyBlock`; 4.2 2026-05-10 demolition of legacy `LayerHashBridge.sol` + 8 sibling contracts + 49 tests + `layer-hashes-prover/` + `bk-set-rotation-prover/`). `AckiNackiBridge.verifyBlock(finType, attestationProof, lhProof, …)` is the only AN→ETH state-transition surface now; per-circuit `PrimaryVerifier`/`FallbackVerifier`/`LayerHashesMovementVerifier` adapters carry the proof-shape coverage that used to live in `LayerHashVerifierTest`. Foundry suite: 135/135 across 15 suites. Phase 1.C will plug a fourth optional `bkSetUpdateProof` argument into `verifyBlock`. |
| 5 — Relayer | Blocked on Q1 + Q2 | 🟢 **5.1 done 2026-05-10** (skeleton, no partner dep). New `crates/bridge-relayer-daemon/` (`Relayer::tick()`/`run_loop()` + `BlockSource`/`BridgeClient` traits + `EthBridgeClient` over abigen + `MockBridgeClient`/`InMemoryBlockSource`/`FixturesBlockSource` for tests + atomic `state.json` + CLI binary). 13 Rust unit tests + 6 Foundry tests in `AckiNackiBridgeRelayerLoop.t.sol` (10-block on-chain drive). 5.2 (`LiveBlockSource` over GraphQL + BOC + orchestrator) blocked on Q1 + Q2; relayer should **recompute `transition_hashes` locally**. 5.3 (10-block shellnet acceptance) gated on Q1. |
| 6 — Real interface | Blocked on Phase 5 | Blocked |
| 7 — Docs / release | Final | Final |

**Re-sequenced Phase 1**: now three sub-phases, in this dependency order:
- **1.A — Circuit 1A + 1B prover/verifier wiring** — ✅ **Complete (1B 2026-05-06; 1A 2026-05-07; refreshed for `block_id` rename 2026-05-10)**. Public-instance order locked at `[block_id, bk_set_poseidon, block_seq_no, last_seen_block_seqno]` — matches the partner's renamed `expose_public` sequence (was `envelope_hash` before 2026-05-10, see Decision Log). Both `FallbackKeyManager` and the partner's `KeyManager` (primary) used by us are in `crates/bridge-prover-orchestrator/`.
- **1.B — Circuit 2 (Layer Hashes Movement) wiring** — ✅ **Complete (2026-05-10)**. `LayerHashesKeyManager` + `generate_layer_hashes_proof` + `verify_layer_hashes_proof` + synthetic test-data builder + round-trip test all green. K=17, proof size 6 112 B, PK 339 MB.
- **1.C — Circuit 3 (BK-set update) prover/verifier wiring** — wait for partner signal.

- **2026-05-27 (Phase 4.5.2 — R15 M2 feasibility spike closed: snark-verifier aggregator + Yul EVM verifier validated end-to-end)**:
  - **Deliverable**: new standalone cargo workspace `crates/bridge-evm-aggregator/` with `src/multiply.rs` (inner circuit `a*b==c`), `src/aggregator.rs` (snark-verifier-sdk wiring), `tests/round_trip.rs` (`#[ignore]`-gated acceptance test), and `README.md`. Snark-verifier-sdk v0.1.7-git resolves cleanly against axiom-crypto/halo2-lib v0.4.1-git + PSE halo2 v2023_04_20 in our env on first try.
  - **Empirical results**: aggregator at K=21 SHPLONK produces a Yul EVM verifier of **13 009 bytes (12.7 KB)** — 53 % of the EIP-170 24 576 byte runtime limit. Healthy headroom for Circuit 4 to occupy more PIs / lookups. Solidity source (1 192 lines, 56 055 bytes) compiles cleanly under our Foundry profile (`solc 0.8.19`, `via_ir = true`, `optimizer_runs = 1`).
  - **Two corrections to the M1 roadmap**, captured in `docs/r15_snark_verifier_roadmap.md` M2 section: (a) accumulator is **12 limbs (4 EC coords × 3 native limbs)**, not 4; earlier comments in `deposit-prover/src/aggregation.rs` were wrong. (b) K=20 hits `NOT ENOUGH ADVICE COLUMNS` once we re-expose inner instances — K=21 is the safe floor.
  - **Two items deferred from M2 to M5/M6**: (a) `expose_previous_instances(false)` (re-expose inner SNARK PIs as outer instances) — works at the Rust API level but currently overruns the auto-tuned `num_advice_per_phase`; needs explicit `AggregationConfigParams` tuning, which depends on Circuit 4's actual witness layout (M5). (b) Foundry on-chain harness — deferred to M6/M7 because solc + `via_ir = true` strips the inline-assembly fallback to a 67-byte stub (same problem the legacy `Halo2Verifier.sol` in `contracts/ethereum/src/` has — its bytecode in `out/` is also 67 bytes and the established workaround is `vm.readFileBinary` on a `.bin` file from snark-verifier's bytecode output, which we now save next to the .sol).
  - **One plan deviation**: M2 originally proposed aggregating Circuit 1B (partner fork) as the inner SNARK to also validate cross-fork compatibility. Abandoned because Circuit 1B is the DEPOSIT direction (consumed by AN-side `ZKHALO2VERIFYWITHVK`, NOT by an ETH-side aggregator); the only ETH-side aggregator target is Circuit 4 (withdraw direction). M2 with a synthetic inner is the cleanest scope; cross-fork compat is M5's problem when partner's Circuit 4 lands.
  - **Run instructions**: `cd crates/bridge-evm-aggregator && CARGO_NET_GIT_FETCH_WITH_CLI=true cargo +nightly test --release --test round_trip -- --ignored --nocapture` (~3 min wall-clock). All artefacts (`target/spike/`, `params/`, `Cargo.lock`) `.gitignore`d.
- **2026-05-27 (Phase 4.5.3 — R15 / M3 transcript strategy: native Poseidon transcript landed in orchestrator)**:
  - **What landed.** A native Poseidon-128 Fiat–Shamir transcript at `crates/bridge-prover-orchestrator/src/poseidon_transcript.rs` (`PoseidonRead<R>` / `PoseidonWrite<W>` implementing `halo2_proofs::transcript::{Transcript, TranscriptRead/Write, TranscriptReadBuffer/WriterBuffer}`), parameterised over `OptimizedPoseidonSpec<Fr, 3, 2>` with `R_F=8`, `R_P=57`, `SECURE_MDS=0` — the exact spec snark-verifier-sdk uses (`snark-verifier-sdk/src/halo2.rs` v0.1.7-git lines 54–58). `TranscriptKind::Poseidon = 2` joins the existing `Blake2b = 0` discriminator in `halo2_tvm_bundle::TranscriptKind`. The orchestrator's `generate_fallback_proof` / `verify_fallback_proof` now have `_with_transcript` companions that dispatch on `TranscriptKind`; legacy callers stay on Blake2b by default. AN-side `ZKHALO2VERIFYWITHVK` rejects Poseidon (the existing `VkBlob::verify` assertion suffices — no opcode change needed).
  - **Vendoring vs depending on `snark-verifier`.** `snark-verifier`'s `Cargo.toml` lists `halo2-base` (axiom-crypto fork) as a non-optional dependency (no feature flag turns it off). The orchestrator pins `halo2-base` to the gosh fork — Cargo refuses two copies of `halo2-base` in one cargo unit. Resolution: copied the ~80 lines of native permutation logic from `snark-verifier/src/util/hash/poseidon.rs` into our own module, parameterised over `OptimizedPoseidonSpec` which IS exposed by the gosh fork at the same module path (both forks derive from PSE upstream verbatim). The spec generator is deterministic over `(T, RATE, R_F, R_P, SECURE_MDS, Fr)`, so gosh-fork and axiom-fork builds compute byte-identical round constants and MDS matrices.
  - **Tests.** Five fast unit tests (`cargo test --lib poseidon_transcript`, <1 s) cover: deterministic empty squeeze, scalar write/read round-trip, EC-point write/read round-trip, tampered-byte detection (challenge diverges on bit flip), spec generation determinism. Plus the heavy `fallback_round_trip` integration test extended to prove + verify with BOTH transcripts on a 10-signer fixture, asserting (1) Poseidon proof verifies under Poseidon-Read, (2) Blake2b/Poseidon cross-verification fails, (3) the two byte streams differ. All green.
  - **What this unblocks.** M5 (Circuit 4 aggregator). Once partner's Circuit 4 lands (M4, partner-blocked), the orchestrator will produce its proof with `TranscriptKind::Poseidon`, dump it as `(proof.bin, instances.bin, vk.bin)` via the existing exporter, and `crates/bridge-evm-aggregator/` will read those three files into a `snark-verifier-sdk::Snark` and feed them into an `AggregationCircuit`. Byte-for-byte transcript compatibility with `snark-verifier-sdk::PoseidonTranscript<NativeLoader, _>` will be confirmed there for the first time — if the aggregator accepts our proof, the transcripts agree; if not, M5 will surface the discrepancy and we'll add a golden-vector cross-test.
  - **What we deliberately deferred.** (a) The stretch sub-goal "one Halo2 proof, two transcripts simultaneously" — superseded by the cleaner two-flavour model; each consumer (AN opcode vs ETH aggregator) gets its own proof, prover takes a discriminator at the API. (b) Cross-implementation golden test against `snark-verifier-sdk` as a standalone artefact — folded into M5's round-trip.
- **2026-05-26 (Phase 4.5 — R15 strategy locked: snark-verifier aggregator path, M1 roadmap committed)**:
  - **What R15 actually is.** The on-chain `BridgeWithdrawalGroth16Verifier` (and the sibling Groth16 stubs for circuits 1a/1b/2 under `crates/bridge-prover-orchestrator/gnark-wrappers/`) currently have `Define()` bodies that only assert `for i { api.AssertIsEqual(PI[i], PI[i]) }` — they do **not** verify the underlying Halo2 SHPLONK proof. Any 10 numbers can pass as "valid" public inputs and the wrapper will happily produce a Groth16 proof of them. We confirmed this by reading the deleted historical wrappers (`deposit-prover/gnark-wrapper/circuit.go` @ `09b8842^`, `layer-hashes-prover/gnark-wrapper/circuit.go` @ `5468519^`, `bk-set-rotation-prover/gnark-wrapper/circuit.go` @ `5468519^`); all three had verbose scaffolding ("Week 1–4 ✓ Week 5 (current)" header on deposit) but identical identity-only bodies. **A real Halo2-in-gnark verifier has never existed in this repository.**
  - **Decision: path 2 from the 2026-05-26 chat = snark-verifier aggregator + Yul EVM verifier.** Verify the inner Halo2 SHPLONK proof inside another Halo2 circuit using `axiom-crypto/snark-verifier-sdk` (which we already use successfully in `deposit-prover/src/aggregation.rs`), then emit a Yul Solidity verifier via `gen_evm_verifier_shplonk`. Rejected alternatives: (1) hand-written Halo2 SHPLONK verifier in gnark `sw_bn254` — ≥2 months, re-invents the wheel; (3) drop Groth16 entirely and use the existing `Halo2Verifier.sol` Yul verifiers directly — 2–3 M gas, no aggregation upside. Path 2 is well-trodden (Axiom, Scroll, Taiko run this in prod) and reuses existing snark-verifier integration in `deposit-prover/`.
  - **Constraints surfaced.** (C-1 transcript) `bridge-prover-orchestrator/src/prover.rs` and partner's `bridge-prover-lib` hardcode `Blake2bWrite`; snark-verifier-sdk's in-circuit verifier expects Poseidon. Resolution: produce a Poseidon-transcript variant of the Circuit 4 prover for the ETH side; AN-side `ZKHALO2VERIFYWITHVK` (deposit direction) keeps Blake2b. (C-2 halo2-fork conflict) `snark-verifier-sdk` builds against `axiom-crypto/halo2-lib`; partner circuits build against the gosh fork. Resolution: aggregator lives in a separate cargo workspace under `crates/bridge-evm-aggregator/` (excluded from main workspace like `deposit-prover/`), interfacing with orchestrator over serialised `(VK, proof_bytes, instances)`. (C-3 EVM size) `gen_evm_verifier_shplonk` empirically emits ~10–15 KB at `k=21`, comfortably under EIP-170 24 576 byte limit. (C-4 circuit availability) Circuit 4 not yet in our orchestrator; partner branch `circuit4-single-final-root` finalising. **M2 spike therefore aggregates Circuit 1B as a stand-in** until M4 unblocks.
  - **Milestones (M1 → M8).** M1 (this entry + `docs/r15_snark_verifier_roadmap.md`): roadmap committed. M2: feasibility spike via standalone `crates/bridge-evm-aggregator/` crate aggregating Circuit 1B and emitting Yul. M3: transcript-strategy implementation (parameterise `generate_*_proof` over Blake2b vs Poseidon transcript). M4: Circuit 4 prover in orchestrator (partner-blocked). M5: real Circuit 4 aggregator. M6: Yul verifier under 24 KB → `contracts/ethereum/src/BridgeWithdrawalAggregatorVerifier.sol`. M7: Foundry E2E with real aggregated proof verified on-chain (this is the actual R15 close). M8: remove the gnark identity stubs. Estimated effort: 15–25 working days net, 4–6 wall-clock weeks once M4 unblocks. See `docs/r15_snark_verifier_roadmap.md` for the full breakdown.
  - **What lands in this commit.** M1 only: the roadmap doc + this Decision Log entry + AGENTS.md cross-reference update (`Pending Phase 8 (real Halo2-in-gnark)` → `Pending R15 milestones M2–M7 (snark-verifier aggregator path)`). No code changes; M2 starts in a follow-up commit.
- **2026-05-26 (Phase 4.4 — Circuit 4 ABI sync to partner's single-final-root v3 + emergency pause + LiveBlockSource scaffolding)**:
  - **Circuit 4 ABI**. Partner shipped the unified single-final-root circuit on branch `circuit4-single-final-root` (`gosh-sh/acki-nacki-to-eth-bridge-halo2-circuits`) — 10 public inputs `[tokenId, amount, recipientHi, recipientLo, dstChainId, senderAccFr, dappFr, accFr, nullifier, finalRoot]`, replacing the legacy 103-PI Phase A `verifyEvent` (`bridge-event-prove-circuit` v1) and the 110-PI Phase B `withdrawByProof` (v2). Retired Phase A entirely: deleted `IBridgeEventVerifier.sol`, `IBridgeEventGroth16Verifier.sol`, `BridgeEventVerifier.sol`, `MockBridgeEventVerifier.sol`, `AckiNackiBridgeVerifyEvent.t.sol`. Updated `IBridgeWithdrawalVerifier.sol` + `IBridgeWithdrawalGroth16Verifier.sol` + `BridgeWithdrawalVerifier.sol` to the 10-PI layout (drop `senderDappFr` — TVM `MsgAddrStd` has no dApp-id; drop `layerHashes[100]` calldata — replaced by single `finalRoot` PI + off-circuit anchor check). Refactored `AckiNackiBridge.sol`: replaced the rolling `_layerWindow[LAYER_WINDOW_SIZE=100]` ring buffer with a flat `mapping(uint256 => bool) _knownAnchors` populated on every `verifyBlock` and consulted in `withdrawByProof`; new `UnknownAnchor(finalRoot)` revert path; `_pushLayerWindow → _recordAnchor`, `LayerWindowPushed → AnchorRecorded`, `layerWindowHead → anchorsRecorded` (monotonic counter only, no slot semantics). Merged `BridgeEventConfig` (identity-only) into `BridgeWithdrawConfig({bridgeWithdrawalVerifier, dappFr, accFr})` — constructor surface shrinks from 3 configs to 2. Gnark wrapper `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-4/` rewired for 10 PI (`BridgeWithdrawalVerifierCircuit`); still R15 identity-stub — real Halo2-in-gnark verification gated on Phase 8.
  - **Emergency pause**. New owner-only `pause()` / `unpause()` on `AckiNackiBridge.sol` with `Paused(by)` / `Unpaused(by)` events and `BridgePaused()` / `AlreadyInThatPauseState()` reverts. `whenNotPaused` modifier on `deposit`, `verifyBlock`, `withdrawByProof`. Owner-only AAVE management (`emergencyWithdrawAll`, `withdrawFromAave`, `harvestYield`, `transferOwnership`) intentionally **not** gated — owner must be able to evacuate funds during an incident even while the user surface is halted.
  - **LiveBlockSource scaffolding** (Phase 5.2 prep). New `crates/bridge-relayer-daemon/src/live_source.rs`: split the live path into two narrow async traits — `RawBlockProvider::fetch_raw(seq) → RawBlockWitness` (cheap; HTTP/GraphQL against AN node) and `BoundProofGenerator::prove(raw) → BoundProofArtifacts` (expensive; Halo2+gnark; minutes). `LiveBlockSource<P, G>` composes both behind `BlockSource::fetch`. Ships `InMemoryRawBlockProvider` + `StubBoundProofGenerator` for unit tests (4 new tests, all green). Real `RawBlockProvider` wraps partner's `circuit-data-exporter` (sibling repo `acki-nacki`, branch `bridge_halo2_tests`) once public GraphQL exposure lands; real `BoundProofGenerator` is the Phase 6 `bridge-prover-daemon` over IPC.
  - **Test counts**. Foundry: **152/152 across 15 suites** (was 152 with VerifyEvent + 14 suites pre-refactor; +12 new pause tests, −22 obsoleted VerifyEvent tests, +4 new anchor/PI tests in WithdrawByProof = 152). Relayer Rust: +4 live_source tests. All green.
  - **What this closes** from the 2026-05-26 cross-cutting audit: #2 (Circuit 4 ABI sync), #10 (LiveBlockSource scaffold), #11 (anchor-map design — flat `_knownAnchors` set is the correct on-chain check for single-final-root), #14 (pause / emergency stop). #13 (withdrawal counter): ETH-side is correct; the missing `_withdrawalId` is partner/AN-side. **Still open**: R15 (Phase 8, big R&D), Circuit 4 / TokenBridge re-wiring (partner), AN node `finalizeDeposit` un-sterilising (partner), full E2E pipeline test (blocked on partner).
- **2026-05-10 — Phase 4.4 placeholder (now retitled to Phase 4.5 below — kept for cross-ref)**: ~~the originally-numbered "Phase 4.4" entry was the Circuit 4 v1/v2 attestation scaffolding (deleted 2026-05-26). Cross-references in test names mentioning "Phase A / Phase B" or `_layerWindow` are obsolete after 2026-05-26.~~

Phases 3.1, 3.2, 3.3 are now live (Circuits 1B + 1A + 2); only Phase 3.4 (BK-set update) remains and it's gated on Phase 1.C. **Phases 4.1 + 4.2 + 5.1 + 7.1–7.8 all landed 2026-05-10**: additive `verifyBlock` (cross-circuit consistency + chain-anchor invariants), legacy `LayerHashBridge` + 8 sibling contracts + 49 tests + `layer-hashes-prover/` + `bk-set-rotation-prover/` removed, relayer skeleton (`crates/bridge-relayer-daemon/` with `Relayer::tick()`/`run_loop()` + `BlockSource`/`BridgeClient` traits + production `EthBridgeClient` over abigen + in-memory mocks + atomic `state.json` + CLI), v2 doc set (canonical `four_circuit_architecture.md` + `audit_trail_v2.md` + rewrites of `integration_analysis.md` §3/§5, `bridge_verification.md` §5/§6/§6.5/§8/§9/§10/§11/§13, `manual_verification_runbook.md` Phase D/F/G/J Group4-5/K, `verifying_an_proof.md` per-circuit V1–V5, plus banner updates on `verifying_eth_proof_on_an.md` and `integration_plan.md`; legacy v1 walkthrough preserved at `docs/legacy/verifying_an_proof_v1.md`). Final state at end-of-day: **135/135 Foundry tests across 15 suites green** + **13/13 Rust relayer unit tests green** + zero references to the retired architecture in code. The natural next moves are: (a) **Phase 5.2** (`LiveBlockSource` over partner's `gql_client` + `boc_parser` + our `bridge-prover-orchestrator` for halo2+gnark wrapping inside the relayer); blocked on Q1 + Q2. (b) **Phase 5.3** (10-block shellnet end-to-end against Anvil — the §5 acceptance criterion; reintroduces real-proof multi-block coverage that retired with the legacy E2E suite in Phase 4.2). (c) **Phase 1.C** (Circuit 3 wiring + Phase 3.4 Solidity verifier + extending `verifyBlock` with an optional `bkSetUpdateProof` argument), once Alina signals. (d) **Phase 7.9** — release-manager step: tag `v2.0.0-rc1` once 5.2/5.3/1.C blockers clear or testnet launch is approved.

---

## 8. Open Questions for the Partner

State as of 2026-05-11 after the W=4 end-to-end confirmation on the GOSH stand. Closed and moot items dropped; only live blockers remain.

### Q1 — Circuit 1B + Circuit 3: green commit SHAs

We need a SHA on each circuit repo where `MockProver` (and a real-prover sanity test, if cheap) passes for fallback attestation (1B) and BK-set rotation (3). Without this we cannot start Phase 1.C (Circuit 3 wiring, Phase 3.4 Solidity verifier, optional `bkSetUpdateProof` argument on `verifyBlock`). Phase 5.2 can proceed without 1B for a Primary-only first cut.

### Q2 — `transition_hashes` migration in the AN node

Has `BlockKeeperSetChangeProofData.transition_hashes` been migrated to the new minimal-Poseidon format described in §8 of `ENVELOPE_HASH_MERKLE_SPEC.md`, or does the relayer own that computation? Plan B (relayer recomputes locally from BK-set deltas) is cheap and unblocks us; we just need a "yes" or "no" so Phase 1.C public-input layout is finalised.

### Q3 — Full BK-set Poseidon test vector

One fully-populated input → output sample: a BK set sorted by `signer_index`, padded to 300 entries, expanded to 1800 Fr inputs, and the resulting 32-byte Poseidon output produced by `bridge-prover-lib::poseidon`. We already observed one commitment value (`0x11e20085fcc3d8d494ba4037e884a2b55909498ff27d4595949a9674f52dd495` on the 5-signer lightweight stand) but not the precise input encoding behind it. Needed to write an independent Poseidon implementation in `bridge-prover-orchestrator` without coupling to the partner's crate as a runtime oracle.

### Q4 — `DenseChainLink` siblings: GraphQL or client reconstruct?

Confirm whether `Vec<DenseChainLink>` siblings for a block's layer-hash chain must be reconstructed client-side (as `real_chain_builder` does today, from raw `history_proofs[layer]` data) or whether a GraphQL field exposes the precomputed sibling vectors directly. Affects Phase 5.2 design — reconstruction adds ~200 LOC of tree code to `LiveBlockSource` that we'd otherwise skip.

### Q5 — Hermetic E2E fixture set: wanted?

The 4 historical fixtures (`L2_H16_prevH0_S1` et al.) targeted the retired 13-input `LayerHashBridge` layout and went away in Phase 4.2. Should we regenerate against the new bound-proof format so Phase 5.3's 10-block acceptance test has an offline fallback when no live shellnet is available? Nice-to-have, not blocking.

---

### P-1 (proactive, partner-notify, 2026-05-17) — current Pruvendo gnark wrappers are stubs

> **OBSOLETED 2026-05-17 (same-day, by Phase 4.3 demolition).** Kept
> verbatim below as the audit-trail record of what we told Alina, but
> it is no longer actionable: Phase 4.3 (Decision Log 2026-05-17)
> retired the entire ETH-side Groth16 deposit-verification chain
> (`IAckiNackiVerifier`, `Groth16{,Deposit}Verifier.sol`,
> `deposit-prover/gnark-wrapper/`). The ETH→AN deposit proof now
> reaches the AN side as a **native Halo2 SHPLONK** payload that is
> verified by `ZKHALO2VERIFYWITHVK` (wire format
> frozen 2026-05-22, see Decision Log entry). There is no longer any
> path where `gosh.vergrth16WithVK` participates in the bridge's
> deposit flow — neither in PR 2112's `TokenBridge.finalizeDeposit`
> (which Alina dropped the commented-out call from), nor anywhere
> downstream. The "we'll re-handoff once Phase 8 closes" promise also
> falls away: the deposit path no longer needs a gnark-wrapper at all.
> R15 / Phase 8 stay relevant only for the **AN→ETH direction**
> (Circuit 1A/1B/2/3/4 wrappers) where Groth16 is still the
> cryptographically-required envelope; that work is tracked
> independently in §3 Phase 8 and §5 R15.
>
> See: Decision Log 2026-05-17 (Phase 4.3 demolition), Decision
> Log 2026-05-22 (Q-WIRE-1..5 freeze), `docs/zk_halo2_an_side_design.md`,
> `docs/verifying_eth_proof_on_an.md`.

Not a question for the partner — a heads-up. While preparing a `verification.key` + `circuit.go` handoff for Alina (so AN-side could wire `gosh.vergrth16WithVK` into `TokenBridge.finalizeDeposit`), we discovered that all four of our gnark wrappers (`deposit-prover/gnark-wrapper/`, `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-{1a,1b,2}/`) compile to a ~1 KB R1CS that adds no real Halo2-verification constraints — see R15 in §5 and Phase 8 in §3 of this plan. **AN-side should not wire our current VK (`deposit-prover/gnark-wrapper/verification.key`) into anything that claims cryptographic verification**, including the commented-out `gosh.vergrth16WithVK` call in PR 2112's `TokenBridge.finalizeDeposit`. We'll re-handoff the artefacts once Phase 8 closes (~8–16 calendar weeks of R&D, see §6). The `gosh.vergrth16WithVK` opcode in tvm-sdk is unaffected — that's correct generic Groth16-with-VK plumbing; only our specific VK is currently a stub-VK. For ABI smoke-testing on AN-side (verifying the call shape, gas measurement, integration scaffolding) the stub VK is *usable* as long as it's explicitly labelled as such in commit messages and code comments.

---

Q1 + Q2 gate Phase 1.C. Q3 + Q4 affect Phase 5.2 cleanliness. Q5 is CI-hermeticity polish. ~~P-1 is a one-way heads-up — no answer required.~~ **P-1 was a 2026-05-17 same-day heads-up to Alina about `gosh.vergrth16WithVK` wiring; obsoleted within hours by Phase 4.3 demolition retiring the entire deposit-side Groth16 chain (see OBSOLETED block above).** Phases 5.2 (Primary-only) and 5.3 can otherwise start now (they don't depend on the wrapper actually verifying Halo2 — the stub gives valid Groth16 calldata to exercise the integration path).
