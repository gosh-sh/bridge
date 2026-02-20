use halo2_base::{
    gates::{
        circuit::{builder::RangeCircuitBuilder, CircuitBuilderStage},
        RangeChip,
    },
    halo2_proofs::{
        dev::MockProver,
        halo2curves::bn256::Fr,
        plonk::{keygen_pk, keygen_vk},
    },
    utils::{
        fs::gen_srs,
        testing::{check_proof_with_instances, gen_proof_with_instances},
    },
};
use rand::random;

use crate::{circuit::PoseidonPreimageCircuit, poseidon::poseidon_hash};

const K: u32 = 12;
const UNUSABLE_ROWS: usize = 9;

#[test]
fn test_mock_prover_valid() {
    let preimage = Fr::from(42u64);
    let circuit = PoseidonPreimageCircuit::new(K, UNUSABLE_ROWS, preimage);
    let mut builder = circuit.create_mock();

    let t_cells_lookup = builder
        .lookup_manager()
        .iter()
        .map(|lm| lm.total_rows())
        .sum::<usize>();
    let lookup_bits = if t_cells_lookup == 0 {
        None
    } else {
        builder.lookup_bits()
    };
    builder.config_params.lookup_bits = lookup_bits;
    builder.calculate_params(Some(UNUSABLE_ROWS));

    MockProver::run(K, &builder, circuit.public_inputs())
        .unwrap()
        .assert_satisfied();
}

#[test]
fn test_mock_prover_invalid() {
    let preimage = Fr::from(42u64);
    let circuit = PoseidonPreimageCircuit::new(K, UNUSABLE_ROWS, preimage);
    let mut builder = circuit.create_mock();

    let t_cells_lookup = builder
        .lookup_manager()
        .iter()
        .map(|lm| lm.total_rows())
        .sum::<usize>();
    let lookup_bits = if t_cells_lookup == 0 {
        None
    } else {
        builder.lookup_bits()
    };
    builder.config_params.lookup_bits = lookup_bits;
    builder.calculate_params(Some(UNUSABLE_ROWS));

    // Wrong public input should fail
    let wrong_instances = vec![vec![Fr::from(999u64)]];
    assert!(MockProver::run(K, &builder, wrong_instances)
        .unwrap()
        .verify()
        .is_err());
}

#[test]
fn test_real_proof_blake2b() {
    let k = K;
    let lookup_bits = k as usize - 1;

    // --- Keygen phase ---
    let mut builder = RangeCircuitBuilder::from_stage(CircuitBuilderStage::Keygen)
        .use_k(k as usize)
        .use_instance_columns(1);
    builder.set_lookup_bits(lookup_bits);
    let range = RangeChip::new(lookup_bits, builder.lookup_manager().clone());

    let default_circuit = PoseidonPreimageCircuit::default_for_keygen(k, UNUSABLE_ROWS);
    let res = default_circuit.closure(builder.pool(0), &range);
    builder.assigned_instances[0] = res;

    let t_cells_lookup = builder
        .lookup_manager()
        .iter()
        .map(|lm| lm.total_rows())
        .sum::<usize>();
    let lookup_bits_ = if t_cells_lookup == 0 {
        None
    } else {
        Some(lookup_bits)
    };
    builder.config_params.lookup_bits = lookup_bits_;
    let config_params = builder.calculate_params(Some(UNUSABLE_ROWS));

    let params = gen_srs(k);
    let vk = keygen_vk(&params, &builder).unwrap();
    let pk = keygen_pk(&params, vk.clone(), &builder).unwrap();
    let break_points = builder.break_points();
    drop(builder);

    // --- Prover phase ---
    let secret = Fr::from(random::<u64>());
    let hash = poseidon_hash(&[secret]);
    let pub_inputs: Vec<Fr> = vec![hash];

    let mut builder =
        RangeCircuitBuilder::prover(config_params.clone(), break_points).use_instance_columns(1);
    let range = RangeChip::new(lookup_bits, builder.lookup_manager().clone());

    let circuit = PoseidonPreimageCircuit::new(k, UNUSABLE_ROWS, secret);
    let instances = circuit.closure(builder.pool(0), &range);
    builder.assigned_instances[0] = instances;

    // Generate proof with Blake2b transcript (standard halo2_proofs)
    let proof = gen_proof_with_instances(&params, &pk, builder, &[&pub_inputs]);

    println!("Proof size: {} bytes", proof.len());

    // --- Verifier phase ---
    // Verify with correct public inputs (should succeed)
    check_proof_with_instances(&params, &vk, &proof, &[&pub_inputs], true);

    // Verify with wrong public inputs (should fail)
    let wrong_inputs = vec![Fr::from(999u64)];
    check_proof_with_instances(&params, &vk, &proof, &[&wrong_inputs], false);

    println!("Blake2b proof generation and verification: SUCCESS");
}
