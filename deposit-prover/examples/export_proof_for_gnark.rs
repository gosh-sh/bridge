//! Export Halo2 proof to JSON format for gnark Groth16 wrapper.
//!
//! This example loads a Halo2 proof from a .snark file and exports it to JSON
//! format that can be consumed by the gnark Groth16 wrapper circuit.
//!
//! Usage:
//! ```bash
//! cargo run --example export_proof_for_gnark -- \
//!     --input data/deposit_proof_42.snark \
//!     --output gnark-wrapper/halo2_proof.json
//! ```

use clap::Parser;
use deposit_prover::groth16_wrapper::{load_and_parse_snark, save_proof_data_json};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the input .snark file
    #[arg(short, long, default_value = "data/deposit_proof_42.snark")]
    input: PathBuf,

    /// Path to the output JSON file
    #[arg(short, long, default_value = "gnark-wrapper/halo2_proof.json")]
    output: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    println!("Loading Halo2 proof from: {}", args.input.display());
    
    // Load and parse the Snark
    let proof_data = load_and_parse_snark(&args.input)?;
    
    println!("✓ Proof loaded successfully");
    println!("  - Public inputs: {}", proof_data.public_inputs.len());
    println!("  - Domain k: {}", proof_data.protocol.k);
    println!("  - Proof size: {} bytes", proof_data.proof_bytes.len());
    println!("  - Preprocessed commitments: {}", proof_data.protocol.preprocessed_commitments.len());
    
    // Save to JSON
    println!("\nSaving to JSON: {}", args.output.display());
    save_proof_data_json(&proof_data, &args.output)?;
    
    println!("✓ Proof exported successfully!");
    println!("\nNext steps:");
    println!("1. cd gnark-wrapper");
    println!("2. go run main.go");
    
    Ok(())
}

