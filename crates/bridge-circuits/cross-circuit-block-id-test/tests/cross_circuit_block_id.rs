//! Cross-circuit convergence test: given a single natural-BE `block_id`
//! produced by a real (un-hacked) AN test fixture, both Circuit 1
//! (attestation BLS checker) and Circuit 2 (historical layer hashes movement
//! checker) must emit *the same* `block_id_fr` in their public instance [0].
//!
//! This is what the on-chain `AckiNackiBridge.verifyBlock` requires — it
//! feeds a single `blockId` argument to *both* SNARK verifiers, so if the
//! two circuits disagree on the byte-order convention for that value, one
//! of the two SNARK verifications must fail on real chain data.
//!
//! Prior to the fix in `primary_circuit.rs` / `fallback_circuit.rs` /
//! `attestation_data_parser.rs`, Circuit 1 folded the raw BE bytes without
//! reversal while Circuit 2 folded `reverse(BE)` → LE — a byte-order
//! mismatch. The test-data-generator's `generate_bridge_test_data` builds
//! an attestation with the *natural* BE block_id in its payload (no
//! orchestrator re-sign hack), so this test would have caught the drift
//! immediately.

use std::collections::HashMap;

use attestation_bls_checker_circuit::{
    primary_circuit::PrimaryAttestationBlsCheckerCircuit,
    test_instances::expected_public_instances,
    K as C1_K, LOOKUP_BITS as C1_LOOKUP_BITS, NUM_UNUSABLE_ROWS as C1_NUM_UNUSABLE_ROWS,
};
use bridge_poseidon::{LIMB_BITS, MAX_SIGNERS, NUM_LIMBS};
use bridge_test_data_gen::generator::{generate_bridge_test_data, BridgeTestData};
use bridge_test_data_gen::layer_hashes::ChainProofStep;
use gosh_dense_balanced_tree::DenseChainLink;
use halo2_base::halo2_proofs::{
    dev::MockProver,
    halo2curves::bn256::Fr,
};
use historical_layer_hashes_movement_checker_circuit::{
    circuit::LayerHashesMovementCheckerCircuit,
    test_helpers::{
        bytes_le_to_fr, K as C2_K, LOOKUP_BITS as C2_LOOKUP_BITS,
        NUM_UNUSABLE_ROWS as C2_NUM_UNUSABLE_ROWS,
    },
    LAYER_PREIMAGE_SIZE, MAX_LAYERS, NUM_MERKLE_SIBLINGS,
};

/// Convert a `ChainProofStep` (test-data-gen type) into a `DenseChainLink`
/// (circuit-facing type). The two are structurally identical apart from
/// the `leaf_value` / `leaf_native` field name.
fn to_dense_chain_link(step: &ChainProofStep) -> DenseChainLink {
    DenseChainLink {
        active: step.active,
        siblings: step.siblings.clone(),
        position: step.position,
        leaf_native: step.leaf_value,
    }
}

/// Compute the "canonical" block_id Fr = `uint256(bytes32(td.block_id))` =
/// LE-fold of `reverse(td.block_id)`. This is the value the on-chain
/// contract stores and feeds to both verifiers.
fn expected_block_id_fr(block_id_be: &[u8; 32]) -> Fr {
    let mut reversed = *block_id_be;
    reversed.reverse();
    bytes_le_to_fr(&reversed)
}

/// Build a `PrimaryAttestationBlsCheckerCircuit` from the shared bridge
/// test data.
fn build_circuit_1(
    attestation_bytes: Vec<u8>,
    bk_set: HashMap<u16, Vec<u8>>,
    last_seen_block_seqno: u32,
) -> PrimaryAttestationBlsCheckerCircuit<Fr> {
    PrimaryAttestationBlsCheckerCircuit::<Fr>::new(
        attestation_bytes,
        bk_set,
        last_seen_block_seqno,
        C1_K as usize,
        C1_NUM_UNUSABLE_ROWS,
        C1_LOOKUP_BITS,
        LIMB_BITS,
        NUM_LIMBS,
        MAX_SIGNERS,
    )
}

/// Build a `LayerHashesMovementCheckerCircuit` from the shared bridge test
/// data. Returns the circuit and its expected public instance vector.
fn build_circuit_2(
    td: &BridgeTestData,
    bk_set_poseidon_hash: Fr,
) -> (LayerHashesMovementCheckerCircuit, Vec<Fr>) {
    // Preimage: Vec<u8> → [u8; LAYER_PREIMAGE_SIZE].
    let preimage: [u8; LAYER_PREIMAGE_SIZE] = td
        .layer_hashes_preimage
        .as_slice()
        .try_into()
        .expect("layer_hashes_preimage must be LAYER_PREIMAGE_SIZE bytes");

    // Merkle siblings (L1, H_1, H_23) for the L0 leaf.
    let siblings: [[u8; 32]; NUM_MERKLE_SIBLINGS] = td.l0_opening_siblings;

    let prev_max_level_layer_hash_fr =
        bytes_le_to_fr(&td.layer_hash_chain.prev_max_level_layer_hash);

    // Circuit 2 expects "total active" chain-step count = num_prev + 1.
    // See test-data-gen::layer_hashes::generate_layer_hash_chain_with_depth
    // (total_active_steps = num_prev_chain_steps + 1).
    let num_active_steps: u8 = (td.layer_hash_chain.num_prev_chain_steps + 1)
        .try_into()
        .expect("chain step count must fit in u8");

    let prev_chain_proofs: Vec<DenseChainLink> = td
        .layer_hash_chain
        .chain_proofs
        .iter()
        .map(to_dense_chain_link)
        .collect();

    let circuit = LayerHashesMovementCheckerCircuit::new(
        preimage,
        siblings,
        prev_max_level_layer_hash_fr,
        num_active_steps,
        prev_chain_proofs,
        bk_set_poseidon_hash,
        C2_K as usize,
        C2_NUM_UNUSABLE_ROWS,
        C2_LOOKUP_BITS,
    );

    // Public instances the circuit emits, in order:
    //   [0]     block_id_fr        (must match Circuit 1's [0])
    //   [1]     bk_set_poseidon    (passthrough — whatever we passed in)
    //   [2]     num_layers
    //   [3..13] layer_hash_frs[0..10]
    //   [13]    prev_max_level_layer_hash
    let block_id_fr_expected = expected_block_id_fr(&td.block_id);
    let num_layers = td.layer_hash_chain.num_layers;
    let mut layer_hash_frs: Vec<Fr> = Vec::with_capacity(MAX_LAYERS);
    for i in 0..MAX_LAYERS {
        if i < num_layers {
            layer_hash_frs.push(bytes_le_to_fr(&td.layer_hash_chain.root_hashes[i]));
        } else {
            layer_hash_frs.push(Fr::zero());
        }
    }
    let mut expected = Vec::with_capacity(1 + 1 + 1 + MAX_LAYERS + 1);
    expected.push(block_id_fr_expected);
    expected.push(bk_set_poseidon_hash);
    expected.push(Fr::from(num_layers as u64));
    expected.extend_from_slice(&layer_hash_frs);
    expected.push(prev_max_level_layer_hash_fr);
    (circuit, expected)
}

/// Positive path: with the byte-order fix in Circuit 1, both circuits must
/// agree on `public[0] = block_id_fr` for the same raw AN block, and both
/// MockProvers must succeed against that shared value.
#[test]
fn same_block_id_fits_both_verifications() {
    // Small bk-set / layer counts keep the run reasonable (Circuit 1 is K=20).
    let td = generate_bridge_test_data(5, 3, 1)
        .expect("generate_bridge_test_data failed");

    // ---- Circuit 1: expected block_id_fr comes from
    //      `compute_block_id_fr` (now reverses before folding).
    let (last_seen_block_seqno, c1_instances) =
        expected_public_instances(&td.attestation_bytes, &td.bk_set, MAX_SIGNERS);
    let c1_block_id_fr = c1_instances[0];

    // ---- Circuit 2: expected block_id_fr from `reverse(td.block_id)` folded LE.
    let c2_block_id_fr = expected_block_id_fr(&td.block_id);

    // ---- Cross-circuit consistency check (the whole point of the test).
    assert_eq!(
        c1_block_id_fr, c2_block_id_fr,
        "Circuit 1 and Circuit 2 disagree on block_id_fr — the byte-order \
         convention has drifted again."
    );

    // ---- Run Circuit 1 MockProver.
    let circuit_1 = build_circuit_1(
        td.attestation_bytes.clone(),
        td.bk_set.clone(),
        last_seen_block_seqno,
    );
    let prover_1 = MockProver::run(C1_K, &circuit_1, vec![c1_instances])
        .expect("Circuit 1 MockProver::run failed");
    prover_1.assert_satisfied();

    // ---- Run Circuit 2 MockProver.
    // bk_set_poseidon_hash is a passthrough in Circuit 2 — any Fr works.
    let bk_set_poseidon_hash = Fr::from(0xDEADBEEFu64);
    let (circuit_2, c2_instances) = build_circuit_2(&td, bk_set_poseidon_hash);
    // Sanity: the instance vector we assembled agrees with what we just
    // asserted at the block_id level.
    assert_eq!(c2_instances[0], c2_block_id_fr);
    let prover_2 = MockProver::run(C2_K, &circuit_2, vec![c2_instances])
        .expect("Circuit 2 MockProver::run failed");
    prover_2.assert_satisfied();
}

/// Negative guard: if Circuit 1's public-instance-[0] value is *not* the
/// canonical reversed-LE fold (e.g. someone reverts the fix and lets it
/// fold raw BE again, feeding the natural-BE fold as the expected instance),
/// MockProver must reject.
///
/// This locks in the *direction* of the fix: passing a wrong-convention
/// block_id_fr as the public input fails constraint-equality against the
/// in-circuit computation.
#[test]
#[should_panic]
fn wrong_endian_block_id_fr_is_rejected_by_circuit_1() {
    let td = generate_bridge_test_data(5, 3, 1)
        .expect("generate_bridge_test_data failed");

    let (last_seen_block_seqno, mut c1_instances) =
        expected_public_instances(&td.attestation_bytes, &td.bk_set, MAX_SIGNERS);

    // Sabotage: replace the correct (reversed-LE) block_id_fr with the
    // *un-reversed* LE fold of the raw BE bytes — i.e. the buggy pre-fix
    // convention. The in-circuit `inner_product` now reverses, so the
    // public input no longer matches what the circuit emits.
    let bad_block_id_fr = bytes_le_to_fr(&td.block_id); // no reverse — wrong convention
    // Guard: only meaningful if the buggy value actually differs from the
    // canonical one. For random 32-byte hashes this is overwhelmingly true.
    assert_ne!(
        bad_block_id_fr,
        expected_block_id_fr(&td.block_id),
        "test fixture accidentally chose a palindromic block_id — rerun"
    );
    c1_instances[0] = bad_block_id_fr;

    let circuit_1 = build_circuit_1(
        td.attestation_bytes,
        td.bk_set,
        last_seen_block_seqno,
    );
    let prover = MockProver::run(C1_K, &circuit_1, vec![c1_instances])
        .expect("Circuit 1 MockProver::run failed");
    // Must panic: the injected block_id_fr disagrees with the circuit's own value.
    prover.assert_satisfied();
}
