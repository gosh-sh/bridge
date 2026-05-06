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
- 3.4 Document: per-circuit Groth16 setup ceremony status (each circuit gets its own gnark trusted setup; the trust assumption is identical to today's deposit prover and layer-hash bridge — already documented in `docs/aave_integration.md` and `docs/manual_verification_runbook.md`).

**Acceptance**:
- ✅ 4 `*Groth16VerifierGenerated.sol` contracts auto-generated by gnark, each ≤ 10 KB.
- ✅ Round-trip Foundry tests per circuit (positive + tamper + wrong-instance).
- ✅ Phase 1.A's synthetic fallback proof passes through `export_fallback_proof` → gnark wrap → Solidity verify with all 4 public inputs intact.
- ✅ Gas figures recorded in `docs/an_integration_gas_report.md`. Target: < 1 M gas total per `verifyBlock` call (vs. ~30 M+ for native Yul).

**Risk (revised)**:
- **R14 (NEW)** — gnark Groth16 ceremony per circuit means 4 trusted setups with all the hygiene that implies. Mitigation: same protocol we're using today for the deposit prover; document the ceremony for all 4 in a single `docs/gnark_ceremony.md`.
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

**Tasks**:
- 7.1 Update `AGENTS.md`: drop `LayerHashBridge.sol` references, add the new contract + 4 native verifiers + relayer.
- 7.2 Update `docs/integration_analysis.md` with the new architecture (replace single-circuit narrative with 4-circuit + envelope-hash-tree narrative).
- 7.3 Update `docs/integration_plan.md`: mark legacy M7-M9 as superseded by this plan.
- 7.4 Update `docs/manual_verification_runbook.md`: rewrite Phase G (Layer Hash Bridge) and Phase H (BK Rotation) to match the new contract; add new attack scenarios specific to the 4-circuit cross-checks (e.g., mixing proofs from different blocks).
- 7.5 Update `docs/verifying_an_proof.md` to V1–V5 against the **per-circuit** native Halo2 verifier rather than the gnark wrapper. Keep the gnark version as `docs/legacy/verifying_an_proof_gnark.md`.
- 7.6 Update `docs/bridge_verification.md`: rewrite §5 (LH-#) and §6 (BK-#) for new architecture; add §6.5 cross-circuit invariants (CC-#).
- 7.7 New doc: `docs/four_circuit_architecture.md` — concise overview, diagrams, leaf table, public-input layouts. Cross-link from `AGENTS.md` and `integration_analysis.md`.
- 7.8 Audit prep: list of trust assumptions reduced (chain anchor, BK commitment, last_seen_block_seqno) and assumptions retained (AN consensus, Halo2/SHPLONK soundness, gosh-halo2-crypto-lib correctness — same audit findings BLS-1, FORK-2 still apply).
- 7.9 Tagged release: `v2.0.0-rc1` once Phase 7 is complete and all CI pipelines (build, test, lint, format) pass.

**Acceptance**:
- ✅ All docs consistent with code.
- ✅ Audit-trail document listing what changed.
- ✅ CI pipeline green on `v2.0.0-rc1`.

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
| R14 | gnark Groth16 trusted setup per circuit (4 ceremonies) | New, post-pivot | Medium | Same trust model as today's deposit prover and layer-hash prover (both already in production pipeline). Document the four ceremonies in `docs/gnark_ceremony.md` with reproducibility checklist. |
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
| **Total** | **≈ 30 working days = 6 weeks** |

This assumes Phase 0 returns "all clear" within 2 days. Each "Medium" risk that materialises adds 2–5 days; an R1 or R2 hit would push the timeline closer to 8–9 weeks.

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
- **2026-05-07 (Phase 3.1 — first slice complete)**: Wired the gnark Groth16 path end-to-end for Circuit 1B. New artefacts:
  - `crates/bridge-prover-orchestrator/src/proof_export.rs` (mirrors `layer-hashes-prover::proof_export`, byte-compatible JSON for the gnark wrapper).
  - `crates/bridge-prover-orchestrator/src/bin/export_fallback_proof.rs` (produces `proof.bin` + `instances.bin` + `halo2_proof.json` from a Phase 1.A round-trip).
  - `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-1b/` Go module patterned on the layer-hashes wrapper, configured for 4 public inputs `[envelope_hash, bk_set_poseidon, block_seq_no, last_seen_block_seqno]`. Matches `FallbackProofOutput::instances` exactly.
  - `contracts/ethereum/src/{IFallbackGroth16Verifier,IFallbackVerifier,FallbackVerifier,FallbackGroth16VerifierGenerated}.sol` — interface, adapter, gnark-generated verifier (renamed `Verifier → FallbackGroth16VerifierGenerated`).
  - `contracts/ethereum/test/FallbackVerifier.t.sol` — 8 tests: positive (real proof verifies at **~222 657 gas**), 5 negative (tamper, wrong envelope hash, wrong BK set commitment, wrong block_seq_no, wrong last_seen, bad length), 1 ctor sanity.
  - Full forge suite: **143/143 passing** (135 baseline + 8 new for Fallback).
  - Per-block verifyBlock gas projection (3 Groth16 verifies @ ~225 k each): **≈ 700 k gas** total — well under the 30 M+ that native Yul would have needed if it could even deploy. R4 fully resolved by the pivot.
- **(pending)**: Q6, Q7, Q8 — partner SLA in flight.

### Phase status after these answers

| Phase | Pre-answer status | Post-answer status (after Q1, Q2, Q3, Q4, Q5) |
|---|---|---|
| 0 | All 8 questions open | 5/8 substantively answered (Q1, Q2, Q3 partial, Q4 promised, Q5); Q6, Q7, Q8 still open |
| **1.A — Circuit 1B wiring** | (n/a) | ✅ **Done 2026-05-06**. New `crates/bridge-prover-orchestrator/` crate wraps `FallbackAttestationBlsCheckerCircuit` with `FallbackKeyManager` + `generate_fallback_proof` + `verify_fallback_proof`. Round-trip integration test (`tests/fallback_round_trip.rs`) green: keygen 218 s (VK 140 s, PK 78 s), proof gen 11 s @ 10-signer set, verification + tamper-rejection + wrong-instance-rejection all pass. Final proof = 14 784 bytes; PK = 6.5 GB, VK = 6 KB on disk under `params/`. |
| **1.B — Circuit 2 wiring** | (n/a) | 🛠 Wait for AlinaT's "Circuit 2 cleanup done" signal (~today/tomorrow), then pin SHA. |
| **1.C — Circuit 3 wiring** | (n/a) | ⏸ **Deferred ~2 days** (partner-driven). Schedule it after 1.A + 1.B land. |
| 2 — Layer-hash pipeline | Blocked on Q1 + Q5 + Q7 | Stays blocked until `latest_an_to_eth_bridge_test` has the new envelope code + a node we can target. **Q7 still required.** |
| 3 — Solidity verifiers | Independent of partner | ✅ Can start in parallel — VKs only depend on circuit code, already in our local mirror |
| 4 — Contract rebuild | Independent of partner | ✅ Can start in parallel |
| 5 — Relayer | Blocked on Q1 + Q2 | Blocked. Plan adjustment: relayer should **recompute `transition_hashes` locally** rather than reading them from node-published values, to insulate us from the node-team friction surfaced by Ekaterina. |
| 6 — Real interface | Blocked on Phase 5 | Blocked |
| 7 — Docs / release | Final | Final |

**Re-sequenced Phase 1**: now three sub-phases, in this dependency order:
- **1.A — Circuit 1B prover/verifier wiring** — ✅ **Complete (2026-05-06)**. Crate `crates/bridge-prover-orchestrator/` exists, builds clean against pinned partner sibling repos, and the synthetic round-trip integration test passes end-to-end (keygen + prove + verify + 2 negative tests). Public-instance order locked at `[envelope_hash, bk_set_poseidon, block_seq_no, last_seen_block_seqno]` — matches Circuit 1B's `expose_public` sequence and is the contract layout we'll consume on-chain in Phase 4.
- **1.B — Circuit 2 prover/verifier wiring** — start when AlinaT signals her cleanup is stable.
- **1.C — Circuit 3 prover/verifier wiring** — start ~2 days after 1.B; partner is currently focused on 1+2.

Phases 3 and 4 (Solidity verifier generation and contract rebuild) are the most parallelisable independent work while we wait on 1.B. With 1.A done, recommended next moves: (a) start Phase 3 (generate `Halo2NativeVerifier1B.sol` against the cached `fallback_vk.bin`) and (b) when AlinaT signals stable, start Phase 1.B reusing the `FallbackKeyManager` template applied to `LayerHashesMovementCheckerCircuit`.

---

## 8. Open Questions for the Partner

The full message to send is in `docs/an_partner_phase0_questions.md`. Short version:

1. **Is `latest_an_to_eth_bridge_test` the right AN-node branch?** It's named in `bridge-prover-daemon/README.md` but not visible in our local `acki-nacki` clone. If yes, push it; if no, name the correct branch.
2. **Has `BlockKeeperSetChangeProofData.transition_hashes` been migrated to the new minimal-Poseidon format** (per Section 8 of `ENVELOPE_HASH_MERKLE_SPEC.md`)? If not, who owns the migration — us or you?
3. **Are MockProver tests passing on circuits 1B, 2, 3** at HEAD? Please pin a commit SHA we can patch against.
4. **Test vector for BK-set Poseidon**: please share one BK set (sorted, padded to 300, 1800 Fr inputs) → 32-byte Poseidon output computed by your `bridge-prover-lib::poseidon` — we'll reproduce it byte-for-byte to confirm encoding.
5. **Are shellnet blocks today using the new 8-leaf envelope format?** If yes, `bridge-prover-daemon` already proves them; if no, we'll need a local node from the bridge branch.
6. **Are the 4 historical-fixture data files** (`L2_H16_prevH0_S1` etc.) still useful, or should we re-generate them against the new format?
7. **How is the wave of hash "shifts" exposed in GraphQL?** Specifically, can we query the dense Merkle proof siblings (`Vec<DenseChainLink>`) for any block via a single GraphQL call, or do we have to compute them from raw layer-hash trees? (Affects Phase 2.2.)
8. **gnark vs. native Halo2** on Ethereum — we'll go native first; please flag if you've already produced gnark wrappers for any circuit (so we don't duplicate).

Once answers come back, this doc should be updated and the Phase 0 readiness report produced.
