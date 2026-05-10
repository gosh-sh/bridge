//! Phase 4 — Export a *cross-circuit-bound* block scenario.
//!
//! Drives one synthetic AN block scenario via
//! [`bridge_prover_orchestrator::build_bound_test_data`] and emits proofs for
//! both:
//!  - **Circuit 1A** (Primary attestation, K=20) — 4 public inputs.
//!  - **Circuit 2** (Layer hashes movement, K=17) — 14 public inputs.
//!
//! Both proofs share `block_id` (public input 0) and `bk_set_poseidon`
//! (public input 1) by construction, so the on-chain `AckiNackiBridge.verifyBlock`
//! cross-circuit consistency checks pass.
//!
//! Output layout:
//! ```text
//!   <out_dir>/primary/proof.bin
//!   <out_dir>/primary/instances.bin
//!   <out_dir>/primary/halo2_proof.json
//!   <out_dir>/layer-hashes/proof.bin
//!   <out_dir>/layer-hashes/instances.bin
//!   <out_dir>/layer-hashes/halo2_proof.json
//!   <out_dir>/bound_scenario.json    -- summary metadata
//! ```
//!
//! Re-uses the cached SRS/VK/PK on disk under `--params-dir`. First run does
//! keygen if absent; subsequent runs are pure prove-and-write.

use std::path::PathBuf;

use anyhow::Context;
use bridge_prover_lib::keys::{circuit_k as primary_k, KeyManager as PrimaryKeyManager};
use bridge_prover_lib::prover::generate_primary_proof;
use clap::Parser;
use serde::Serialize;
use tracing::info;

use bridge_prover_orchestrator::{
    build_bound_test_data, compose_layer_hashes_input, format_field_element,
    generate_layer_hashes_proof,
    layer_hashes_keys::LayerHashesReferenceWitness,
    proof_export::{build_proof_data, save_instances_binary, save_proof_data_json},
    BoundBlockTestData, Fr, LayerHashesKeyManager, LAYER_HASHES_K,
};

#[derive(Parser, Debug)]
#[command(
    name = "export-bound-block-proofs",
    about = "Generate bound Circuit 1A (Primary) + Circuit 2 (Layer Hashes) proofs sharing a block_id and bk_set_poseidon"
)]
struct Args {
    #[arg(long, default_value_t = default_params_dir())]
    params_dir: String,
    #[arg(long, default_value_t = default_out_dir())]
    out_dir: String,
    /// BK-set size (>= 2 — partner generator constraint).
    #[arg(long, default_value_t = 10)]
    signers: usize,
    /// Number of active layer slots (1..=10).
    #[arg(long, default_value_t = 5)]
    num_layers: usize,
    /// Number of active prev-chain steps (>= 1, num_chain_steps + 1 <= MAX_CHAIN_LEN = 11).
    #[arg(long, default_value_t = 3)]
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
    p.push("bound");
    p.to_string_lossy().into_owned()
}

#[derive(Serialize)]
struct BoundScenario {
    bk_set_size: usize,
    num_layers: usize,
    num_chain_steps: usize,
    block_seq_no: u32,
    last_seen_block_seqno: u32,
    block_id_decimal: String,
    bk_set_poseidon_decimal: String,
    layer_hash_decimals: Vec<String>,
    prev_max_level_layer_hash_decimal: String,
    primary_proof_bytes: usize,
    layer_hashes_proof_bytes: usize,
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
    let primary_dir = out_dir.join("primary");
    let layer_dir = out_dir.join("layer-hashes");
    std::fs::create_dir_all(&primary_dir)
        .with_context(|| format!("failed to create {:?}", primary_dir))?;
    std::fs::create_dir_all(&layer_dir).with_context(|| format!("failed to create {:?}", layer_dir))?;

    info!(
        ?params_dir, ?out_dir, signers = args.signers,
        num_layers = args.num_layers, num_chain_steps = args.num_chain_steps,
        "exporting bound block proofs (Circuit 1A + Circuit 2)"
    );

    // ------------------------------------------------------------------
    // 1. Bound scenario.
    // ------------------------------------------------------------------
    let bound = build_bound_test_data(
        args.signers,
        args.num_layers,
        args.num_chain_steps,
        false, // fallback not needed for Phase 4 binary; export-fallback-proof handles 1B.
    )
    .context("building bound test data failed")?;

    let primary_instances = bound.attestation_instances();
    let layer_instances = bound.layer_hashes_instances();

    info!(
        block_id = %format_field_element(&bound.block_id_fr),
        bk_set_poseidon = %format_field_element(&bound.bk_set_poseidon_fr),
        block_seq_no = bound.block_seq_no,
        "bound scenario assembled"
    );

    // ------------------------------------------------------------------
    // 2. Circuit 1A (Primary attestation) — K=20.
    // ------------------------------------------------------------------
    let mut primary_km = PrimaryKeyManager::new(&params_dir);
    primary_km
        .ensure_primary_keys(&bound.bk_set)
        .context("ensure_primary_keys failed")?;

    let primary_proof = generate_primary_proof(
        &primary_km,
        &bound.attestation_primary_bytes,
        &bound.bk_set,
        bound.last_seen_block_seqno,
    )
    .context("generate_primary_proof failed")?;

    // Sanity: the prover's internal Fr instances must match what we computed
    // from the bound data (both derive from the same attestation bytes).
    debug_assert_eq!(primary_proof.block_id_fr, bound.block_id_fr);
    debug_assert_eq!(primary_proof.bk_set_commitment_fr, bound.bk_set_poseidon_fr);

    write_proof_artefacts(
        &primary_dir,
        &primary_proof.proof_bytes,
        &primary_instances,
        primary_k(),
    )?;
    info!(
        bytes = primary_proof.proof_bytes.len(),
        "wrote Circuit 1A proof"
    );

    // ------------------------------------------------------------------
    // 3. Circuit 2 (Layer hashes movement) — K=17.
    // ------------------------------------------------------------------
    let mut layer_km = LayerHashesKeyManager::new(&params_dir);
    layer_km
        .ensure_keys(&LayerHashesReferenceWitness {
            layer_hashes_preimage: bound.layer_hashes_preimage,
            merkle_siblings: bound.merkle_siblings,
            prev_max_level_layer_hash: bound.prev_max_level_layer_hash,
            num_prev_chain_steps: bound.num_prev_chain_steps,
            prev_chain_proofs: bound.prev_chain_proofs.clone(),
            bk_set_poseidon_hash: bound.bk_set_poseidon_fr,
        })
        .context("ensure layer-hashes keys failed")?;

    let layer_proof = generate_layer_hashes_proof(&layer_km, compose_layer_hashes_input(&bound))
        .context("generate_layer_hashes_proof failed")?;

    write_proof_artefacts(&layer_dir, &layer_proof.proof_bytes, &layer_instances, LAYER_HASHES_K)?;
    info!(
        bytes = layer_proof.proof_bytes.len(),
        "wrote Circuit 2 proof"
    );

    // ------------------------------------------------------------------
    // 4. Summary metadata (JSON, useful for Foundry fixture extraction).
    // ------------------------------------------------------------------
    let scenario = BoundScenario {
        bk_set_size: args.signers,
        num_layers: bound.num_layers as usize,
        num_chain_steps: bound.num_prev_chain_steps as usize,
        block_seq_no: bound.block_seq_no,
        last_seen_block_seqno: bound.last_seen_block_seqno,
        block_id_decimal: format_field_element(&bound.block_id_fr),
        bk_set_poseidon_decimal: format_field_element(&bound.bk_set_poseidon_fr),
        layer_hash_decimals: bound
            .layer_hash_frs
            .iter()
            .map(format_field_element)
            .collect(),
        prev_max_level_layer_hash_decimal: format_field_element(&bound.prev_max_level_layer_hash),
        primary_proof_bytes: primary_proof.proof_bytes.len(),
        layer_hashes_proof_bytes: layer_proof.proof_bytes.len(),
    };
    let scenario_path = out_dir.join("bound_scenario.json");
    std::fs::write(&scenario_path, serde_json::to_string_pretty(&scenario)?)
        .with_context(|| format!("failed to write {:?}", scenario_path))?;

    println!("OK: bound scenario written");
    println!("    block_id (dec)        = {}", scenario.block_id_decimal);
    println!("    bk_set_poseidon (dec) = {}", scenario.bk_set_poseidon_decimal);
    println!("    block_seq_no          = {}", scenario.block_seq_no);
    println!("    primary proof bytes   = {}", scenario.primary_proof_bytes);
    println!("    layer-hashes proof bytes = {}", scenario.layer_hashes_proof_bytes);
    println!("    primary  artefacts -> {}", primary_dir.display());
    println!("    layer-h. artefacts -> {}", layer_dir.display());
    println!("    summary             -> {}", scenario_path.display());

    Ok(())
}

fn write_proof_artefacts(
    dir: &std::path::Path,
    proof_bytes: &[u8],
    instances: &[Fr],
    k: u32,
) -> anyhow::Result<()> {
    let proof_path = dir.join("proof.bin");
    let instances_path = dir.join("instances.bin");
    let json_path = dir.join("halo2_proof.json");

    std::fs::write(&proof_path, proof_bytes)
        .with_context(|| format!("failed to write {:?}", proof_path))?;
    save_instances_binary(instances, &instances_path)?;
    let proof_data = build_proof_data(proof_bytes.to_vec(), instances, k);
    save_proof_data_json(&proof_data, &json_path)?;
    Ok(())
}

// Suppress unused-import warning when bound test data is reused.
#[allow(dead_code)]
fn _force_bound_in_scope(_b: BoundBlockTestData) {}
