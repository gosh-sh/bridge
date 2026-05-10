//! Phase 3.3 — Export a Circuit 2 (Layer Hashes Movement) proof + instances
//! to disk in the formats consumed by the gnark Groth16 wrapper
//! (`gnark-wrappers/circuit-2/`).
//!
//! Workflow:
//!   1. Load the cached `LayerHashesKeyManager` (SRS + VK + PK at K=17).
//!   2. Build a synthetic test input via
//!      `build_synthetic_layer_hashes_input(num_layers, num_chain_steps)`.
//!   3. Produce a layer-hashes proof.
//!   4. Write three artefacts side by side:
//!        - `<out_dir>/proof.bin`         — raw Halo2 SHPLONK proof bytes.
//!        - `<out_dir>/instances.bin`     — flat 32-byte LE Fr concatenation
//!                                          (14 elements).
//!        - `<out_dir>/halo2_proof.json`  — gnark-wrapper-friendly JSON.

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use tracing::info;

use bridge_prover_orchestrator::{
    build_synthetic_layer_hashes_input, generate_layer_hashes_proof,
    layer_hashes_keys::LayerHashesReferenceWitness,
    proof_export::{build_proof_data, save_instances_binary, save_proof_data_json},
    LayerHashesKeyManager, LayerHashesProofInput, LAYER_HASHES_K,
};

#[derive(Parser, Debug)]
#[command(
    name = "export-layer-hashes-proof",
    about = "Generate a Circuit 2 (Layer Hashes Movement) proof and write proof.bin + instances.bin + halo2_proof.json"
)]
struct Args {
    #[arg(long, default_value_t = default_params_dir())]
    params_dir: String,
    #[arg(long, default_value_t = default_out_dir())]
    out_dir: String,
    /// Number of active layer hashes (1..=10).
    #[arg(long, default_value_t = 1)]
    num_layers: usize,
    /// Number of active chain steps (≥1, num_chain_steps + 1 ≤ MAX_CHAIN_LEN=11).
    #[arg(long, default_value_t = 1)]
    num_chain_steps: usize,
}

fn default_params_dir() -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("params");
    p.to_string_lossy().into_owned()
}

fn default_out_dir() -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("proofs");
    p.push("layer-hashes");
    p.to_string_lossy().into_owned()
}

fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let args = Args::parse();
    let params_dir = PathBuf::from(&args.params_dir);
    let out_dir = PathBuf::from(&args.out_dir);
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("failed to create out_dir {:?}", out_dir))?;

    info!(
        ?params_dir, ?out_dir, num_layers = args.num_layers,
        num_chain_steps = args.num_chain_steps,
        "exporting layer-hashes proof"
    );

    let mut km = LayerHashesKeyManager::new(&params_dir);

    // Keygen reference uses the maximal-shape input so the cached VK / PK
    // works for every (num_layers, num_chain_steps) combination.
    let reference = build_synthetic_layer_hashes_input(10, 10);
    km.ensure_keys(&LayerHashesReferenceWitness {
        layer_hashes_preimage: reference.layer_hashes_preimage,
        merkle_siblings: reference.merkle_siblings,
        prev_max_level_layer_hash: reference.prev_max_level_layer_hash,
        num_prev_chain_steps: reference.num_prev_chain_steps,
        prev_chain_proofs: reference.prev_chain_proofs.clone(),
        bk_set_poseidon_hash: reference.bk_set_poseidon_hash,
    })
    .context("failed to ensure layer-hashes keys")?;

    let input_data = build_synthetic_layer_hashes_input(args.num_layers, args.num_chain_steps);

    let proof = generate_layer_hashes_proof(
        &km,
        LayerHashesProofInput {
            layer_hashes_preimage: input_data.layer_hashes_preimage,
            merkle_siblings: input_data.merkle_siblings,
            prev_max_level_layer_hash: input_data.prev_max_level_layer_hash,
            num_prev_chain_steps: input_data.num_prev_chain_steps,
            prev_chain_proofs: &input_data.prev_chain_proofs,
            bk_set_poseidon_hash: input_data.bk_set_poseidon_hash,
            expected_instances: input_data.expected_instances,
        },
    )
    .context("layer-hashes proof generation failed")?;

    let instances = proof.instances();

    let proof_bin_path = out_dir.join("proof.bin");
    let instances_bin_path = out_dir.join("instances.bin");
    let json_path = out_dir.join("halo2_proof.json");

    std::fs::write(&proof_bin_path, &proof.proof_bytes)
        .with_context(|| format!("failed to write {:?}", proof_bin_path))?;
    save_instances_binary(&instances, &instances_bin_path)?;
    let proof_data =
        build_proof_data(proof.proof_bytes.clone(), &instances, LAYER_HASHES_K);
    save_proof_data_json(&proof_data, &json_path)?;

    info!(
        proof_bytes = proof.proof_bytes.len(),
        instances = instances.len(),
        "wrote layer-hashes proof artefacts"
    );
    println!(
        "OK: wrote {} ({} bytes proof, {} public inputs, k = {})",
        out_dir.display(),
        proof.proof_bytes.len(),
        instances.len(),
        LAYER_HASHES_K
    );
    println!("    proof.bin            : {}", proof_bin_path.display());
    println!("    instances.bin        : {}", instances_bin_path.display());
    println!("    halo2_proof.json     : {}", json_path.display());
    Ok(())
}
