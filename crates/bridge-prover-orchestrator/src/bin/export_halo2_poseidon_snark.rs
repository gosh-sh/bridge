//! CLI wrapper for [`bridge_prover_orchestrator::halo2_snark::export_poseidon_snark`].

use std::path::PathBuf;

use bridge_prover_orchestrator::halo2_snark::{export_poseidon_snark, load_instances_binary};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "export-halo2-poseidon-snark")]
struct Args {
    #[arg(long)]
    vk: PathBuf,
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    proof: PathBuf,
    #[arg(long)]
    instances: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let instances = load_instances_binary(&args.instances)?;
    export_poseidon_snark(
        &args.vk,
        &args.config,
        &args.proof,
        &instances,
        &args.out,
    )?;
    println!(
        "OK: {} -> {} ({} B)",
        args.proof.display(),
        args.out.display(),
        std::fs::metadata(&args.out)?.len()
    );
    Ok(())
}
