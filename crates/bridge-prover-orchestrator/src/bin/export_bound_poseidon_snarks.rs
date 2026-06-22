//! Re-prove the bound block scenario with Poseidon transcript and emit
//! snark-verifier [`Snark`] files for the R15 aggregator pipeline.
//!
//! Instances are identical to the Blake2b export (reuse `instances.bin` from
//! `proofs/bound/{primary,fallback,layer-hashes}/`). Proof bytes differ
//! (Poseidon Fiat–Shamir).

use std::path::{Path, PathBuf};

use anyhow::Context;
use bridge_prover_lib::{
    keys::{KeyManager as PrimaryKeyManager},
};
use bridge_prover_orchestrator::{
    build_bound_test_data, compose_layer_hashes_input,
    generate_fallback_proof_with_transcript, generate_layer_hashes_proof_with_transcript,
    generate_primary_proof_with_transcript,
    halo2_tvm_bundle::TranscriptKind,
    layer_hashes_keys::{LayerHashesKeyManager, LayerHashesReferenceWitness},
    proof_export::load_instances_binary,
    FallbackKeyManager,
};
use clap::Parser;
use tracing::info;

#[derive(Parser, Debug)]
#[command(
    name = "export-bound-poseidon-snarks",
    about = "Poseidon re-prove of bound 1A/1B/2 + subprocess snark export for aggregator"
)]
struct Args {
    #[arg(long, default_value = "../../params")]
    params_dir: String,
    #[arg(long, default_value = "../../proofs/bound")]
    bound_dir: String,
    #[arg(long, default_value = "../../proofs/bound/poseidon-snark")]
    snark_dir: String,
    /// Path to built `export-halo2-poseidon-snark` (default: sibling aggregator target).
    #[arg(long)]
    snark_exporter: Option<PathBuf>,
    #[arg(long, default_value_t = 10)]
    signers: usize,
    #[arg(long, default_value_t = 5)]
    num_layers: usize,
    #[arg(long, default_value_t = 3)]
    num_chain_steps: usize,
}

struct CircuitExport<'a> {
    name: &'a str,
    vk_key: &'a str,
    config_key: &'a str,
    instances_rel: &'a str,
    num_instances: usize,
}

const CIRCUITS: &[CircuitExport<'static>] = &[
    CircuitExport {
        name: "primary",
        vk_key: "primary_vk.bin",
        config_key: "primary_config_params.json",
        instances_rel: "primary/instances.bin",
        num_instances: 4,
    },
    CircuitExport {
        name: "fallback",
        vk_key: "fallback_vk.bin",
        config_key: "fallback_config_params.json",
        instances_rel: "fallback/instances.bin",
        num_instances: 4,
    },
    CircuitExport {
        name: "layer_hashes",
        vk_key: "layer_hashes_vk.bin",
        config_key: "layer_hashes_config_params.json",
        instances_rel: "layer-hashes/instances.bin",
        num_instances: 14,
    },
];

fn default_snark_exporter() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../bridge-evm-aggregator/target/release/export-halo2-poseidon-snark")
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
    let bound_dir = PathBuf::from(&args.bound_dir);
    let snark_dir = PathBuf::from(&args.snark_dir);
    std::fs::create_dir_all(&snark_dir)?;

    let exporter = args.snark_exporter.unwrap_or_else(default_snark_exporter);
    if !exporter.is_file() {
        anyhow::bail!(
            "snark exporter not found at {} — build bridge-evm-aggregator first",
            exporter.display()
        );
    }

    let bound = build_bound_test_data(args.signers, args.num_layers, args.num_chain_steps, true)
        .context("build bound test data")?;

    // --- 1A Poseidon ---
    let mut primary_km = PrimaryKeyManager::new(&params_dir);
    primary_km.ensure_primary_keys(&bound.bk_set)?;
    primary_km.load_primary_pk()?;
    let primary = generate_primary_proof_with_transcript(
        &primary_km,
        &bound.attestation_primary_bytes,
        &bound.bk_set,
        bound.last_seen_block_seqno,
        TranscriptKind::Poseidon,
    )?;
    primary_km.unload_primary_pk();
    let primary_proof_path = snark_dir.join("primary.proof.bin");
    std::fs::write(&primary_proof_path, &primary.proof_bytes)?;

    // --- 1B Poseidon ---
    let fallback_bytes = bound
        .attestation_fallback_bytes
        .as_ref()
        .context("bound data missing fallback attestation")?;
    let mut fallback_km = FallbackKeyManager::new(&params_dir);
    fallback_km.ensure_keys(&bound.bk_set)?;
    let fallback = generate_fallback_proof_with_transcript(
        &fallback_km,
        &bound.attestation_primary_bytes,
        fallback_bytes,
        &bound.bk_set,
        bound.last_seen_block_seqno,
        TranscriptKind::Poseidon,
    )?;
    let fallback_proof_path = snark_dir.join("fallback.proof.bin");
    std::fs::write(&fallback_proof_path, &fallback.proof_bytes)?;

    // --- Circuit 2 Poseidon ---
    let mut layer_km = LayerHashesKeyManager::new(&params_dir);
    layer_km.ensure_keys(&LayerHashesReferenceWitness {
        layer_hashes_preimage: bound.layer_hashes_preimage,
        merkle_siblings: bound.merkle_siblings,
        prev_max_level_layer_hash: bound.prev_max_level_layer_hash,
        num_prev_chain_steps: bound.num_prev_chain_steps,
        prev_chain_proofs: bound.prev_chain_proofs.clone(),
        bk_set_poseidon_hash: bound.bk_set_poseidon_fr,
    })?;
    let layer = generate_layer_hashes_proof_with_transcript(
        &layer_km,
        compose_layer_hashes_input(&bound),
        TranscriptKind::Poseidon,
    )?;
    let layer_proof_path = snark_dir.join("layer_hashes.proof.bin");
    std::fs::write(&layer_proof_path, &layer.proof_bytes)?;

    let proof_paths = [
        ("primary", primary_proof_path),
        ("fallback", fallback_proof_path),
        ("layer_hashes", layer_proof_path),
    ];

    for spec in CIRCUITS {
        let proof_path = proof_paths
            .iter()
            .find(|(n, _)| *n == spec.name)
            .map(|(_, p)| p)
            .context("internal circuit list mismatch")?;
        let instances_path = bound_dir.join(spec.instances_rel);
        let out_snark = snark_dir.join(format!("{}.snark", spec.name));
        let vk_path = params_dir.join(spec.vk_key);
        let config_path = params_dir.join(spec.config_key);

        // Sanity: Poseidon instances match Blake2b export.
        let file_instances = load_instances_binary(&instances_path)?;
        if spec.name == "primary" {
            assert_eq!(file_instances.as_slice(), &primary.instances());
        }

        info!(circuit = spec.name, "exporting Poseidon Snark");
        run_snark_exporter(
            &exporter,
            &vk_path,
            &config_path,
            proof_path,
            &instances_path,
            &out_snark,
            spec.num_instances,
        )?;
        println!(
            "OK: {} -> {} ({} B)",
            spec.name,
            out_snark.display(),
            std::fs::metadata(&out_snark)?.len()
        );
    }

    println!("OK: Poseidon snarks in {}", snark_dir.display());
    Ok(())
}

fn run_snark_exporter(
    exporter: &Path,
    vk: &Path,
    config: &Path,
    proof: &Path,
    instances: &Path,
    out: &Path,
    num_instances: usize,
) -> anyhow::Result<()> {
    let status = std::process::Command::new(exporter)
        .arg("--vk")
        .arg(vk)
        .arg("--config")
        .arg(config)
        .arg("--proof")
        .arg(proof)
        .arg("--instances")
        .arg(instances)
        .arg("--out")
        .arg(out)
        .arg("--num-instances")
        .arg(num_instances.to_string())
        .status()
        .with_context(|| format!("run {}", exporter.display()))?;
    if !status.success() {
        anyhow::bail!("{} failed with {status}", exporter.display());
    }
    Ok(())
}
