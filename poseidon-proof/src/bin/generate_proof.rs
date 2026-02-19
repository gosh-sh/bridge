//! Binary to generate Poseidon preimage proof with Blake2b transcript.
//! Outputs: proof bytes, public inputs, verifying key (all as hex files).

use std::fs;

use halo2_base::{
    gates::{
        circuit::{builder::RangeCircuitBuilder, CircuitBuilderStage},
        RangeChip,
    },
    halo2_proofs::{
        halo2curves::{bn256::Fr, serde::SerdeObject},
        plonk::{keygen_pk, keygen_vk},
        poly::commitment::Params,
        SerdeFormat,
    },
    utils::{fs::gen_srs, testing::gen_proof_with_instances},
};
use poseidon_proof::{circuit::PoseidonPreimageCircuit, poseidon::poseidon_hash};

const K: u32 = 12;
const UNUSABLE_ROWS: usize = 9;

fn main() {
    let lookup_bits = K as usize - 1;

    println!("=== Poseidon Preimage Proof Generator (Blake2b transcript) ===");

    // --- Keygen phase ---
    println!("[1/4] Key generation...");
    let mut builder = RangeCircuitBuilder::from_stage(CircuitBuilderStage::Keygen)
        .use_k(K as usize)
        .use_instance_columns(1);
    builder.set_lookup_bits(lookup_bits);
    let range = RangeChip::new(lookup_bits, builder.lookup_manager().clone());

    let default_circuit = PoseidonPreimageCircuit::default_for_keygen(K, UNUSABLE_ROWS);
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

    let params = gen_srs(K);
    let vk = keygen_vk(&params, &builder).unwrap();
    let pk = keygen_pk(&params, vk.clone(), &builder).unwrap();
    let break_points = builder.break_points();
    drop(builder);

    println!("  Config params: {:?}", config_params);

    // --- Save VK ---
    println!("[2/4] Saving verifying key...");
    fs::create_dir_all("data").unwrap();
    {
        let mut vk_file = fs::File::create("data/vk.bin").unwrap();
        vk.write(&mut vk_file, SerdeFormat::RawBytes).unwrap();
    }

    // --- Prover phase ---
    println!("[3/4] Generating proof...");
    let secret = Fr::from(42u64); // Simple test secret
    let hash = poseidon_hash(&[secret]);

    println!("  Secret preimage: 42");
    println!("  Poseidon hash:   0x{}", hex::encode(hash.to_raw_bytes()));

    let pub_inputs: Vec<Fr> = vec![hash];

    let mut builder =
        RangeCircuitBuilder::prover(config_params.clone(), break_points).use_instance_columns(1);
    let range = RangeChip::new(lookup_bits, builder.lookup_manager().clone());

    let circuit = PoseidonPreimageCircuit::new(K, UNUSABLE_ROWS, secret);
    let instances = circuit.closure(builder.pool(0), &range);
    builder.assigned_instances[0] = instances;

    let proof = gen_proof_with_instances(&params, &pk, builder, &[&pub_inputs]);

    // --- Save artifacts ---
    println!("[4/4] Saving artifacts...");
    fs::write("data/proof.bin", &proof).unwrap();
    fs::write("data/proof.hex", hex::encode(&proof)).unwrap();

    // Save public inputs
    let pub_input_bytes: Vec<u8> = pub_inputs.iter().flat_map(|f| f.to_raw_bytes()).collect();
    fs::write("data/public_inputs.bin", &pub_input_bytes).unwrap();
    fs::write("data/public_inputs.hex", hex::encode(&pub_input_bytes)).unwrap();

    // Save params (just the serialized SRS)
    {
        let mut params_file = fs::File::create("data/params.bin").unwrap();
        params.write(&mut params_file).unwrap();
    }

    println!("\n=== Done ===");
    println!("  Proof size:    {} bytes", proof.len());
    println!("  Public inputs: 1 (Poseidon hash)");
    println!("  Transcript:    Blake2b (Challenge255)");
    println!("  Scheme:        KZG + SHPLONK on BN254");
    println!("\nArtifacts saved to data/:");
    println!("  data/vk.bin          - Verifying key");
    println!("  data/proof.bin       - Proof bytes");
    println!("  data/proof.hex       - Proof bytes (hex)");
    println!("  data/public_inputs.bin - Public inputs");
    println!("  data/public_inputs.hex - Public inputs (hex)");
    println!("  data/params.bin      - KZG params (SRS)");
}
