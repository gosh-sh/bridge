//! Phase 1.B end-to-end round-trip test for Circuit 2 (Layer Hashes Movement):
//! synthetic test data → prove → verify → tamper-rejection.
//!
//! Heavy: first run does keygen for K=17 (~5–30 s) and writes ~few-hundred-MB
//! PK to disk under `crates/bridge-prover-orchestrator/params/`. Subsequent
//! runs reuse the cache.

use std::path::PathBuf;

use bridge_prover_orchestrator::{
    build_synthetic_layer_hashes_input, generate_layer_hashes_proof,
    layer_hashes_keys::LayerHashesReferenceWitness, verify_layer_hashes_proof, Fr,
    LayerHashesKeyManager, LayerHashesProofInput,
};

fn params_dir() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("params");
    p
}

#[test]
fn layer_hashes_round_trip_synthetic() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let mut km = LayerHashesKeyManager::new(&params_dir());

    // Reference witness for keygen — uses (num_layers=10, num_chain_steps=10)
    // to maximally exercise advice / lookup columns. Single VK works for any
    // (num_layers, num_chain_steps) combination since chain padding is fixed.
    let reference = build_synthetic_layer_hashes_input(10, 10);
    km.ensure_keys(&LayerHashesReferenceWitness {
        layer_hashes_preimage: reference.layer_hashes_preimage,
        merkle_siblings: reference.merkle_siblings,
        prev_max_level_layer_hash: reference.prev_max_level_layer_hash,
        num_prev_chain_steps: reference.num_prev_chain_steps,
        prev_chain_proofs: reference.prev_chain_proofs.clone(),
        bk_set_poseidon_hash: reference.bk_set_poseidon_hash,
    })
    .unwrap();

    // Real test input: minimal (num_layers=1, num_chain_steps=1).
    let input_data = build_synthetic_layer_hashes_input(1, 1);
    let proof = generate_layer_hashes_proof(&km, LayerHashesProofInput {
        layer_hashes_preimage: input_data.layer_hashes_preimage,
        merkle_siblings: input_data.merkle_siblings,
        prev_max_level_layer_hash: input_data.prev_max_level_layer_hash,
        num_prev_chain_steps: input_data.num_prev_chain_steps,
        prev_chain_proofs: &input_data.prev_chain_proofs,
        bk_set_poseidon_hash: input_data.bk_set_poseidon_hash,
        expected_instances: input_data.expected_instances,
    })
    .expect("layer-hashes proof generation must succeed");

    assert!(!proof.proof_bytes.is_empty(), "proof must be non-empty");
    let instances = proof.instances();

    let ok = verify_layer_hashes_proof(&km, &proof.proof_bytes, &instances);
    assert!(ok, "native layer-hashes verification must succeed");

    // Negative: tampered proof.
    let mut tampered = proof.proof_bytes.clone();
    let mid = tampered.len() / 2;
    tampered[mid] ^= 0xFF;
    let ok_tampered = verify_layer_hashes_proof(&km, &tampered, &instances);
    assert!(!ok_tampered, "tampered layer-hashes proof must NOT verify");

    // Negative: wrong public instances (flip prev_hash).
    let mut wrong_instances = instances;
    wrong_instances[13] = wrong_instances[13] + Fr::one();
    let ok_wrong = verify_layer_hashes_proof(&km, &proof.proof_bytes, &wrong_instances);
    assert!(!ok_wrong, "wrong instances must NOT verify");

    println!(
        "OK: layer-hashes round-trip succeeded (proof = {} bytes, {} public inputs)",
        proof.proof_bytes.len(),
        instances.len()
    );
}
