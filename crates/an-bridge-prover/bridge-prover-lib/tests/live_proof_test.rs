//! Integration test: generate and verify a real halo2 proof from live node attestation.
//! Requires: /tmp/live_attestation.hex + ./bk_set.json + running node for fresh attestation

use std::path::Path;
use std::time::Instant;

#[test]
fn test_live_proof_generation_and_verification() {
    let att_hex_path = "/tmp/live_attestation.hex";
    let bk_set_path = if Path::new("bk_set.json").exists() {
        "bk_set.json"
    } else if Path::new("../bk_set.json").exists() {
        "../bk_set.json"
    } else {
        eprintln!("Skipping: bk_set.json not found");
        return;
    };

    if !Path::new(att_hex_path).exists() {
        eprintln!("Skipping: /tmp/live_attestation.hex not found");
        return;
    }

    let params_dir = if Path::new("params").exists() {
        "params"
    } else if Path::new("../params").exists() {
        "../params"
    } else {
        "params"
    };

    println!("=== Live Proof Generation Test ===");
    let t_total = Instant::now();

    // 1. Load BK set.
    let bk_set = bridge_prover_lib::bk_set_fetcher::load_bk_set_from_config(bk_set_path)
        .expect("failed to load BK set");
    println!("BK set: {} signers", bk_set.len());

    // 2. Load attestation bytes.
    let att_hex = std::fs::read_to_string(att_hex_path).unwrap();
    let att_bytes = hex::decode(att_hex.trim()).unwrap();
    println!("Attestation: {} bytes", att_bytes.len());

    // 3. Initialize key manager (loads SRS, tries cached VK/PK).
    println!("\nStep 1: Loading keys...");
    let t = Instant::now();
    let mut key_manager = bridge_prover_lib::keys::KeyManager::new(Path::new(params_dir));
    key_manager.ensure_primary_keys(&bk_set).expect("keygen failed");
    println!("[timing] key loading/generation: {:?}", t.elapsed());

    // 4. Generate proof.
    println!("\nStep 2: Generating proof...");
    let t = Instant::now();
    let last_seen_seqno: u32 = 0; // first proof, no previous block
    let proof_output = bridge_prover_lib::prover::generate_primary_proof(
        &key_manager,
        &att_bytes,
        &bk_set,
        last_seen_seqno,
    )
    .expect("proof generation failed");
    let proof_time = t.elapsed();
    println!("[timing] proof generation: {:?}", proof_time);
    println!("  proof size: {} bytes", proof_output.proof_bytes.len());
    println!("  block_seq_no: {}", proof_output.block_seq_no);
    println!("  block_id: {:?}", proof_output.block_id_fr);
    println!("  bk_set_commitment: {:?}", proof_output.bk_set_commitment_fr);

    // 5. Verify proof.
    println!("\nStep 3: Verifying proof...");
    let t = Instant::now();
    let instances = vec![
        proof_output.block_id_fr,
        proof_output.bk_set_commitment_fr,
        bridge_prover_lib::Fr::from(proof_output.block_seq_no as u64),
        bridge_prover_lib::Fr::from(last_seen_seqno as u64),
    ];
    let verified = bridge_prover_lib::verifier::verify_primary_proof(
        &key_manager,
        &proof_output.proof_bytes,
        &instances,
    );
    let verify_time = t.elapsed();
    println!("[timing] verification: {:?}", verify_time);
    println!("Verified: {}", verified);

    assert!(verified, "Proof verification FAILED on live attestation data!");

    println!("\n=== TOTAL: {:?} ===", t_total.elapsed());
    println!("LIVE PROOF GENERATION AND VERIFICATION: PASSED!");
}
