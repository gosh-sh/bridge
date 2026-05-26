//! End-to-end round-trip test for Circuit 1B (Fallback attestation):
//! synthetic test data → prove → verify → check public instances.
//!
//! This is the Phase 1.A acceptance criterion. Runs the full pipeline using the
//! partner's
//! `bridge_test_data_gen::generator::generate_test_data_fallback_all_sign(N)`.
//!
//! Heavy: first run does keygen (~2-5 min) and writes a multi-GB PK to disk
//! under `crates/bridge-prover-orchestrator/params/`. Subsequent runs reuse the
//! cache.

use std::path::PathBuf;

use bridge_prover_orchestrator::{
    generate_fallback_proof, generate_fallback_proof_with_transcript,
    halo2_tvm_bundle::TranscriptKind, verify_fallback_proof, verify_fallback_proof_with_transcript,
    FallbackKeyManager,
};

/// Where to cache SRS / VK / PK for this test run.
fn params_dir() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("params");
    p
}

#[test]
fn fallback_round_trip_10_signers() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let test_data =
        bridge_test_data_gen::generator::generate_test_data_fallback_all_sign(10).unwrap();
    let attestation_2_bytes = test_data
        .attestation_2_bytes
        .clone()
        .expect("fallback test data must have attestation_2_bytes");

    let mut km = FallbackKeyManager::new(&params_dir());
    km.ensure_keys(&test_data.bk_set).unwrap();

    // Pick last_seen = block_seq_no - 1 (must be strictly less than the value
    // embedded in the primary attestation). The synthetic generator produces a
    // known seq_no per call; we re-derive it by parsing the primary attestation
    // here.
    let block_seq_no = extract_block_seq_no_from_primary(&test_data.attestation_bytes);
    let last_seen = block_seq_no
        .checked_sub(1)
        .expect("synthetic test data should have block_seq_no >= 1");

    let proof = generate_fallback_proof(
        &km,
        &test_data.attestation_bytes,
        &attestation_2_bytes,
        &test_data.bk_set,
        last_seen,
    )
    .expect("fallback proof generation must succeed");

    assert_eq!(proof.block_seq_no, block_seq_no);
    assert_eq!(proof.last_seen_block_seqno, last_seen);
    assert!(!proof.proof_bytes.is_empty(), "proof must be non-empty");

    let instances = proof.instances();
    let ok = verify_fallback_proof(&km, &proof.proof_bytes, &instances);
    assert!(ok, "native fallback verification must succeed");

    // Negative test: tampering with the proof must reject.
    let mut tampered = proof.proof_bytes.clone();
    let mid = tampered.len() / 2;
    tampered[mid] ^= 0xFF;
    let ok_tampered = verify_fallback_proof(&km, &tampered, &instances);
    assert!(!ok_tampered, "tampered fallback proof must NOT verify");

    // Negative test: wrong public instances must reject.
    let mut wrong_instances = instances;
    wrong_instances[3] = wrong_instances[3] + bridge_prover_orchestrator::Fr::one();
    let ok_wrong = verify_fallback_proof(&km, &proof.proof_bytes, &wrong_instances);
    assert!(!ok_wrong, "wrong instances must NOT verify");

    println!(
        "OK: fallback round-trip succeeded (proof = {} bytes, bk_set = {} signers, block_seq_no = \
         {})",
        proof.proof_bytes.len(),
        test_data.bk_set.len(),
        block_seq_no
    );

    // --- Poseidon transcript round-trip (R15 / M3) -----------------------
    //
    // Reuses the cached VK/PK so we do NOT pay another keygen pass. The
    // transcript choice does not affect the verifying key — only how Fiat–
    // Shamir challenges are derived from the (proof, instances) stream.
    //
    // Acceptance criteria for M3:
    //   1. Poseidon proof verifies under the matching Poseidon verifier.
    //   2. Mixing transcripts (prove Poseidon → verify Blake2b, or vice versa)
    //      rejects — confirms the two paths are independent and the reader is
    //      actually using the chosen hash.
    //   3. Poseidon proof bytes differ from Blake2b proof bytes (otherwise we'd be
    //      silently using the same transcript under the hood).

    let proof_poseidon = generate_fallback_proof_with_transcript(
        &km,
        &test_data.attestation_bytes,
        &attestation_2_bytes,
        &test_data.bk_set,
        last_seen,
        TranscriptKind::Poseidon,
    )
    .expect("fallback proof generation (Poseidon) must succeed");

    let instances_p = proof_poseidon.instances();
    assert_eq!(
        instances_p, instances,
        "Poseidon transcript must produce identical public instances"
    );

    let ok_p = verify_fallback_proof_with_transcript(
        &km,
        &proof_poseidon.proof_bytes,
        &instances_p,
        TranscriptKind::Poseidon,
    );
    assert!(ok_p, "native Poseidon-transcript verification must succeed");

    let mismatched_a = verify_fallback_proof_with_transcript(
        &km,
        &proof_poseidon.proof_bytes,
        &instances_p,
        TranscriptKind::Blake2b,
    );
    assert!(
        !mismatched_a,
        "Poseidon proof must NOT verify under Blake2b transcript"
    );

    let mismatched_b = verify_fallback_proof_with_transcript(
        &km,
        &proof.proof_bytes,
        &instances,
        TranscriptKind::Poseidon,
    );
    assert!(
        !mismatched_b,
        "Blake2b proof must NOT verify under Poseidon transcript"
    );

    assert_ne!(
        proof.proof_bytes, proof_poseidon.proof_bytes,
        "Blake2b and Poseidon transcripts must produce distinct proof byte streams"
    );

    println!(
        "OK: Poseidon round-trip succeeded (proof = {} bytes, |Δ vs Blake2b| = {})",
        proof_poseidon.proof_bytes.len(),
        proof_poseidon.proof_bytes.len() as i64 - proof.proof_bytes.len() as i64
    );
}

/// Mirror of the private helper in `prover.rs` so the test can derive the
/// seq_no the generator embedded.
fn extract_block_seq_no_from_primary(attestation_bytes: &[u8]) -> u32 {
    use bridge_parsers::attestation_data_parser::{attestation_data_offset, parse_num_signers};
    const BLOCK_SEQ_NO_REL_OFFSET: usize = 80;

    let num_signers = parse_num_signers(attestation_bytes);
    let abs_offset = attestation_data_offset(num_signers) + BLOCK_SEQ_NO_REL_OFFSET;
    let seqno_bytes = &attestation_bytes[abs_offset..abs_offset + 4];
    u32::from_le_bytes(seqno_bytes.try_into().unwrap())
}
