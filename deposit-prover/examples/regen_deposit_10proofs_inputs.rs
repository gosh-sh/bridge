//! Regenerate `fixtures/deposit_10proofs/proof_NN/input.json` from synthetic witnesses.
//!
//! ```bash
//! cargo run --release --example regen_deposit_10proofs_inputs -- \
//!   --set-dir fixtures/deposit_10proofs --count 10
//! ```

use std::{fs, path::Path};

use clap::Parser;
use deposit_prover::synthetic_deposit_proof_input;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value = "fixtures/deposit_10proofs")]
    set_dir: String,
    #[arg(long, default_value = "10")]
    count: usize,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    for i in 0..args.count {
        let dir = Path::new(&args.set_dir).join(format!("proof_{i:02}"));
        fs::create_dir_all(&dir)?;
        let input = synthetic_deposit_proof_input(i as u64);
        let json = serde_json::to_string_pretty(&input)?;
        let path = dir.join("input.json");
        fs::write(&path, json)?;
        println!("wrote {}", path.display());
    }
    Ok(())
}
