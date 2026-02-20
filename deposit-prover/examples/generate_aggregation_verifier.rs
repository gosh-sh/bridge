//! Generate Aggregation Verifier Contract
//!
//! This example generates a Solidity verifier contract for aggregated proofs.
//! The aggregation verifier is much smaller (~10-15KB) than the direct verifier
//! (~30KB) and fits under the 24KB Ethereum contract size limit.
//!
//! ## Workflow
//!
//! 1. Generate a deposit proof (using generate_verifier or test_with_real_data)
//! 2. Aggregate the proof (this creates the aggregation proving key)
//! 3. Generate the aggregation verifier (this example)
//!
//! ## Usage
//!
//! ```bash
//! # Step 1: Generate a deposit proof first
//! cargo run --release --example test_with_real_data -- \
//!   --input e2e_test_data/deposit_info.json \
//!   --generate-proof \
//!   --output deposit_proof_output.json
//!
//! # Step 2: Run this example to aggregate and generate verifier
//! cargo run --release --example generate_aggregation_verifier -- \
//!   --deposit-proof deposit_proof_output.json
//! ```
//!
//! The aggregation verifier will be saved to:
//! ../contracts/AggregationVerifier.sol

use std::fs;

use clap::Parser;
use deposit_prover::{
    aggregation::{aggregate_proof, generate_aggregation_verifier, AggregationConfig},
    types::DepositProofOutput,
};

#[derive(Parser, Debug)]
#[command(name = "generate-aggregation-verifier")]
#[command(about = "Generate aggregation verifier contract", long_about = None)]
struct Args {
    /// Path to deposit proof output JSON file
    #[arg(long, default_value = "deposit_proof_output.json")]
    deposit_proof: String,

    /// Aggregation circuit degree (default: 21)
    #[arg(long, default_value = "21")]
    degree: u32,

    /// Lookup bits (default: 19)
    #[arg(long, default_value = "19")]
    lookup_bits: usize,

    /// Output path for aggregation verifier
    #[arg(long, default_value = "../contracts/AggregationVerifier.sol")]
    output: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    println!("=== Deposit Prover: Aggregation Verifier Generator ===\n");

    // Configuration
    let config = AggregationConfig {
        degree: args.degree,
        lookup_bits: args.lookup_bits,
    };

    println!("Aggregation Configuration:");
    println!("  - Degree (k): {}", config.degree);
    println!("  - Lookup bits: {}", config.lookup_bits);
    println!();

    // Load deposit proof
    println!("Loading deposit proof from {}...", args.deposit_proof);
    let deposit_proof_json = fs::read_to_string(&args.deposit_proof)?;
    let deposit_proof: DepositProofOutput = serde_json::from_str(&deposit_proof_json)?;
    println!(
        "✓ Loaded deposit proof for depositId: {}",
        deposit_proof.deposit_id
    );
    println!();

    // Aggregate the proof
    println!("Step 1: Aggregating deposit proof...");
    println!("This will:");
    println!("  1. Create an aggregation circuit that verifies the deposit proof");
    println!("  2. Generate a proving key for the aggregation circuit");
    println!("  3. Generate an aggregated proof");
    println!();

    let _aggregated_proof = aggregate_proof(&deposit_proof, &config)?;
    println!("✓ Aggregation complete!");
    println!();

    // Generate aggregation verifier
    println!("Step 2: Generating aggregation verifier contract...");
    println!("This will create a Solidity verifier that can verify aggregated proofs.");
    println!();

    let verifier_size = generate_aggregation_verifier(&config, &args.output)?;

    println!();
    println!("=== Summary ===");
    println!();
    println!("✅ Aggregation verifier generated successfully!");
    println!();
    println!("Verifier Details:");
    println!("  - Path: {}", args.output);
    println!(
        "  - Size: {} bytes ({:.1} KB)",
        verifier_size,
        verifier_size as f64 / 1024.0
    );
    println!(
        "  - Fits under 24KB: {}",
        if verifier_size <= 24576 {
            "✅ YES"
        } else {
            "❌ NO"
        }
    );
    println!();

    if verifier_size > 24576 {
        println!("⚠️  WARNING: Verifier still exceeds 24KB limit!");
        println!();
        println!("Possible solutions:");
        println!("  1. Increase aggregation circuit degree (try --degree 22)");
        println!("  2. Aggregate multiple proofs together");
        println!("  3. Use a different proving system");
        println!();
    } else {
        println!("Next steps:");
        println!("  1. Review the generated verifier: {}", args.output);
        println!("  2. Deploy it to Ethereum using the deployment script");
        println!("  3. Update the bridge to use the aggregation verifier");
        println!();
        println!("Note: You'll need to modify the prover to generate aggregated proofs");
        println!("      instead of direct proofs for E2E testing.");
    }

    Ok(())
}
