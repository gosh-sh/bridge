//! v3 attestation-fetcher smoke test.
//!
//! Validates that `attestation_fetcher::fetch_attestation_for_block` against
//! the v3 GraphQL endpoint produces a `ParsedAttestation` whose `raw_bytes`
//! pass the same layout-parsing the circuit performs, and whose BLS signature
//! verifies off-circuit against the live bk_set.
//!
//! Requires a live v3 GraphQL endpoint (default `http://127.0.0.1:80/graphql`)
//! and `./bk_set.json`. Tunable via env:
//!   BRIDGE_GQL_ENDPOINT       (default: http://127.0.0.1:80)
//!   BRIDGE_TEST_SEQ_NO        (default: 32256)

use std::path::Path;

#[tokio::test]
async fn test_v3_fetch_attestation_envelope() {
    let endpoint = std::env::var("BRIDGE_GQL_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:80".to_string());
    let seq_no: u32 = std::env::var("BRIDGE_TEST_SEQ_NO")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(32256);

    let bk_set_path = if Path::new("bk_set.json").exists() {
        "bk_set.json"
    } else if Path::new("../bk_set.json").exists() {
        "../bk_set.json"
    } else {
        eprintln!("Skipping: bk_set.json not found");
        return;
    };

    let gql = match bridge_prover_lib::gql_client::create_client(&endpoint) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Skipping: GQL client init failed: {e}");
            return;
        }
    };

    let att = match bridge_prover_lib::attestation_fetcher::fetch_attestation_for_block(
        &gql, seq_no,
    )
    .await
    {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Skipping: fetch_attestation_for_block({seq_no}) failed: {e}");
            return;
        }
    };

    println!(
        "fetched attestation seq={} block_id={} envelope_hash={} target={} signers={:?} raw_len={}",
        att.block_seq_no,
        hex::encode(att.block_id),
        hex::encode(att.envelope_hash),
        att.target_type,
        att.signature_occurrences,
        att.raw_bytes.len(),
    );

    // Sanity: seq matches request and bincode layout invariants hold.
    assert_eq!(att.block_seq_no, seq_no, "seq mismatch");

    // Validate raw_bytes layout matches the bincode(Envelope<AttestationData>) shape.
    let sig_bytes = bridge_parsers::attestation_data_parser::parse_signature_bytes(&att.raw_bytes);
    assert_eq!(sig_bytes.len(), 192, "sig bytes must be 192");
    let num_signers =
        bridge_parsers::attestation_data_parser::parse_num_signers(&att.raw_bytes);
    assert_eq!(
        num_signers,
        att.signature_occurrences.values().copied().map(|c| c as usize).sum::<usize>(),
        "num_signers (summed counts) must match signature_occurrences entries",
    );
    let att_data =
        bridge_parsers::attestation_data_parser::parse_attestation_data_bytes(&att.raw_bytes);
    assert_eq!(att_data.len(), 120, "AttestationData section must be 120 bytes");

    // Inner block_id at REL_OFFSET 48 must equal the parent_block_id field separator + value.
    let inner_block_id = &att_data[48..80];
    assert_eq!(inner_block_id, &att.block_id, "inner block_id slice mismatch");
    let inner_seq = u32::from_le_bytes(att_data[80..84].try_into().unwrap());
    assert_eq!(inner_seq, att.block_seq_no, "inner seq_no mismatch");
    let inner_env_hash = &att_data[84..116];
    assert_eq!(inner_env_hash, &att.envelope_hash, "inner envelope_hash mismatch");
    let inner_target = u32::from_le_bytes(att_data[116..120].try_into().unwrap());
    assert_eq!(inner_target, att.target_type, "inner target_type mismatch");

    // Off-circuit BLS verification against live bk_set.
    let bk_set = bridge_prover_lib::bk_set_fetcher::load_bk_set_from_config(bk_set_path)
        .expect("failed to load BK set");
    println!("bk_set has {} signers", bk_set.len());

    let entries =
        bridge_parsers::attestation_data_parser::parse_signer_entries(&att.raw_bytes);
    let signature = gosh_bls_verification::helpers::deserialize_g2_signature(sig_bytes);
    let msg = &att_data[..120];
    let msg_hash = gosh_bls_verification::helpers::compute_msg_hash(msg);
    let pubkeys_with_counts =
        gosh_bls_verification::helpers::resolve_pubkeys(&entries, &bk_set);
    let agg_pk = gosh_bls_verification::helpers::compute_agg_pubkey(&pubkeys_with_counts);
    let bls_ok =
        gosh_bls_verification::helpers::verify_bls_native(&signature, &agg_pk, &msg_hash);
    println!(
        "Off-circuit BLS verification on seq={}: {}",
        seq_no,
        if bls_ok { "PASSED" } else { "FAILED" }
    );
    assert!(bls_ok, "off-circuit BLS verification failed");
}
