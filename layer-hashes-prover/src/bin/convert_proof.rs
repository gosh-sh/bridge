use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use halo2_base::halo2_proofs::halo2curves::ff::PrimeField;

use layer_hashes_prover::proof_export::{build_proof_data, save_proof_data_json};

#[derive(Parser)]
#[command(name = "convert-proof")]
#[command(about = "Convert binary Halo2 proof + instances to gnark-wrapper JSON")]
struct Args {
    #[arg(long)]
    proof: PathBuf,

    #[arg(long)]
    instances: PathBuf,

    #[arg(long, default_value = "halo2_proof.json")]
    output: PathBuf,

    #[arg(long, default_value_t = 19)]
    k: u32,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let proof_bytes = std::fs::read(&args.proof)
        .with_context(|| format!("Failed to read proof file: {}", args.proof.display()))?;

    let instances_bytes = std::fs::read(&args.instances)
        .with_context(|| format!("Failed to read instances file: {}", args.instances.display()))?;

    let instances: Vec<Fr> = instances_bytes
        .chunks_exact(32)
        .map(|chunk| {
            let mut repr = <Fr as PrimeField>::Repr::default();
            repr.as_mut().copy_from_slice(chunk);
            Fr::from_repr(repr).expect("invalid Fr encoding in instances file")
        })
        .collect();

    eprintln!(
        "proof: {} bytes, instances: {} field elements",
        proof_bytes.len(),
        instances.len()
    );

    let proof_data = build_proof_data(proof_bytes, &instances, args.k);
    save_proof_data_json(&proof_data, &args.output)?;

    eprintln!("Wrote {}", args.output.display());
    Ok(())
}
