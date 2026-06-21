//! Phase 3.2 — Export a Circuit 1A (Primary attestation) proof + instances to
//! disk in the formats consumed by the gnark Groth16 wrapper
//! (`gnark-wrappers/circuit-1a/`).
//!
//! Mirrors `export_fallback_proof.rs` 1:1 but uses the partner's
//! `bridge_prover_lib::KeyManager` + `generate_primary_proof` for Circuit 1A.
//! Public-instance layout is identical to Circuit 1B's:
//! `[envelope_hash, bk_set_poseidon, block_seq_no, last_seen_block_seqno]`.
//!
//! Workflow:
//!   1. Load `bridge_prover_lib::KeyManager` from `params/` (re-uses the same
//!      SRS as our Phase 1.A FallbackKeyManager — both want K=20).
//!   2. Generate synthetic test data via
//!      `bridge_test_data_gen::generate_test_data_all_sign(N)`.
//!   3. Produce a primary proof via `generate_primary_proof`.
//!   4. Write `proof.bin`, `instances.bin`, `halo2_proof.json` side by side.
//!
//! Usage:
//! ```bash
//! cargo run --bin export-primary-proof --release -- \
//!     --params-dir crates/bridge-prover-orchestrator/params \
//!     --out-dir   crates/bridge-prover-orchestrator/proofs/primary \
//!     --signers 10
//! ```

use std::path::PathBuf;

use anyhow::Context;
use bridge_parsers::attestation_data_parser::{attestation_data_offset, parse_num_signers};
use bridge_prover_lib::{
    keys::{circuit_k, KeyManager},
    prover::generate_primary_proof,
    Fr,
};
use bridge_prover_orchestrator::proof_export::{
    build_proof_data, save_instances_binary, save_proof_data_json,
};
use clap::Parser;
use tracing::info;

#[derive(Parser, Debug)]
#[command(
    name = "export-primary-proof",
    about = "Generate a Circuit 1A (primary) proof and write proof.bin + instances.bin + \
             halo2_proof.json"
)]
struct Args {
    /// Where the partner's KeyManager looks for SRS / VK / PK / config.
    #[arg(long, default_value_t = default_params_dir())]
    params_dir: String,

    /// Where to write the three output artefacts.
    #[arg(long, default_value_t = default_out_dir())]
    out_dir: String,

    /// BK-set size for synthetic test data.
    #[arg(long, default_value_t = 10)]
    signers: usize,
}

fn default_params_dir() -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("params");
    p.to_string_lossy().into_owned()
}

fn default_out_dir() -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("proofs");
    p.push("primary");
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
        ?params_dir,
        ?out_dir,
        signers = args.signers,
        "exporting primary proof"
    );

    let mut km = KeyManager::new(&params_dir);

    let test_data = bridge_test_data_gen::generator::generate_test_data_all_sign(args.signers)
        .context("failed to generate synthetic primary test data")?;

    km.ensure_primary_keys(&test_data.bk_set)
        .context("failed to ensure primary keys")?;
    km.load_primary_pk()
        .context("failed to load primary PK")?;

    let block_seq_no = extract_block_seq_no(&test_data.attestation_bytes);
    let last_seen = block_seq_no
        .checked_sub(1)
        .context("synthetic block_seq_no must be >= 1")?;

    let proof = generate_primary_proof(
        &km,
        &test_data.attestation_bytes,
        &test_data.bk_set,
        last_seen,
    )
    .context("primary proof generation failed")?;

    let instances: [Fr; 4] = [
        proof.block_id_fr,
        proof.bk_set_commitment_fr,
        Fr::from(proof.block_seq_no as u64),
        Fr::from(proof.last_seen_block_seqno as u64),
    ];

    let proof_bin_path = out_dir.join("proof.bin");
    let instances_bin_path = out_dir.join("instances.bin");
    let json_path = out_dir.join("halo2_proof.json");

    std::fs::write(&proof_bin_path, &proof.proof_bytes)
        .with_context(|| format!("failed to write {:?}", proof_bin_path))?;
    save_instances_binary(&instances, &instances_bin_path)?;
    let proof_data = build_proof_data(proof.proof_bytes.clone(), &instances, circuit_k());
    save_proof_data_json(&proof_data, &json_path)?;

    info!(
        proof_bytes = proof.proof_bytes.len(),
        block_seq_no = proof.block_seq_no,
        last_seen_block_seqno = proof.last_seen_block_seqno,
        instances = instances.len(),
        "wrote primary proof artefacts"
    );
    println!(
        "OK: wrote {} ({} bytes proof, {} public inputs)",
        out_dir.display(),
        proof.proof_bytes.len(),
        instances.len()
    );
    println!("    proof.bin            : {}", proof_bin_path.display());
    println!(
        "    instances.bin        : {}",
        instances_bin_path.display()
    );
    println!("    halo2_proof.json     : {}", json_path.display());
    Ok(())
}

/// Mirror of bridge_prover_lib::prover's private helper.
fn extract_block_seq_no(attestation_bytes: &[u8]) -> u32 {
    const BLOCK_SEQ_NO_REL_OFFSET: usize = 80;
    let num_signers = parse_num_signers(attestation_bytes);
    let abs_offset = attestation_data_offset(num_signers) + BLOCK_SEQ_NO_REL_OFFSET;
    let seqno_bytes = &attestation_bytes[abs_offset..abs_offset + 4];
    u32::from_le_bytes(seqno_bytes.try_into().unwrap())
}
