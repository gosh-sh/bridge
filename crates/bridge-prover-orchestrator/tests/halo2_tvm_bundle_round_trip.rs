//! `Halo2TvmBundle` round-trip test.
//!
//! Validates the wire format we propose for the AN-side
//! `ZKHALO2VERIFYWITHVK` opcode (see `crate::halo2_tvm_bundle` and
//! `docs/zk_halo2_an_side_design.md`).
//!
//! ## What this test proves
//!
//! 1. **Producer side**: generate a real Halo2 SHPLONK proof (Blake2b
//!    transcript) for Circuit 1B (Fallback attestation).
//! 2. **Serialise**: pack `(BaseCircuitParams, VK, instances, proof)` into the
//!    [`Halo2TvmBundle`] byte layout.
//! 3. **Re-load**: deserialise the bundle from those bytes only — no in-memory
//!    shortcut, no key manager reference.
//! 4. **Verify**: reassemble `vk` + `instances` from the bundle and run
//!    `verify_proof::<KZG, VerifierSHPLONK, Blake2bRead, SingleStrategy>` using
//!    a freshly-loaded `ParamsKZG<Bn256>` (chain-wide shared SRS, Q-WIRE-2 in
//!    the design memo).
//! 5. **Negative cases**: a flipped proof byte and a wrong public input must
//!    both make the bundle-side verify return `Ok(false)`.
//!
//! If all assertions hold, every byte boundary the AN-side opcode would
//! ever touch is exercised on the producer side and we have a concrete
//! fixture for the partner team to review.
//!
//! ## Cost
//!
//! Reuses the `params/` cache populated by `fallback_round_trip` — first
//! invocation of either test does ~2-5 min keygen, this one is then
//! seconds (re-uses VK/PK from disk, only does prove+verify).

use std::path::PathBuf;

use bridge_prover_orchestrator::{
    generate_fallback_proof, FallbackKeyManager, Fr, Halo2TvmBundle, TranscriptKind,
};
use halo2_base::halo2_proofs::halo2curves::ff::PrimeField;

fn params_dir() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("params");
    p
}

#[test]
fn halo2_tvm_bundle_round_trip_fallback_circuit() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    // ---- Produce a real proof using the existing infrastructure. ----
    let test_data =
        bridge_test_data_gen::generator::generate_test_data_fallback_all_sign(10).unwrap();
    let attestation_2_bytes = test_data
        .attestation_2_bytes
        .clone()
        .expect("fallback test data must have attestation_2_bytes");

    let mut km = FallbackKeyManager::new(&params_dir());
    km.ensure_keys(&test_data.bk_set).unwrap();

    let block_seq_no = extract_block_seq_no(&test_data.attestation_bytes);
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

    let instances: Vec<Fr> = proof.instances().to_vec();

    // ---- Wire-format pack. ----
    let bundle =
        Halo2TvmBundle::from_native(km.config(), km.vk(), &instances, proof.proof_bytes.clone())
            .expect("bundle construction must succeed");

    assert_eq!(bundle.num_instances(), instances.len());
    assert_eq!(bundle.transcript, TranscriptKind::Blake2b);

    let mut bytes = Vec::new();
    bundle.write(&mut bytes).unwrap();

    // ---- Wire-format unpack. ----
    let recovered =
        Halo2TvmBundle::read(bytes.as_slice()).expect("bundle deserialisation must succeed");

    assert_eq!(recovered.num_instances(), instances.len());
    assert_eq!(recovered.transcript, TranscriptKind::Blake2b);
    assert_eq!(recovered.proof_bytes, proof.proof_bytes);
    // Decoded instances must match the originals bit-for-bit (strict 32-byte LE).
    let recovered_instances =
        bridge_prover_orchestrator::decode_instances(&recovered.instances_bytes)
            .expect("instances bytes must decode");
    assert_eq!(recovered_instances, instances);

    // ---- Verify from the bundle, using only the re-loaded contents. ----
    //
    // The SRS plays the role of the chain-wide shared trusted setup the
    // TVM opcode would look up by `k`. We just reuse the key manager's
    // SRS reference here (same on-disk file).
    let ok = recovered.verify(&km.srs).expect("verify must not panic");
    assert!(ok, "round-tripped bundle must verify");

    // ---- Negative test: flipped proof byte must reject (Ok(false)). ----
    let mut tampered_bundle = recovered.clone();
    let mid = tampered_bundle.proof_bytes.len() / 2;
    tampered_bundle.proof_bytes[mid] ^= 0xFF;
    let tampered_ok = tampered_bundle
        .verify(&km.srs)
        .expect("verify must not panic on tampered proof bytes");
    assert!(
        !tampered_ok,
        "bundle with flipped proof byte must NOT verify"
    );

    // ---- Negative test: bumped instance must reject (Ok(false)). ----
    let mut wrong_instances_bundle = recovered.clone();
    {
        // Bump instance[3] (= last_seen_block_seqno) by 1 in-place.
        let off = 3 * 32;
        let mut repr = <Fr as PrimeField>::Repr::default();
        repr.as_mut()
            .copy_from_slice(&wrong_instances_bundle.instances_bytes[off..off + 32]);
        let next = Fr::from_repr(repr).unwrap() + Fr::one();
        wrong_instances_bundle.instances_bytes[off..off + 32]
            .copy_from_slice(next.to_bytes().as_ref());
    }
    let wrong_ok = wrong_instances_bundle
        .verify(&km.srs)
        .expect("verify must not panic on wrong instances");
    assert!(
        !wrong_ok,
        "bundle with mutated public instance must NOT verify"
    );

    println!(
        "OK: Halo2TvmBundle round-trip succeeded (bundle size = {} B, vk = {} B, proof = {} B, \
         instances = {} × 32 B)",
        bytes.len(),
        recovered.vk_bytes.len(),
        recovered.proof_bytes.len(),
        recovered.num_instances(),
    );
}

fn extract_block_seq_no(attestation_bytes: &[u8]) -> u32 {
    use bridge_parsers::attestation_data_parser::{attestation_data_offset, parse_num_signers};
    const BLOCK_SEQ_NO_REL_OFFSET: usize = 80;

    let num_signers = parse_num_signers(attestation_bytes);
    let abs_offset = attestation_data_offset(num_signers) + BLOCK_SEQ_NO_REL_OFFSET;
    let seqno_bytes = &attestation_bytes[abs_offset..abs_offset + 4];
    u32::from_le_bytes(seqno_bytes.try_into().unwrap())
}
