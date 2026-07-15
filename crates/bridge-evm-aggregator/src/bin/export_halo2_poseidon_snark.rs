//! Wrap on-disk Halo2 VK + Poseidon proof + instances into bincode [`Snark`].
//!
//! ```bash
//! cargo run --release --bin export-halo2-poseidon-snark -- \
//!   --vk ../../params/primary_vk.bin \
//!   --config ../../params/primary_config_params.json \
//!   --proof ../../proofs/bound/poseidon-snark/primary.proof.bin \
//!   --instances ../../proofs/bound/primary/instances.bin \
//!   --out ../../proofs/bound/poseidon-snark/primary.snark \
//!   --num-instances 4
//! ```

use std::path::PathBuf;

use bridge_evm_aggregator::halo2_snark::export_poseidon_snark;
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
    #[arg(long)]
    num_instances: usize,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    export_poseidon_snark(
        &args.vk,
        &args.config,
        &args.proof,
        &args.instances,
        &args.out,
        args.num_instances,
    )?;
    println!(
        "OK: {} -> {} ({} B)",
        args.proof.display(),
        args.out.display(),
        std::fs::metadata(&args.out)?.len()
    );
    Ok(())
}
