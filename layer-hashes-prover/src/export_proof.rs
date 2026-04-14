use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;
use gosh_dense_balanced_tree::{bytes_to_fr, DenseChainLink};
use gosh_bls_verification::helpers::deserialize_g1_pubkey;
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use halo2_base::halo2_proofs::halo2curves::group::ff::PrimeField;
use halo2_base::halo2_proofs::plonk::{keygen_pk, keygen_vk};
use halo2_base::utils::fs::gen_srs;
use halo2_base::utils::testing::gen_proof_with_instances;
use pse_poseidon::Poseidon;

use layer_hashes_update_halo2_circuit::primary_circuit::LayerHashesUpdateCircuit;
use layer_hashes_update_halo2_circuit::{
    MAX_LAYERS,
    POSEIDON_T, POSEIDON_RATE, POSEIDON_R_F, POSEIDON_R_P,
    FIRST_ROOT_HASH_OFFSET,
};

use layer_hashes_prover::proof_export::{build_proof_data, save_proof_data_json};

const K: u32 = 19;
const NUM_UNUSABLE_ROWS: usize = 109;
const LIMB_BITS: usize = 104;
const NUM_LIMBS: usize = 5;
const MAX_SIGNERS: usize = 300;

#[derive(Parser)]
#[command(name = "export-proof", about = "Generate a layer-hashes Halo2 proof and export for gnark")]
struct Cli {
    /// Path to a fixture JSON file (circuit_test_data_*.json)
    #[arg(short, long)]
    fixture: PathBuf,

    /// Output JSON file for gnark consumption
    #[arg(short, long, default_value = "halo2_proof.json")]
    output: PathBuf,
}

// ── Fixture types (mirrors test_real_data.rs) ──

#[derive(serde::Deserialize)]
struct ChainLinkJson {
    active: bool,
    siblings_hex: Vec<String>,
    position: usize,
    leaf_hex: String,
}

#[derive(serde::Deserialize)]
struct CircuitFixtureJson {
    block_envelope_hex: String,
    attestation_hex: String,
    bk_set: HashMap<String, String>,
    num_layers: usize,
    layer_hash_byte_offsets: Vec<usize>,
    root_hashes_hex: Vec<String>,
    prev_max_level_layer_hash_hex: String,
    num_prev_chain_steps: usize,
    prev_chain_proofs: Vec<ChainLinkJson>,
}

struct CircuitTestInput {
    block_data: Vec<u8>,
    attestation: Vec<u8>,
    bk_set: HashMap<u16, Vec<u8>>,
    history_proofs_byte_offset: usize,
    root_hashes: Vec<[u8; 32]>,
    num_layers: usize,
    prev_max_level_layer_hash: Fr,
    num_prev_chain_steps: usize,
    prev_chain_proofs: Vec<DenseChainLink>,
}

fn hex_to_32(s: &str) -> [u8; 32] {
    let bytes = hex::decode(s).unwrap_or_else(|_| panic!("invalid hex: {s}"));
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    arr
}

fn compute_bk_set_poseidon_instance(
    bk_set: &HashMap<u16, Vec<u8>>,
    limb_bits: usize,
    num_limbs: usize,
) -> Fr {
    let mut sorted_keys: Vec<u16> = bk_set.keys().cloned().collect();
    sorted_keys.sort();
    let mut poseidon_input: Vec<Fr> = Vec::new();
    for &idx in &sorted_keys {
        poseidon_input.push(Fr::from(idx as u64));
        let pk_bytes = &bk_set[&idx];
        let g1 = deserialize_g1_pubkey(pk_bytes);
        let x_bytes_le = g1.x.to_bytes();
        let x_bigint = num_bigint::BigUint::from_bytes_le(&x_bytes_le);
        let limb_mask = (num_bigint::BigUint::from(1u64) << limb_bits) - 1u64;
        for i in 0..num_limbs {
            let limb_val = (&x_bigint >> (i * limb_bits)) & &limb_mask;
            let limb_bytes = limb_val.to_bytes_le();
            let mut buf = [0u8; 32];
            let len = limb_bytes.len().min(32);
            buf[..len].copy_from_slice(&limb_bytes[..len]);
            poseidon_input.push(Fr::from_repr(buf).unwrap());
        }
    }
    let mut sponge =
        Poseidon::<Fr, POSEIDON_T, POSEIDON_RATE>::new(POSEIDON_R_F, POSEIDON_R_P);
    sponge.update(&poseidon_input);
    sponge.squeeze()
}

fn parse_fixture(fixture: &CircuitFixtureJson) -> CircuitTestInput {
    let block_data = hex::decode(&fixture.block_envelope_hex).expect("invalid block_envelope_hex");
    let attestation = hex::decode(&fixture.attestation_hex).expect("invalid attestation_hex");
    let bk_set: HashMap<u16, Vec<u8>> = fixture.bk_set.iter().map(|(k, v)| {
        let idx: u16 = k.parse().expect("bk_set key must be u16");
        let pk_bytes = hex::decode(v).expect("invalid pubkey hex");
        (idx, pk_bytes)
    }).collect();
    let root_hashes: Vec<[u8; 32]> = fixture.root_hashes_hex.iter().map(|s| hex_to_32(s)).collect();
    let first_offset = fixture.layer_hash_byte_offsets[0];
    let history_proofs_byte_offset = first_offset - FIRST_ROOT_HASH_OFFSET;
    let prev_bytes = hex_to_32(&fixture.prev_max_level_layer_hash_hex);
    let prev_fr = bytes_to_fr(&prev_bytes);
    let prev_chain_proofs: Vec<DenseChainLink> = fixture.prev_chain_proofs.iter().map(|link| {
        DenseChainLink {
            active: link.active,
            siblings: link.siblings_hex.iter().map(|s| hex_to_32(s)).collect(),
            position: link.position,
            leaf_native: hex_to_32(&link.leaf_hex),
        }
    }).collect();
    CircuitTestInput {
        block_data, attestation, bk_set, history_proofs_byte_offset,
        root_hashes, num_layers: fixture.num_layers,
        prev_max_level_layer_hash: prev_fr,
        num_prev_chain_steps: fixture.num_prev_chain_steps,
        prev_chain_proofs,
    }
}

fn build_instances(input: &CircuitTestInput) -> Vec<Fr> {
    let bk_set_commitment = compute_bk_set_poseidon_instance(&input.bk_set, LIMB_BITS, NUM_LIMBS);
    let mut instances = vec![bk_set_commitment, Fr::from(input.num_layers as u64)];
    for i in 0..MAX_LAYERS {
        instances.push(bytes_to_fr(&input.root_hashes[i]));
    }
    instances.push(input.prev_max_level_layer_hash);
    instances
}

fn build_circuit(input: &CircuitTestInput) -> LayerHashesUpdateCircuit {
    let layer_tree_depth = input.prev_chain_proofs[0].siblings.len();
    let lookup_bits = (K - 1) as usize;
    LayerHashesUpdateCircuit::new(
        input.block_data.clone(),
        input.attestation.clone(),
        input.bk_set.clone(),
        K as usize,
        NUM_UNUSABLE_ROWS,
        lookup_bits,
        LIMB_BITS,
        NUM_LIMBS,
        MAX_SIGNERS,
        input.num_layers,
        input.history_proofs_byte_offset,
        input.prev_max_level_layer_hash,
        input.num_prev_chain_steps,
        layer_tree_depth,
        input.prev_chain_proofs.clone(),
    )
}

fn main() {
    let cli = Cli::parse();

    println!("Loading fixture from {}...", cli.fixture.display());
    let json_str = std::fs::read_to_string(&cli.fixture)
        .unwrap_or_else(|e| panic!("Failed to read fixture: {e}"));
    let fixture: CircuitFixtureJson = serde_json::from_str(&json_str)
        .unwrap_or_else(|e| panic!("Failed to parse fixture: {e}"));
    let input = parse_fixture(&fixture);
    let instances = build_instances(&input);
    println!("  {} public inputs computed", instances.len());

    println!("Generating SRS (K={K})...");
    let t = Instant::now();
    let srs = gen_srs(K);
    println!("  SRS ready in {:?}", t.elapsed());

    println!("Building keygen circuit...");
    let t = Instant::now();
    let keygen_circuit = build_circuit(&input);
    let shared_params = keygen_circuit.params.base_circuit_params.clone();
    println!("  Circuit built in {:?}", t.elapsed());
    println!("  params: {:?}", shared_params);

    println!("Running keygen_vk + keygen_pk...");
    let t = Instant::now();
    let vk = keygen_vk(&srs, &keygen_circuit).expect("keygen_vk failed");
    let pk = keygen_pk(&srs, vk, &keygen_circuit).expect("keygen_pk failed");
    println!("  Keys generated in {:?}", t.elapsed());

    println!("Building proof circuit...");
    let t = Instant::now();
    let mut circuit = build_circuit(&input);
    circuit.override_base_circuit_params(shared_params);
    println!("  Circuit built in {:?}", t.elapsed());

    println!("Generating Halo2 proof...");
    let t = Instant::now();
    let proof_bytes = gen_proof_with_instances(&srs, &pk, circuit, &[&instances]);
    println!("  Proof generated in {:?} ({} bytes)", t.elapsed(), proof_bytes.len());

    println!("Exporting to {}...", cli.output.display());
    let proof_data = build_proof_data(proof_bytes, &instances, K);
    save_proof_data_json(&proof_data, &cli.output).expect("Failed to save JSON");

    println!("Done! Proof exported with {} public inputs.", proof_data.public_inputs.len());
    for (i, pi) in proof_data.public_inputs.iter().enumerate() {
        let label = match i {
            0 => "bk_set_commitment",
            1 => "num_layers",
            12 => "prev_max_level_layer_hash",
            _ => "layer_hash",
        };
        let truncated = if pi.len() > 20 { format!("{}...", &pi[..20]) } else { pi.clone() };
        println!("  [{i:2}] {label}: {truncated}");
    }
}
