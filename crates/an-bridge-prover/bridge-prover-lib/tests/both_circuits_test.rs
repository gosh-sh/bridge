//! Integration test: Circuit 1a (real data from shellnet) + Circuit 2 (MockProver).
//!
//! Demonstrates that both circuits work correctly:
//! - Circuit 1a: real attestation from shellnet → proof gen → verification
//! - Circuit 2: synthetic layer hash chain → MockProver constraint check
//! - Combined: key generation for both circuits
//!
//! Run: cargo test --release test_both_circuits -- --nocapture

use std::path::Path;
use std::time::Instant;

use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use halo2_base::halo2_proofs::halo2curves::group::ff::PrimeField;
use halo2_base::halo2_proofs::dev::MockProver;

use bridge_prover_lib::keys::KeyManager;
use bridge_prover_lib::poseidon;
use bridge_prover_lib::prover;
use bridge_prover_lib::verifier;

use historical_layer_hashes_movement_checker_circuit::{
    circuit::LayerHashesMovementCheckerCircuit,
    LAYER_PREIMAGE_SIZE, MAX_LAYERS,
    test_helpers::{K as LAYER_K, NUM_UNUSABLE_ROWS as LAYER_UNUSABLE, LOOKUP_BITS as LAYER_LOOKUP, bytes_le_to_fr},
};
use gosh_dense_balanced_tree::{bytes_to_fr, DenseChainLink};

/// Test Circuit 2 (Layer Hashes Movement Checker) with MockProver.
///
/// This validates that the circuit constraints are satisfied with synthetic data.
#[test]
fn test_circuit2_mockprover() {
    let t_total = Instant::now();

    let num_layers: u8 = 3;
    let num_chain_steps: u8 = 2;

    // 1. Build chain data.
    let chain_data = bridge_test_data_gen::layer_hashes::generate_layer_hash_chain(
        num_layers as usize,
        (num_chain_steps - 1) as usize, // num_prev_chain_steps
    );

    // 2. Build preimage.
    let mut preimage = [0u8; LAYER_PREIMAGE_SIZE];
    preimage[0] = num_layers;
    for i in 0..10 {
        let offset = 1 + i * 33;
        preimage[offset] = (i + 1) as u8;
        if i < num_layers as usize {
            preimage[offset + 1..offset + 1 + 32].copy_from_slice(&chain_data.root_hashes[i]);
        }
    }

    // 3. Build Merkle siblings (synthetic).
    let siblings: [[u8; 32]; 3] = {
        let mut s = [[0u8; 32]; 3];
        for i in 0..3 {
            for j in 0..32 {
                s[i][j] = ((i * 32 + j + 0x10) & 0xFF) as u8;
            }
        }
        s
    };

    // 4. Chain proof data.
    let prev_hash_fr = bytes_to_fr(&chain_data.prev_max_level_layer_hash);
    let chain_links: Vec<DenseChainLink> = chain_data
        .chain_proofs
        .iter()
        .map(|step| DenseChainLink {
            active: step.active,
            siblings: step.siblings.clone(),
            position: step.position,
            leaf_native: step.leaf_value,
        })
        .collect();

    let bk_set_poseidon_hash = Fr::from(0xDEADBEEFu64);

    // 5. Compute expected public instances.
    let block_id_fr = compute_block_id_fr_native(&preimage, &siblings);
    let num_layers_fr = Fr::from(num_layers as u64);

    let mut layer_hash_frs = Vec::with_capacity(MAX_LAYERS);
    for i in 0..MAX_LAYERS {
        let offset = 2 + i * 33;
        let hash_bytes = &preimage[offset..offset + 32];
        let mut repr = [0u8; 32];
        repr.copy_from_slice(hash_bytes);
        layer_hash_frs.push(Option::from(Fr::from_repr(repr)).unwrap_or(Fr::zero()));
    }

    let mut expected_instances = vec![block_id_fr, bk_set_poseidon_hash, num_layers_fr];
    for fr in &layer_hash_frs {
        expected_instances.push(*fr);
    }
    expected_instances.push(prev_hash_fr);

    println!("block_id_fr = {:?}", block_id_fr);
    println!("num_layers = {}", num_layers);
    println!("chain_steps = {}", num_chain_steps);
    println!("Total public instances: {}", expected_instances.len());

    // 6. Build circuit.
    let t = Instant::now();
    let circuit = LayerHashesMovementCheckerCircuit::new(
        preimage,
        siblings,
        prev_hash_fr,
        num_chain_steps,
        chain_links,
        bk_set_poseidon_hash,
        LAYER_K as usize,
        LAYER_UNUSABLE,
        LAYER_LOOKUP,
    );
    println!("[timing] circuit construction: {:?}", t.elapsed());
    println!("base_circuit_params: {:?}", circuit.base_circuit_params());

    // 7. Run MockProver.
    println!("Running MockProver at K={}...", LAYER_K);
    let t = Instant::now();
    let prover = MockProver::run(LAYER_K, &circuit, vec![expected_instances]).unwrap();
    println!("[timing] MockProver::run: {:?}", t.elapsed());

    let t = Instant::now();
    prover.assert_satisfied();
    println!("[timing] MockProver::assert_satisfied: {:?}", t.elapsed());

    println!(
        "[timing] TOTAL Circuit 2 MockProver: {:?}",
        t_total.elapsed()
    );
    println!(
        "Circuit 2 MockProver PASSED ({} layers, {} chain steps)!",
        num_layers, num_chain_steps
    );
}

/// Test Circuit 1a with real attestation from shellnet.
///
/// Requires: internet access to shellnet, bk_set.json config file.
/// Generates a real proof and verifies it.
#[test]
fn test_circuit1a_real_proof() {
    let t_total = Instant::now();

    // 1. Load BK set.
    let bk_set = match bridge_prover_lib::bk_set_fetcher::load_bk_set_from_config("./bk_set.json") {
        Ok(bk) => {
            println!("BK set loaded from config: {} signers", bk.len());
            bk
        }
        Err(e) => {
            println!("BK set config not found ({}), trying shellnet...", e);
            let rt = tokio::runtime::Runtime::new().unwrap();
            let gql = bridge_prover_lib::gql_client::create_client(
                "https://shellnet.ackinacki.org/graphql",
            )
            .unwrap();
            match rt.block_on(bridge_prover_lib::bk_set_fetcher::fetch_bk_set(&gql)) {
                Ok(bk) => {
                    println!("BK set from shellnet: {} signers", bk.len());
                    bk
                }
                Err(e2) => {
                    println!("SKIPPING test_circuit1a_real_proof: no BK set available ({}, {})", e, e2);
                    return;
                }
            }
        }
    };

    if bk_set.is_empty() {
        println!("SKIPPING: empty BK set");
        return;
    }

    let (bk_set_commitment, _) = poseidon::compute_bk_set_poseidon(&bk_set);
    println!("BK set commitment: {:?}", bk_set_commitment);

    // 2. Initialize keys.
    let params_dir = Path::new("./params");
    let mut key_manager = KeyManager::new(params_dir);

    println!("ensuring primary keys...");
    let t = Instant::now();
    key_manager.ensure_primary_keys(&bk_set).unwrap();
    println!("[timing] primary keys: {:?}", t.elapsed());

    // 3. Fetch a real attestation from shellnet.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let gql = bridge_prover_lib::gql_client::create_client(
        "https://shellnet.ackinacki.org/graphql",
    )
    .unwrap();

    let blocks = rt.block_on(gql.query_latest_blocks(20)).unwrap();
    let latest_seq = blocks.iter().map(|(_, s)| *s).max().unwrap_or(0);
    let target_seq = (latest_seq - 5) as u32;

    println!("fetching attestation for block {}...", target_seq);
    let attestation = match rt.block_on(
        bridge_prover_lib::attestation_fetcher::fetch_attestation_for_block(&gql, target_seq),
    ) {
        Ok(att) => att,
        Err(e) => {
            println!("SKIPPING: attestation not available for block {}: {}", target_seq, e);
            return;
        }
    };

    if attestation.target_type != 0 {
        println!("SKIPPING: got fallback attestation (type={})", attestation.target_type);
        return;
    }

    // Check signers in BK set.
    let missing: Vec<u16> = attestation
        .signature_occurrences
        .keys()
        .filter(|idx| !bk_set.contains_key(idx))
        .cloned()
        .collect();
    if !missing.is_empty() {
        println!("SKIPPING: signers {:?} not in BK set", missing);
        return;
    }

    println!(
        "attestation: seq={}, type=Primary, signers={:?}",
        attestation.block_seq_no,
        attestation.signature_occurrences.keys().collect::<Vec<_>>()
    );

    // 4. Generate proof.
    let last_seen = target_seq.saturating_sub(1);
    println!("generating Circuit 1a proof (last_seen={})...", last_seen);
    let t = Instant::now();
    let proof_output = prover::generate_primary_proof(
        &key_manager,
        &attestation.raw_bytes,
        &bk_set,
        last_seen,
    )
    .unwrap();
    let proof_time = t.elapsed();
    println!("[timing] Circuit 1a proof generation: {:?}", proof_time);
    println!("proof size: {} bytes", proof_output.proof_bytes.len());

    // 5. Verify proof.
    let instances = vec![
        proof_output.block_id_fr,
        proof_output.bk_set_commitment_fr,
        Fr::from(proof_output.block_seq_no as u64),
        Fr::from(proof_output.last_seen_block_seqno as u64),
    ];

    let t = Instant::now();
    let verified = verifier::verify_primary_proof(
        &key_manager,
        &proof_output.proof_bytes,
        &instances,
    );
    let verify_time = t.elapsed();
    println!("[timing] Circuit 1a verification: {:?}", verify_time);

    assert!(verified, "Circuit 1a proof verification FAILED!");
    println!("Circuit 1a VERIFIED OK!");
    println!("[timing] TOTAL Circuit 1a: {:?}", t_total.elapsed());
}

/// Test Circuit 2 key generation (VK + PK).
#[test]
fn test_circuit2_keygen() {
    let t_total = Instant::now();

    let params_dir = Path::new("./params");
    let mut key_manager = KeyManager::new(params_dir);

    println!("generating Circuit 2 keys...");
    let t = Instant::now();
    key_manager.ensure_layer_keys().unwrap();
    let keygen_time = t.elapsed();
    println!("[timing] Circuit 2 keygen: {:?}", keygen_time);
    println!("layer config: {:?}", key_manager.layer_config());

    // Verify VK and PK exist.
    assert!(key_manager.layer_vk.is_some(), "layer VK should be loaded");
    assert!(key_manager.layer_pk.is_some(), "layer PK should be loaded");

    println!("Circuit 2 keygen PASSED! Total: {:?}", t_total.elapsed());
}

/// Compute block_id Fr natively (same as in layer_prover.rs).
fn compute_block_id_fr_native(preimage: &[u8; LAYER_PREIMAGE_SIZE], siblings: &[[u8; 32]; 3]) -> Fr {
    use sha2::{Digest, Sha256};

    let l0_hash = bridge_poseidon::poseidon_hash_bytes(preimage);
    let mut l0_bytes = [0u8; 32];
    l0_bytes.copy_from_slice(&l0_hash);

    let h0: [u8; 32] = {
        let mut input = Vec::with_capacity(64);
        input.extend_from_slice(&l0_bytes);
        input.extend_from_slice(&siblings[0]);
        Sha256::digest(&input).into()
    };

    let h01: [u8; 32] = {
        let mut input = Vec::with_capacity(64);
        input.extend_from_slice(&h0);
        input.extend_from_slice(&siblings[1]);
        Sha256::digest(&input).into()
    };

    let root_be: [u8; 32] = {
        let mut input = Vec::with_capacity(64);
        input.extend_from_slice(&h01);
        input.extend_from_slice(&siblings[2]);
        Sha256::digest(&input).into()
    };

    let mut root_le = root_be;
    root_le.reverse();

    bytes_le_to_fr(&root_le)
}
