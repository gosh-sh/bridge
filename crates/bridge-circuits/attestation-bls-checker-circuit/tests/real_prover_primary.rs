//! Real KZG prover test for the Primary Attestation BLS Checker Circuit.
//!
//! The default `test_real_prover_primary_multi_bk_set` test uses
//! `MAX_SIGNERS = 300` and generates a single proving key / verification
//! key, then creates and verifies proofs for several BK set sizes
//! (10, 100, 299) using the same keys — i.e. it asserts that one VK/PK
//! works across BK set sizes that share the same `max_signers` padding.
//!
//! The `#[ignore]` `test_real_prover_primary_max_{500,1000,2000}` tests
//! exercise larger `max_signers` paddings. Each gets its own VK/PK
//! (because `max_signers` changes `BaseCircuitParams`) and only proves a
//! single BK set size matching `max_signers` — they're sizing checks, not
//! universality checks.
//!
//! Reports timings for SRS generation, keygen, proof generation, and
//! verification. Caches SRS, VK, and PK to disk for faster re-runs;
//! artifact filenames are suffixed with `max_signers` so the caches don't
//! collide.
//!
//! Run:
//! ```text
//! cargo test -p attestation-bls-checker-circuit --test real_prover_primary -- --nocapture
//! cargo test -p attestation-bls-checker-circuit --test real_prover_primary -- --ignored --nocapture
//! ```

use std::path::Path;
use std::time::Instant;

use attestation_bls_checker_circuit::{
    primary_circuit::PrimaryAttestationBlsCheckerCircuit,
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
// Build a circuit for a given BK set size + max_signers padding
// ---------------------------------------------------------------------------

fn build_circuit_for_bk_set(
    bk_set_size: usize,
    max_signers: usize,
    shared_params: Option<&BaseCircuitParams>,
) -> (
    PrimaryAttestationBlsCheckerCircuit<Fr>,
    Vec<Fr>,
) {
    let test_data = bridge_test_data_gen::generator::generate_test_data_all_sign(bk_set_size)
        .expect("generate_test_data_all_sign failed");

    let (last_seen_block_seqno, instances) = expected_public_instances(
        &test_data.attestation_bytes,
        &test_data.bk_set,
        max_signers,
    );

    let mut circuit = PrimaryAttestationBlsCheckerCircuit::<Fr>::new(
        test_data.attestation_bytes,
        test_data.bk_set,
        last_seen_block_seqno,
        K as usize,
        NUM_UNUSABLE_ROWS,
        LOOKUP_BITS,
        LIMB_BITS,
        NUM_LIMBS,
        max_signers,
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
    max_signers: usize,
    vk_path: &str,
    pk_path: &str,
    config_path: &str,
) -> (VerifyingKey<G1Affine>, BaseCircuitParams) {
    println!("  Cache miss — running keygen (reference BK set size = {})", ref_bk_set_size);
    let t = Instant::now();
    let (ref_circuit, _) = build_circuit_for_bk_set(ref_bk_set_size, max_signers, None);
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
// Parameterised runner: SRS → keygen-or-load → prove + verify ×5 per size
// ---------------------------------------------------------------------------

/// Drive a full real-prover run for one `max_signers` padding and a list of
/// BK set sizes (each ≤ `max_signers`). Artifact filenames are suffixed
/// with `max_signers` so different paddings cache independently.
fn run_primary_real_prover_case(max_signers: usize, bk_set_sizes: &[usize]) {
    let vk_path = format!("{}/primary_max{}_vk.bin", ARTIFACT_DIR, max_signers);
    let pk_path = format!("{}/primary_max{}_pk.bin", ARTIFACT_DIR, max_signers);
    let config_path =
        format!("{}/primary_max{}_config_params.json", ARTIFACT_DIR, max_signers);

    println!("\n{}", "=".repeat(60));
    println!(
        "Primary real-prover — max_signers = {}, bk_set_sizes = {:?}",
        max_signers, bk_set_sizes
    );

    // ── Step 1: SRS (cached by gen_srs) ───────────────────────────
    println!("Step 1: SRS (K={})", K);
    let t = Instant::now();
    let params = gen_srs(K);
    println!("[timing] SRS load/gen: {:?}", t.elapsed());

    // ── Step 2: VK + PK (try cache, else keygen) ─────────────────
    println!("\nStep 2: VK + PK");

    let cached_config = try_read_config_params(&config_path);
    let (vk, base_params) = if let Some(cfg) = cached_config {
        if Path::new(&vk_path).exists() && Path::new(&pk_path).exists() {
            // Universal deser via `BaseCircuitBuilder<Fr>` — see utils crate.
            let vk = read_vk_from_path(&vk_path, &cfg);
            println!("  Loaded VK + config from cache (PK on disk for prove)");
            (vk, cfg)
        } else {
            keygen_and_cache(
                &params,
                bk_set_sizes[0],
                max_signers,
                &vk_path,
                &pk_path,
                &config_path,
            )
        }
    } else {
        keygen_and_cache(
            &params,
            bk_set_sizes[0],
            max_signers,
            &vk_path,
            &pk_path,
            &config_path,
        )
    };

    // ── Step 3: Prove + verify for each BK set size ──────────────
    for &bk_set_size in bk_set_sizes {
        println!("\n{}", "=".repeat(60));
        println!("BK set size = {} (max_signers = {})", bk_set_size, max_signers);
        println!("{}", "=".repeat(60));

        let t = Instant::now();
        let (circuit, instances) =
            build_circuit_for_bk_set(bk_set_size, max_signers, Some(&base_params));
        println!("[timing] circuit construction: {:?}", t.elapsed());

        let inst_refs: Vec<&[Fr]> = vec![instances.as_slice()];

        let t = Instant::now();
        let proof = Proof::create_for_circuit_from_paths::<PrimaryAttestationBlsCheckerCircuit<Fr>>(
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
            &format!(
                "{}/primary_max{}_proof_bk{}.bin",
                ARTIFACT_DIR, max_signers, bk_set_size
            ),
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
            &format!(
                "{}/primary_max{}_instances_bk{}.bin",
                ARTIFACT_DIR, max_signers, bk_set_size
            ),
            &instances_bytes,
        );
    }

    println!("\n{}", "=".repeat(60));
    println!("All proofs generated and verified successfully!");
    println!("Artifacts saved to {}/", ARTIFACT_DIR);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Universality check at `MAX_SIGNERS = 300`: one VK/PK proves across
/// three BK set sizes.
#[test]
fn test_real_prover_primary_multi_bk_set() {
    run_primary_real_prover_case(MAX_SIGNERS, &[10, 100, 299, 300]);
}

#[test]
fn test_real_prover_primary_max_300() {
    run_primary_real_prover_case(MAX_SIGNERS, &[MAX_SIGNERS]);
}

/// Sizing check at `max_signers = 500`. Each `max_signers` jump grows
/// `BaseCircuitParams` (advice/lookup columns) and therefore the PK
/// footprint and keygen time, so these are gated behind `#[ignore]`.
#[test]
#[ignore]
fn test_real_prover_primary_max_500() {
    run_primary_real_prover_case(500, &[500]);
}

#[test]
#[ignore]
fn test_real_prover_primary_max_1000() {
    run_primary_real_prover_case(1000, &[1000]);
}

#[test]
#[ignore]
fn test_real_prover_primary_max_2000() {
    run_primary_real_prover_case(2000, &[2000]);
}
