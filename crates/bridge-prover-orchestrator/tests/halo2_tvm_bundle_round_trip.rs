//! `Halo2TvmOperands` round-trip test.
//!
//! Validates the **3-operand** wire format consumed by the AN-side
//! `ZKHALO2VERIFYWITHVK` opcode (see `crate::halo2_tvm_bundle` and
//! `docs/zkhalo2verifywithvk_reference.md`).
//!
//! ## What this test proves
//!
//! 1. **Producer side**: generate a real Halo2 SHPLONK proof (Blake2b
//!    transcript) for Circuit 1B (Fallback attestation).
//! 2. **Pack as three operands**: build a [`VkBlob`] for the `vk_cell`, encode
//!    public inputs as raw `N × 32` LE `Fr`, keep the proof bytes raw.
//! 3. **Round-trip**: serialise the `VkBlob` to bytes, parse it back via
//!    [`VkBlob::read`], decode public inputs via
//!    [`bridge_prover_orchestrator::decode_instances`].
//! 4. **Verify**: reassemble `vk` + `instances` and run `verify_proof::<KZG,
//!    VerifierSHPLONK, Blake2bRead, SingleStrategy>` using the freshly-loaded
//!    `ParamsKZG<Bn256>` (chain-wide shared SRS, Q-WIRE-2 in the design memo).
//! 5. **Negative cases**: a flipped proof byte and a wrong public input must
//!    both make the operand-side verify return `Ok(false)`.
//!
//! If all assertions hold, every byte boundary the AN-side opcode
//! would touch is exercised on the producer side and we have a
//! concrete fixture for the partner team to review.
//!
//! ## Cost
//!
//! First invocation does ~2-5 min Circuit 1B keygen into `params/`;
//! subsequent runs re-use the cached VK/PK from disk and only do
//! prove+verify (seconds). Keys come from `bridge-prover-lib`'s
//! `FallbackKeyManager` (K=21 SHPLONK-production degree).

use std::path::PathBuf;

use bridge_prover_lib::{keys::KeyManager, prover::generate_fallback_proof};
use bridge_prover_orchestrator::{Fr, Halo2TvmOperands, TranscriptKind, VkBlob};
use halo2_base::halo2_proofs::halo2curves::ff::PrimeField;

fn params_dir() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("params");
    p
}

#[test]
fn halo2_tvm_operands_round_trip_fallback_circuit() {
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

    let mut km = KeyManager::new(&params_dir());
    km.ensure_fallback_keys(&test_data.bk_set).unwrap();
    km.load_fallback_pk().unwrap();

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

    // Circuit 1B public instances `[block_id, bk_set_poseidon, block_seq_no,
    // last_seen]`, reconstructed from the prover-lib `ProofOutput`.
    let instances: Vec<Fr> = vec![
        proof.block_id_fr,
        proof.bk_set_commitment_fr,
        Fr::from(proof.block_seq_no as u64),
        Fr::from(proof.last_seen_block_seqno as u64),
    ];

    // ---- Pack as three opcode operands. ----
    let operands = Halo2TvmOperands::from_native(
        km.fallback_config(),
        km.fallback_vk(),
        &instances,
        proof.proof_bytes.clone(),
    )
    .expect("operand bundle construction must succeed");

    assert_eq!(operands.num_instances(), instances.len());
    assert_eq!(operands.public_inputs.len(), instances.len() * 32);
    assert_eq!(operands.proof, proof.proof_bytes);

    // ---- Round-trip the VkBlob via its serialised bytes. ----
    let blob =
        VkBlob::read(operands.vk_blob.as_slice()).expect("VkBlob deserialisation must succeed");
    assert_eq!(blob.transcript, TranscriptKind::Blake2b);
    // Re-emit the VkBlob and confirm byte-stable round-trip.
    let reemitted = blob.to_bytes().unwrap();
    assert_eq!(
        reemitted, operands.vk_blob,
        "VkBlob round-trip must be byte-stable"
    );

    // Decoded public inputs must match the originals bit-for-bit
    // (strict 32-byte LE).
    let recovered_instances = bridge_prover_orchestrator::decode_instances(&operands.public_inputs)
        .expect("public inputs must decode");
    assert_eq!(recovered_instances, instances);

    // ---- Verify from the operands, using only their byte payloads. ----
    //
    // The SRS plays the role of the chain-wide shared trusted setup
    // the TVM opcode would look up by `k`. Circuit 1B keygens at K=21
    // via `FallbackKeyManager`, so reuse that manager's degree-matched
    // slice (same on-disk `kzg_bn254_21.srs`).
    let srs = km.fallback.srs();
    let ok = operands.verify(srs).expect("verify must not panic");
    assert!(ok, "round-tripped operand bundle must verify");

    // ---- Negative test: flipped proof byte must reject (Ok(false)). ----
    let mut tampered = operands.clone();
    let mid = tampered.proof.len() / 2;
    tampered.proof[mid] ^= 0xFF;
    let tampered_ok = tampered
        .verify(srs)
        .expect("verify must not panic on tampered proof bytes");
    assert!(
        !tampered_ok,
        "operands with flipped proof byte must NOT verify"
    );

    // ---- Negative test: bumped instance must reject (Ok(false)). ----
    let mut wrong = operands.clone();
    {
        // Bump instance[3] (= last_seen_block_seqno) by 1 in-place.
        let off = 3 * 32;
        let mut repr = <Fr as PrimeField>::Repr::default();
        repr.as_mut()
            .copy_from_slice(&wrong.public_inputs[off..off + 32]);
        let next = Fr::from_repr(repr).unwrap() + Fr::one();
        wrong.public_inputs[off..off + 32].copy_from_slice(next.to_bytes().as_ref());
    }
    let wrong_ok = wrong
        .verify(srs)
        .expect("verify must not panic on wrong instances");
    assert!(
        !wrong_ok,
        "operands with mutated public instance must NOT verify"
    );

    // ---- Optional fixture export. ----
    //
    // When `EXPORT_HALO2_FIXTURE_DIR=/some/path` is set in the
    // environment, dump the three operand byte streams as separate
    // files so they can be checked in as TVM fixtures (e.g. in
    // `tvm-sdk/tvm_vm/halo2_test_data/fallback_*`) and consumed by
    // `test_halo2_with_vk.rs`. The files are written exactly as they
    // appear on the wire — no extra framing.
    if let Ok(dir) = std::env::var("EXPORT_HALO2_FIXTURE_DIR") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).expect("creating fixture export dir");
        std::fs::write(dir.join("fallback_vk_blob.bin"), &operands.vk_blob).unwrap();
        std::fs::write(
            dir.join("fallback_public_inputs.bin"),
            &operands.public_inputs,
        )
        .unwrap();
        std::fs::write(dir.join("fallback_proof.bin"), &operands.proof).unwrap();
        // Mirror the on-disk VK + config the KeyManager just wrote (same bytes
        // the tvm-sdk ABI tests reassemble into a VkBlob).
        let params = params_dir();
        for name in [
            "fallback_vk.bin",
            "fallback_config_params.json",
        ] {
            std::fs::copy(params.join(name), dir.join(name))
                .unwrap_or_else(|e| panic!("copy {name}: {e}"));
        }
        println!(
            "Exported Circuit 1B Hermez fixture to {} (vk_blob={}B proof={}B k={})",
            dir.display(),
            operands.vk_blob.len(),
            operands.proof.len(),
            km.fallback_config().k,
        );
    }

    println!(
        "OK: Halo2TvmOperands round-trip succeeded (vk_blob = {} B, public_inputs = {} × 32 B, \
         proof = {} B)",
        operands.vk_blob.len(),
        operands.num_instances(),
        operands.proof.len(),
    );
}

fn extract_block_seq_no(attestation_bytes: &[u8]) -> u32 {
    use attestation_bls_checker_circuit::attestation_data_parser::{attestation_data_offset, parse_num_signers};
    const BLOCK_SEQ_NO_REL_OFFSET: usize = 80;

    let num_signers = parse_num_signers(attestation_bytes);
    let abs_offset = attestation_data_offset(num_signers) + BLOCK_SEQ_NO_REL_OFFSET;
    let seqno_bytes = &attestation_bytes[abs_offset..abs_offset + 4];
    u32::from_le_bytes(seqno_bytes.try_into().unwrap())
}
