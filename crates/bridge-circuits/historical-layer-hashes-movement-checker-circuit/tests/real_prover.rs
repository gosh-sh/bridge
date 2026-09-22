//! Real KZG prover tests for the layer-hashes-movement-checker circuit.
//!
//! Sweeps `num_chain_steps` and `num_layers` to measure proof gen/verify/size.
//! Fixed to production `HISTORY_PROOF_WINDOW_SIZE = 128` (Poseidon tree depth
//! 8). Smaller depths are no longer supported — they would change the
//! circuit shape (constraint count → different VK/PK), and we don't want
//! test code that exercises a shape we will never deploy.
//!
//! Synthetic input generation is delegated to
//! [`bridge_test_data_gen::layer_hashes::build_synthetic_layer_hashes_input`],
//! which carries the production tree depth as a hard-coded constant.

use std::fs;
use std::path::Path;
use std::time::Instant;

use bridge_test_data_gen::layer_hashes::{
    build_synthetic_layer_hashes_input, SyntheticLayerHashesInput, TREE_DEPTH,
};
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::halo2curves::bn256::{Bn256, Fr, G1Affine};
use halo2_base::halo2_proofs::plonk::{keygen_pk, keygen_vk, ProvingKey, VerifyingKey};
use halo2_base::halo2_proofs::poly::kzg::commitment::ParamsKZG;
use halo2_base::halo2_proofs::SerdeFormat;
use halo2_base::utils::fs::gen_srs;
use historical_layer_hashes_movement_checker_circuit::circuit::LayerHashesMovementCheckerCircuit;
use historical_layer_hashes_movement_checker_circuit::test_helpers::*;

use gosh_zk_snark_halo2_utils::io::{read_vk_from_path, save_config_params};
use gosh_zk_snark_halo2_utils::proof::Proof;

const CACHE_DIR: &str = "test_cache_real_prover";

// ── I/O helpers ─────────────────────────────────────────────────
//
// `gosh_zk_snark_halo2_utils` is used for `save_config_params`,
// `read_vk_from_path`, and proof create/verify. The local `write_vk` /
// `write_pk` wrappers stay because the library only exposes a combined
// `keygen::generate_and_save_keys` (which fuses keygen + save and would
// erase the per-step `keygen_vk` / `keygen_pk` timings reported here).

fn save_bytes(path: &str, data: &[u8]) {
    fs::write(path, data).unwrap();
}

fn write_vk(path: &str, vk: &VerifyingKey<G1Affine>) {
    let mut buf = Vec::new();
    vk.write(&mut buf, SerdeFormat::RawBytesUnchecked).unwrap();
    save_bytes(path, &buf);
}

fn write_pk(path: &str, pk: &ProvingKey<G1Affine>) {
    let mut buf = Vec::new();
    pk.write(&mut buf, SerdeFormat::RawBytesUnchecked).unwrap();
    save_bytes(path, &buf);
}

/// `Option`-returning config loader, used to drive cache-hit-vs-keygen
/// (library version panics on miss).
fn load_config(path: &str) -> Option<BaseCircuitParams> {
    let data = fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Materialize a `LayerHashesMovementCheckerCircuit` from a
/// `SyntheticLayerHashesInput` produced by `bridge_test_data_gen`.
fn circuit_from_input(input: &SyntheticLayerHashesInput) -> LayerHashesMovementCheckerCircuit {
    LayerHashesMovementCheckerCircuit::new(
        input.layer_hashes_preimage,
        input.merkle_siblings,
        input.prev_max_level_layer_hash,
        input.num_prev_chain_steps,
        input.prev_chain_proofs.clone(),
        input.bk_set_poseidon_hash,
        K as usize,
        NUM_UNUSABLE_ROWS,
        LOOKUP_BITS,
    )
}

/// Run keygen with a max-size reference circuit, write VK/PK/config to disk,
/// return `(VK, base_circuit_params)` for the prove loop.
fn keygen_and_cache(
    srs: &ParamsKZG<Bn256>,
    vk_path: &str,
    pk_path: &str,
    config_path: &str,
) -> (VerifyingKey<G1Affine>, BaseCircuitParams) {
    println!(
        "[KEYGEN] building reference circuit (10 layers, 10 steps, depth={})...",
        TREE_DEPTH
    );
    let ref_input = build_synthetic_layer_hashes_input(10, 10);
    let ref_circuit = circuit_from_input(&ref_input);
    let bp = ref_circuit.base_circuit_params().clone();
    save_config_params(&bp, config_path);
    println!("[KEYGEN] params: {:?}", bp);

    let t = Instant::now();
    let vk = keygen_vk(srs, &ref_circuit).unwrap();
    println!("[KEYGEN] VK in {:.1}s", t.elapsed().as_secs_f64());

    let t = Instant::now();
    let pk = keygen_pk(srs, vk.clone(), &ref_circuit).unwrap();
    println!("[KEYGEN] PK in {:.1}s\n", t.elapsed().as_secs_f64());

    write_vk(vk_path, &vk);
    write_pk(pk_path, &pk);
    (vk, bp)
}

// ── Test cases ──────────────────────────────────────────────────

const TEST_CASES: &[(usize, usize)] = &[
    (1, 1),   // minimal
    (3, 1),   // moderate layers, minimal chain
    (5, 3),   // moderate layers, moderate chain
    (10, 1),  // max layers, minimal chain
    (10, 5),  // max layers, moderate chain
    (10, 10), // max layers, max chain
];

#[test]
fn test_real_prover_layer_hashes_sweep() {
    let cache = format!(
        "{}/{}",
        env!("CARGO_MANIFEST_DIR"),
        CACHE_DIR
    );
    fs::create_dir_all(&cache).ok();

    let config_path = format!("{}/config.json", cache);
    let vk_path = format!("{}/vk.bin", cache);
    let pk_path = format!("{}/pk.bin", cache);

    // Step 1: SRS
    let t = Instant::now();
    let srs = gen_srs(K);
    println!("\n[SRS] {:.1}s\n", t.elapsed().as_secs_f64());

    // Step 2: Keygen (use max-size case)
    let cached_bp = load_config(&config_path);
    let (vk, base_params) = if let Some(bp) = cached_bp {
        if Path::new(&vk_path).exists() && Path::new(&pk_path).exists() {
            // Universal deserialization via `BaseCircuitBuilder<Fr>` — works
            // because `LayerHashesMovementCheckerCircuit::configure_with_params`
            // delegates to `BaseCircuitBuilder::configure_with_params`.
            let t = Instant::now();
            let vk = read_vk_from_path(&vk_path, &bp);
            println!("[CACHE] VK loaded in {:.1}s (PK on disk for prove)\n", t.elapsed().as_secs_f64());
            (vk, bp)
        } else {
            keygen_and_cache(&srs, &vk_path, &pk_path, &config_path)
        }
    } else {
        keygen_and_cache(&srs, &vk_path, &pk_path, &config_path)
    };

    // Step 3: Prove + verify sweep
    println!(
        "{:<12} {:<12} {:>10} {:>12} {:>10}",
        "layers", "steps", "prove(s)", "verify(ms)", "proof(B)"
    );
    println!("{}", "-".repeat(58));

    for &(nl, ns) in TEST_CASES {
        let input = build_synthetic_layer_hashes_input(nl, ns);
        let mut circuit = circuit_from_input(&input);
        circuit.override_base_circuit_params(base_params.clone());

        let instances: Vec<Fr> = input.expected_instances.to_vec();
        let inst_refs: Vec<&[Fr]> = vec![instances.as_slice()];

        let t = Instant::now();
        let pf = Proof::create_for_circuit_from_paths::<LayerHashesMovementCheckerCircuit>(
            &srs, &pk_path, &config_path, circuit, &inst_refs,
        );
        let pt = t.elapsed().as_secs_f64();

        let mut vt_total = 0.0;
        for _ in 0..3 {
            let t = Instant::now();
            assert!(pf.verify_with_vk(&vk, &srs, &inst_refs), "VERIFY FAILED: L={nl}, S={ns}");
            vt_total += t.elapsed().as_secs_f64();
        }
        let vt_ms = (vt_total / 3.0) * 1000.0;

        println!(
            "{:<12} {:<12} {:>10.1} {:>12.2} {:>10}",
            nl, ns, pt, vt_ms, pf.as_bytes().len()
        );
        save_bytes(&format!("{}/proof_L{}_S{}.bin", cache, nl, ns), pf.as_bytes());
    }

    println!("\nAll {} cases passed.\n", TEST_CASES.len());
}
