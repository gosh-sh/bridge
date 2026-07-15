//! Integration test: parse a live attestation and verify BLS off-circuit.
//! Requires: /tmp/live_attestation.hex (extracted from live node)
//!           ./bk_set.json (BK set pubkeys)

use std::path::Path;

#[test]
fn test_live_attestation_bls_verification() {
    let att_hex_path = "/tmp/live_attestation.hex";
    // Integration test cwd may be the package dir or workspace root.
    let bk_set_path = if Path::new("bk_set.json").exists() {
        "bk_set.json"
    } else if Path::new("../bk_set.json").exists() {
        "../bk_set.json"
    } else {
        eprintln!("Skipping: bk_set.json not found");
        return;
    };

    if !Path::new(att_hex_path).exists() || !Path::new(bk_set_path).exists() {
        eprintln!("Skipping: live attestation or BK set not available");
        return;
    }

    // Load BK set.
    let bk_set = bridge_prover_lib::bk_set_fetcher::load_bk_set_from_config(bk_set_path)
        .expect("failed to load BK set");
    println!("BK set: {} signers", bk_set.len());

    // Load attestation bytes.
    let att_hex = std::fs::read_to_string(att_hex_path).unwrap();
    let att_bytes = hex::decode(att_hex.trim()).unwrap();
    println!("Attestation: {} bytes", att_bytes.len());

    // Parse with bridge-parsers.
    let sig_bytes = bridge_parsers::attestation_data_parser::parse_signature_bytes(&att_bytes);
    let num_signers = bridge_parsers::attestation_data_parser::parse_num_signers(&att_bytes);
    let entries = bridge_parsers::attestation_data_parser::parse_signer_entries(&att_bytes);
    println!("sig_len={}, num_signers={}, entries={:?}", sig_bytes.len(), num_signers, entries);

    // Verify BLS signature off-circuit.
    let signature = gosh_bls_verification::helpers::deserialize_g2_signature(sig_bytes);
    let att_data = bridge_parsers::attestation_data_parser::parse_attestation_data_bytes(&att_bytes);
    let msg = &att_data[..120];
    let msg_hash = gosh_bls_verification::helpers::compute_msg_hash(msg);
    let pubkeys_with_counts = gosh_bls_verification::helpers::resolve_pubkeys(&entries, &bk_set);
    let agg_pk = gosh_bls_verification::helpers::compute_agg_pubkey(&pubkeys_with_counts);
    let bls_ok = gosh_bls_verification::helpers::verify_bls_native(&signature, &agg_pk, &msg_hash);
    println!("Off-circuit BLS verification: {}", if bls_ok { "PASSED" } else { "FAILED" });
    assert!(bls_ok, "BLS signature verification failed on live attestation!");

    // Compute Poseidon commitment.
    let (commitment, _) = bridge_prover_lib::poseidon::compute_bk_set_poseidon(&bk_set);
    println!("BK set Poseidon commitment: {:?}", commitment);

    println!("All checks passed!");
}
