# Verifying an Acki Nacki Proof — End-to-End Guide (v2)

This document is the operational instruction set for **verifying that an Acki Nacki block update is correct** at every stage of the v2 pipeline. Two audiences are addressed:

- **Verifier** (auditor, relayer operator, bridge maintainer, on-chain caller): you receive a proof and want to confirm it's valid before trusting / submitting it.
- **Prover** (relayer, partner): you produce a proof and need to confirm it before publishing.

Both flows run through the same five checks, applied **per circuit** in v2 (was: once for the legacy single circuit). Producers do them all; verifiers can stop earlier if they trust the producer's keys.

For the *what-it-proves* and *how-it-was-built* context, see `docs/four_circuit_architecture.md` and `docs/layer_hashes_circuit_audit.md`. This doc is purely procedural.

> **v1 history**: the legacy single-circuit walkthrough lives at `docs/legacy/verifying_an_proof_v1.md`. It is no longer canonical; v1 proofs cannot be submitted to the v2 `AckiNackiBridge.verifyBlock` (different VKs, different public-input shape).

---

## 1. What Each Proof Asserts

In v2 a single AN block update is **two** ZK proofs (Circuit 1A *or* 1B + Circuit 2). Each proof is independent at the cryptographic level but bound at the public-input level via a shared `block_id` and `bk_set_poseidon`.

### 1.1 Circuit 1A — Primary Attestation (4 PI)

| # | Field | Meaning |
|---|---|---|
| 0 | `block_id` | 32-byte AN block identifier (envelope leaf-1 at offset 48), reduced mod Fr |
| 1 | `bk_set_commitment` | Poseidon commitment of the BK committee that signed the block |
| 2 | `block_seq_no` | AN block sequence number |
| 3 | `last_seen_block_seq_no` | Bridge's currently stored seqno (caller-supplied; bridge re-checks on submission) |

A valid proof certifies, conditional on knowing the witness:

1. ≥ 2/3 of the BK set whose Poseidon commitment is `bk_set_commitment` signed a Primary BLS attestation for an AN block whose envelope's offset-48 leaf is `block_id`.
2. The block's `target_type` discriminant (offset 116 of `AttestationData`) is Primary (`0`).
3. The BLS signature scheme used is `GoshBLS` (BLS12-381, hash-to-curve `ExpandMsgXmd`).

### 1.2 Circuit 1B — Fallback Attestation (4 PI)

Same shape as 1A, with `target_type = Fallback (1)` and threshold > 1/2 instead of ≥ 2/3.

### 1.3 Circuit 2 — Layer Hashes Movement (14 PI)

| # | Field | Meaning |
|---|---|---|
| 0 | `block_id` | Same as Circuit 1A/1B PI[0] |
| 1 | `bk_set_commitment` | Same as Circuit 1A/1B PI[1] |
| 2 | `num_layers` | Number of active layers (1..10) |
| 3..12 | `layer_hashes[0..10]` | Per-layer Poseidon Merkle roots; index ≥ `num_layers` is 0 |
| 13 | `prev_max_level_layer_hash` | Top-level hash of the previous chain (anchor) |

A valid proof certifies:

4. The 331-byte layer-hashes preimage SHA-256-hashes to envelope leaf-0 of the same block whose `block_id` is committed.
5. The 10 layer hashes were extracted from the correct BTreeMap offsets in the layer-hashes preimage (`BTREE_ENTRY_SIZE = 124`).
6. A Poseidon Merkle chain links `prev_max_level_layer_hash` to `layer_hashes[num_layers - 1]`.
7. `num_layers` is in `[1, 10]` (range-checked via three 4-bit checks).

### 1.4 Cross-circuit binding

Items 1+2+3 (above) come from Circuit 1A/1B. Items 4+5+6+7 come from Circuit 2. The two circuits agree on **the same AN block** because:

- they share `block_id` (envelope leaf-1) and `bk_set_commitment` (envelope leaf-2) at fixed positions in their public-input vectors;
- the bridge passes a single `blockId` argument and a single `bkSetCommitment` argument into both gnark verifier calls — any mismatch surfaces as one of the verifications returning `false`.

The bridge contract additionally enforces (`AckiNackiBridge.verifyBlock`):

- `bk_set_commitment == storedBkSetCommitment` (CC-3),
- `block_seq_no > storedLastSeenBlockSeqNo` (CC-5),
- `prev_max_level_layer_hash == storedPrevMaxLevelLayerHash` (CC-6),
- `layer_hashes[i] == 0` for `i ≥ num_layers` (CC-7).

See `docs/four_circuit_architecture.md` §4 for the full CC-# table.

---

## 2. Pipeline Overview

```
[Acki Nacki node — branch latest_an_to_eth_bridge_test (Q1 pending)]
    │  GraphQL: blocks, BOC, bkSetUpdates, attestations
    ▼
[bridge-relayer-daemon — Phase 5.1 (mock sources today)]
    │  per block:
    │  1. Fetch envelope, attestation, BK set, layer-hash chain proofs
    │  2. Detect Primary vs Fallback finalization
    │  3. Build attestation witness + layer-hashes witness
    │  4. (Phase 5.2) Recompute transition_hashes locally
    ▼
[bridge-prover-orchestrator]
    │  Per circuit (parallelisable):
    │  Circuit 1A or 1B:                Circuit 2:
    │  Halo2 SHPLONK prove (~3-6 min)   Halo2 SHPLONK prove (~3-6 min)
    │  Public inputs (4 Fr)             Public inputs (14 Fr)
    │  ↓                                ↓
    │  gnark Groth16 wrap (~5-15 s)     gnark Groth16 wrap (~5-15 s)
    │  → 256-byte attestationProof      → 256-byte layerHashesProof
    ▼
[Ethereum: AckiNackiBridge.verifyBlock]
    │  primaryVerifier OR fallbackVerifier      ~280k gas pairing check
    │  layerHashesVerifier                      ~280k gas pairing check
    │  Effects: advance state, emit BlockVerified
```

**Verification points** correspond to each arrow. You can verify at any subset of these stages.

---

## 3. The Five Verification Stages (per circuit)

Each circuit (1A, 1B, 2) is verified independently through these five stages. Stages V1, V4, V5 are tuple-aware (they cross-check both circuits' public inputs at once); V2 and V3 are per-circuit.

| Stage | What you check | Where to run | Time | Required artifacts |
|---|---|---|---|---|
| V1 | Public inputs match AN node ground truth + bound across circuits | off-chain query | seconds | AN node access + claimed PI for both circuits |
| V2 | Halo2 proof valid against per-circuit VK | Rust (`gosh-zk-snark-halo2-utils`) | < 1 s | Halo2 proof, instances, VK (one set per circuit) |
| V3 | gnark wrapping bound the same instances | Go (`gnark-wrappers/{circuit-1a,circuit-1b,circuit-2}/`) | < 1 s | gnark JSON, gnark VK |
| V4 | Groth16 proof verifies natively | Go (gnark) | < 1 s | Groth16 proof, gnark VK (one per circuit) |
| V5 | Tuple of Groth16 proofs verifies on-chain via `verifyBlock` | Foundry / cast | seconds | both proofs, deployed `AckiNackiBridge` |

---

## 4. Stage V1 — Public-Input Ground-Truth Cross-Check

**Goal**: confirm the claimed public inputs of both circuits describe the same real AN block.

**Inputs**:

- For Circuit 1A or 1B: `[block_id, bk_set_commitment, block_seq_no, last_seen_block_seq_no]`.
- For Circuit 2: `[block_id, bk_set_commitment, num_layers, layer_hashes[0..10], prev_max_level_layer_hash]`.

**Procedure**:

1. **Fetch the block envelope from AN GraphQL** (`blocks.where(seq_no = block_seq_no)`).
2. **Recompute envelope leaf-1** (offset 48, 32 bytes) → SHA-256 → reduce mod Fr → must equal `block_id` claimed by both circuits.
3. **Recompute envelope leaf-2** (offset 96, 32 bytes) → SHA-256 → reduce mod Fr → must equal `bk_set_commitment` claimed by both circuits. Independently verify by querying `/v2/bk_set` on the AN HTTP API and computing `bridge_poseidon::compute_bk_set_poseidon` (partner crate).
4. **Cross-check binding**: Circuit 1A/1B's PI[0] must byte-equal Circuit 2's PI[0]; Circuit 1A/1B's PI[1] must byte-equal Circuit 2's PI[1].
5. **Sanity-check the seqno**: `block_seq_no` from PI[2] of 1A/1B must match the AN block's `seq_no` field.
6. **Recompute layer hashes**: parse the layer-hashes preimage from the block envelope (offset 0, 331 bytes), extract the 10 BTreeMap entries at stride `BTREE_ENTRY_SIZE = 124`, hash each with Poseidon, compare against `layer_hashes[0..num_layers]`.
7. **Recompute the chain anchor**: walk the AN node's layer-hash chain from the previous key block to the current top-level hash, compare against `prev_max_level_layer_hash`.

**Pass condition**: all seven checks succeed.

**Common failure modes**:

- `block_id` mismatch between the two circuits → relayer mixed proofs from different blocks, or partner's `bridge-test-data-gen` produced inconsistent fixtures.
- `bk_set_commitment` mismatch → the relayer used a stale BK set; refetch via `/v2/bk_set_update` at the height of the block.
- `layer_hashes[i]` mismatch for some `i < num_layers` → bincode layout drift in the AN node (audit finding L-1; see `docs/audit_trail_v2.md` K-11).

---

## 5. Stage V2 — Native Halo2 Verification (per circuit)

**Goal**: confirm each Halo2 SHPLONK proof verifies under its own VK with the claimed instances.

**Per-circuit verification** (1A, 1B, 2):

```rust
use gosh_zk_snark_halo2_utils::proof::Proof;

let proof = Proof::load_from(proof_path)?;
let vk    = load_vk(vk_path)?;
let params = load_kzg_params(srs_path)?;        // kzg_bn254_19.srs

let instances: Vec<Vec<Fr>> = vec![claimed_public_inputs];

proof.verify_with_vk(&params, &vk, &instances)?;
// returns Ok(()) on success, Err(...) on any verification failure.
```

In our orchestrator, this is wrapped under `crates/bridge-prover-orchestrator/src/{primary_prover,fallback_prover,layer_hashes_prover}.rs::verify_proof`. Run via:

```bash
cd crates/bridge-prover-orchestrator
cargo test --release verify_round_trip
# Verifies each circuit's keygen → prove → verify path with the bound test data.
```

**Pass condition**: `verify_with_vk` returns `Ok(())` for all three proofs (or two if Circuit 1B is being skipped because the block was Primary-finalized).

**Why each circuit independently**: VKs are not shared; mixing a 1A proof with a 1B VK is a guaranteed failure regardless of soundness — this is a useful debugging check.

---

## 6. Stage V3 — gnark JSON Sanity (per circuit)

**Goal**: confirm the gnark JSON dump faithfully reproduces the Halo2 instances and protocol metadata.

**Per-circuit walkthrough**:

```bash
cd crates/bridge-prover-orchestrator/gnark-wrappers/circuit-1a
go run cmd/verify-json halo2_proof_circuit-1a.json
# Decodes the JSON, reconstructs the instances vector,
# compares byte-for-byte against the Halo2 .bin.

# Same for circuit-1b and circuit-2.
```

The JSON format is per-circuit (different number of instance entries; different protocol witness commitment counts). Each gnark-wrapper subdirectory has its own `cmd/verify-json` shim.

**Pass condition**: JSON instances byte-match the corresponding `.instances.bin`. If you modified the gnark wrapper (e.g. for a new circuit), this is the first thing to break.

---

## 7. Stage V4 — Native Groth16 Verification (per circuit)

**Goal**: confirm the gnark Groth16 proof verifies natively (off-chain) against the gnark VK.

**Per-circuit walkthrough**:

```bash
cd crates/bridge-prover-orchestrator/gnark-wrappers/circuit-1a
./gnark-wrapper verify groth16_proof_circuit-1a.hex \
                       groth16_public_inputs_circuit-1a.hex
# returns 0 on success, non-zero on any verification failure.

# Same for circuit-1b and circuit-2.
```

The wrapper's `verify` subcommand:

1. Decodes the 256-byte proof into 8 BN254 base-field elements (A.x, A.y, B.x_im, B.x_re, B.y_im, B.y_re, C.x, C.y per gnark MarshalSolidity layout).
2. Decodes the public-input hex blob (4 Fr × 32 bytes for 1A/1B; 14 Fr × 32 bytes for Circuit 2).
3. Calls `groth16.Verify(proof, vk, publicWitness)`.

**Pass condition**: all submitted proofs verify natively.

**Why before V5**: V4 lets you isolate a Solidity bug from a proof bug. If V4 passes but V5 fails, the bug is in the Solidity adapter or the auto-generated `*Groth16VerifierGenerated.sol`.

---

## 8. Stage V5 — On-Chain Tuple Verification

**Goal**: confirm the tuple `(attestationProof, layerHashesProof)` verifies through `AckiNackiBridge.verifyBlock` against a live (or forked) Ethereum.

This is the final integration check — it exercises both gnark Groth16 verifiers, the bridge's anchor checks (CC-3 / CC-5 / CC-6 / CC-7), and the state-transition logic in one transaction.

### 8.1 Foundry (in-repo, fastest)

The single-block bound real-proof test is `AckiNackiBridgeVerifyBlockTest::testHappyPathPrimary` (and `…::testHappyPathFallback`). It loads exported bound proofs from `crates/bridge-prover-orchestrator/exports/`:

```bash
# 1. Generate fresh bound proofs (Halo2 + gnark wrap, ~10-15 min total)
cargo run -p bridge-prover-orchestrator --bin export-bound-block-proofs --release

# 2. Replay them through verifyBlock
cd contracts/ethereum
forge test --match-test "testHappyPathPrimary" -vv
```

Expected output: `BlockVerified(blockId=…, blockSeqNo=…, finType=0 [Primary], numLayers=…)`. Any cross-circuit mismatch surfaces as one of the verifier-rejection reverts.

### 8.2 Anvil + cast (manual)

Once `bridge-relayer-daemon` Phase 5.2 lands (live `BlockSource`), the manual flow will be:

```bash
# 1. Boot Anvil + deploy bridge with verifyBlock wired (USE_VERIFY_BLOCK=true)
forge script script/DeployRealBridge.s.sol --broadcast --rpc-url http://localhost:8545

# 2. Fetch a real AN block + generate proofs via the relayer in one-shot mode
cargo run -p bridge-relayer-daemon -- one-shot --block-seq-no $SEQ \
    --rpc-url http://localhost:8545 --bridge $BRIDGE

# 3. Inspect on-chain state
cast call $BRIDGE "storedLastSeenBlockSeqNo()(uint64)"
cast call $BRIDGE "storedPrevMaxLevelLayerHash()(uint256)"
cast call $BRIDGE "getStoredLayerHashes()(uint256[10])"
```

### 8.3 Production (mainnet)

Same as Anvil, but against the deployed `AckiNackiBridge` and a production-grade relayer running continuously (Phase 5.3 + production hardening).

**Pass condition**: V5 emits `BlockVerified` and the storage reads match the claimed PI. Any anchor-mismatch revert means a discrepancy between the proof's claimed state and the bridge's recorded state — investigate before resubmitting.

---

## 9. Failure-Mode Quick Reference

| Failure at | Most likely cause | Where to look |
|---|---|---|
| V1 step 4 (binding mismatch) | Relayer mixed proofs from two blocks | Inspect the bound-test-data harness or live relayer; ensure both proofs originate from the same `BridgeTestData` |
| V1 step 6 (layer hashes mismatch) | Bincode layout drift in AN node | Audit finding L-1; pin AN node version |
| V2 (Halo2 verify fails) | Wrong VK or wrong SRS | Check that `kzg_bn254_19.srs` matches the one used at keygen time |
| V3 (gnark JSON mismatch) | Wrapper version skew | Rebuild the wrapper binary; check `crates/bridge-prover-orchestrator/gnark-wrappers/<circuit>/go.mod` |
| V4 (gnark verify fails) | Wrong gnark VK or wrong proof bytes | Confirm both files are from the same `setup` invocation |
| V5 `AttestationProofRejected` | Attestation proof bytes don't match the claimed `(blockId, bkSetCommitment, blockSeqNo, lastSeen)` | Recompute claimed PI; ensure `lastSeen` matches the on-chain `storedLastSeenBlockSeqNo` |
| V5 `LayerHashesProofRejected` | Layer-hashes proof bytes don't match the claimed `(blockId, bkSetCommitment, numLayers, layerHashes, prevMaxLevelLayerHash)` | Recompute claimed PI; verify layer-hashes preimage parsing |
| V5 `BkSetCommitmentMismatch(supplied, stored)` | Caller supplied a stale or wrong `bkSetCommitment` | Refetch BK set via `/v2/bk_set_update`; if BK set rotated, Phase 1.C (Circuit 3) is needed |
| V5 `BlockSeqNoNotMonotonic(supplied, stored)` | Replay or out-of-order submission | Check the bridge's `storedLastSeenBlockSeqNo`; only blocks with `seqNo > stored` are accepted |
| V5 `PrevAnchorMismatch(supplied, stored)` | Chain anchor desync; relayer skipped a block or saw a fork | Walk the AN chain from `storedLastSeenBlockSeqNo + 1` upward; ensure no gaps |
| V5 `LayerHashTailNonZero(i)` | Caller padded `layerHashes` with garbage past `numLayers` | Zero-fill the tail; check the relayer's witness construction |
| V5 `InvalidNumLayers(numLayers)` | `numLayers == 0` or `> 10` | Inspect AN block; out-of-range counts are bugs |
| V5 `VerifyBlockDisabled` | One of the three verifier slots was zero at construction | Redeploy with `VerifyBlockConfigLib.with(...)` instead of `disabled()` |

---

## 10. Reproducible End-to-End Recipe

```bash
# 0. One-time: pull SRS (kzg_bn254_19.srs) and verify checksum.
cd crates/bridge-prover-orchestrator/params
sha256sum kzg_bn254_19.srs    # compare against published checksum

# 1. Generate a bound block scenario + proofs (Halo2 + gnark, all 3 circuits).
cd ..
cargo run --bin export-bound-block-proofs --release
# Writes:
#   exports/bound_scenario.json
#   exports/halo2_proof_circuit-1a.{bin,json}  + instances
#   exports/halo2_proof_circuit-2.{bin,json}   + instances
#   exports/groth16_proof_circuit-1a.hex       + groth16_public_inputs_circuit-1a.hex
#   exports/groth16_proof_circuit-2.hex        + groth16_public_inputs_circuit-2.hex

# 2. V2 — native Halo2 verification (per circuit).
cargo test --release verify_round_trip

# 3. V3 — gnark JSON sanity (per circuit).
for c in circuit-1a circuit-2; do
  (cd gnark-wrappers/$c && go run cmd/verify-json ../../exports/halo2_proof_${c}.json)
done

# 4. V4 — native Groth16 verification (per circuit).
for c in circuit-1a circuit-2; do
  (cd gnark-wrappers/$c && \
   ./gnark-wrapper verify ../../exports/groth16_proof_${c}.hex \
                          ../../exports/groth16_public_inputs_${c}.hex)
done

# 5. V5 — on-chain tuple verification (Foundry).
cd ../../contracts/ethereum
forge test --match-test "testHappyPathPrimary" -vv
```

V1 (ground-truth cross-check against an AN node) only applies once Phase 5.2 ships — the
synthetic bound test data is internally consistent by construction, so V1 trivially passes
for it.

---

## 11. What This Doc Does *Not* Cover

| Outside this guide | Where it lives |
|---|---|
| Architecture & cross-circuit binding | `docs/four_circuit_architecture.md` |
| Invariant labels (LH-#, CC-#, etc.) | `docs/bridge_verification.md` |
| Hands-on manual review protocol with attack scenarios | `docs/manual_verification_runbook.md` |
| Mirror flow (Ethereum → AN deposit verification) | `docs/verifying_eth_proof_on_an.md` |
| Phase-by-phase implementation status | `docs/an_partner_integration_plan.md` |
| Trust-assumption delta v1 → v2 | `docs/audit_trail_v2.md` |
| Legacy v1 walkthrough (single-circuit, 13 PI) | `docs/legacy/verifying_an_proof_v1.md` |
