//! Real KZG prover test for the Fallback Attestation BLS Checker Circuit.
//!
//! Generates a single proving key / verification key, then creates and verifies
//! proofs for different BK set sizes (10, 100, 299) using the same key.
//! Reports timings for SRS generation, keygen, proof generation, and verification.
//! Caches SRS, VK, and PK to disk for faster re-runs.
//!
//! Run: `cargo test -p attestation-bls-checker-circuit --test real_prover_fallback -- --nocapture`

use std::path::Path;
use std::time::Instant;

use attestation_bls_checker_circuit::{
    fallback_circuit::FallbackAttestationBlsCheckerCircuit,
    test_instances::expected_public_instances,
    K, LOOKUP_BITS, NUM_UNUSABLE_ROWS,
};
use bridge_poseidon::{LIMB_BITS, MAX_SIGNERS, NUM_LIMBS};
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::{keygen_pk, keygen_vk, VerifyingKey},
    poly::kzg::commitment::ParamsKZG,
};
use halo2_base::halo2_proofs::halo2curves::ff::PrimeField;
use halo2_base::utils::fs::gen_srs;

use gosh_zk_snark_halo2_utils::io::{
    read_vk_from_path, save_bytes, save_config_params, save_pk_to_path, save_vk_to_path,
    try_read_config_params,
};
use gosh_zk_snark_halo2_utils::proof::Proof;

const ARTIFACT_DIR: &str = "params";

// ---------------------------------------------------------------------------
// Build a fallback circuit for a given BK set size
// ---------------------------------------------------------------------------

fn build_fallback_circuit(
    bk_set_size: usize,
    shared_params: Option<&BaseCircuitParams>,
) -> (
    FallbackAttestationBlsCheckerCircuit<Fr>,
    Vec<Fr>,
) {
    let test_data =
        bridge_test_data_gen::generator::generate_test_data_fallback_all_sign(bk_set_size)
            .expect("generate_test_data_fallback_all_sign failed");

    let (last_seen_block_seqno, instances) = expected_public_instances(
        &test_data.attestation_bytes,
        &test_data.bk_set,
        MAX_SIGNERS,
    );

    let mut circuit = FallbackAttestationBlsCheckerCircuit::<Fr>::new(
        test_data.attestation_bytes,
        test_data.attestation_2_bytes.unwrap(),
        test_data.bk_set,
        last_seen_block_seqno,
        K as usize,
        NUM_UNUSABLE_ROWS,
        LOOKUP_BITS,
        LIMB_BITS,
        NUM_LIMBS,
        MAX_SIGNERS,
    );

    if let Some(sp) = shared_params {
        circuit.override_base_circuit_params(sp.clone());
    }

    (circuit, instances)
}

/// Run keygen against a reference circuit, write VK/PK/config to disk, return
/// `(VK, base_circuit_params)` for use by the prove loop.
fn keygen_and_cache(
    params: &ParamsKZG<Bn256>,
    ref_bk_set_size: usize,
    vk_path: &str,
    pk_path: &str,
    config_path: &str,
) -> (VerifyingKey<G1Affine>, BaseCircuitParams) {
    println!("  Cache miss — running keygen (reference BK set size = {})", ref_bk_set_size);
    let t = Instant::now();
    let (ref_circuit, _) = build_fallback_circuit(ref_bk_set_size, None);
    let base_params = ref_circuit.params.base_circuit_params.clone();
    println!("  base_circuit_params: {:?}", base_params);
    println!("[timing] reference circuit construction: {:?}", t.elapsed());

    let t = Instant::now();
    let vk = keygen_vk(params, &ref_circuit).expect("keygen_vk failed");
    println!("[timing] keygen_vk: {:?}", t.elapsed());

    let t = Instant::now();
    let pk = keygen_pk(params, vk.clone(), &ref_circuit).expect("keygen_pk failed");
    println!("[timing] keygen_pk: {:?}", t.elapsed());

    let t = Instant::now();
    save_vk_to_path(&vk, vk_path);
    save_pk_to_path(&pk, pk_path);
    save_config_params(&base_params, config_path);
    println!("[timing] save VK + PK + config: {:?}", t.elapsed());
    println!("  Cached to {}/", ARTIFACT_DIR);

    (vk, base_params)
}

// ---------------------------------------------------------------------------
// Main test
// ---------------------------------------------------------------------------

/// Real K=20 fallback aggregator keygen + three proofs across the
/// `[10, 100, 299]` BK-set sweep. Same weight class as
/// `test_real_prover_primary_multi_bk_set` — on-demand only, gated
/// behind `#[ignore]` so per-MR CI doesn't OOM-kill the worker.
/// Trigger with `cargo test -- --ignored`.
#[test]
#[ignore]
fn test_real_prover_fallback_multi_bk_set() {
    let vk_path = format!("{}/fallback_vk.bin", ARTIFACT_DIR);
    let pk_path = format!("{}/fallback_pk.bin", ARTIFACT_DIR);
    let config_path = format!("{}/fallback_config_params.json", ARTIFACT_DIR);
    let bk_set_sizes = [10, 100, 299];

    // ── Step 1: SRS (cached by gen_srs) ───────────────────────────
    println!("\n{}", "=".repeat(60));
    println!("Step 1: SRS (K={})", K);
    let t = Instant::now();
    let params = gen_srs(K);
    println!("[timing] SRS load/gen: {:?}", t.elapsed());

    // ── Step 2: VK + PK (try cache, else keygen) ─────────────────
    println!("\nStep 2: VK + PK");

    let cached_config = try_read_config_params(&config_path);
    let (vk, base_params) = if let Some(cfg) = cached_config {
        if Path::new(&vk_path).exists() && Path::new(&pk_path).exists() {
            let vk = read_vk_from_path(&vk_path, &cfg);
            println!("  Loaded VK + config from cache (PK on disk for prove)");
            (vk, cfg)
        } else {
            keygen_and_cache(&params, bk_set_sizes[0], &vk_path, &pk_path, &config_path)
        }
    } else {
        keygen_and_cache(&params, bk_set_sizes[0], &vk_path, &pk_path, &config_path)
    };

    // ── Step 3: Prove + verify for each BK set size ──────────────
    for &bk_set_size in &bk_set_sizes {
        println!("\n{}", "=".repeat(60));
        println!("BK set size = {}", bk_set_size);
        println!("{}", "=".repeat(60));

        let t = Instant::now();
        let (circuit, instances) = build_fallback_circuit(bk_set_size, Some(&base_params));
        println!("[timing] circuit construction: {:?}", t.elapsed());

        let inst_refs: Vec<&[Fr]> = vec![instances.as_slice()];

        let t = Instant::now();
        let proof = Proof::create_for_circuit_from_paths::<FallbackAttestationBlsCheckerCircuit<Fr>>(
            &params,
            &pk_path,
            &config_path,
            circuit,
            &inst_refs,
        );
        let proof_time = t.elapsed();
        println!("[timing] proof generation: {:?}", proof_time);
        println!("  proof size: {} bytes", proof.as_bytes().len());

        save_bytes(
            &format!("{}/fallback_proof_bk{}.bin", ARTIFACT_DIR, bk_set_size),
            proof.as_bytes(),
        );

        let mut verify_times = Vec::new();
        for _ in 0..5 {
            let t = Instant::now();
            let valid = proof.verify_with_vk(&vk, &params, &inst_refs);
            verify_times.push(t.elapsed());
            assert!(valid, "Proof verification failed for bk_set_size={}", bk_set_size);
        }
        let avg_verify = verify_times.iter().sum::<std::time::Duration>() / 5;
        println!("[timing] verification (avg of 5): {:?}", avg_verify);

        let instances_bytes: Vec<u8> = instances
            .iter()
            .flat_map(|f| f.to_repr().as_ref().to_vec())
            .collect();
        save_bytes(
            &format!("{}/fallback_instances_bk{}.bin", ARTIFACT_DIR, bk_set_size),
            &instances_bytes,
        );
    }

    println!("\n{}", "=".repeat(60));
    println!("All fallback proofs generated and verified successfully!");
    println!("Artifacts saved to {}/", ARTIFACT_DIR);
}
