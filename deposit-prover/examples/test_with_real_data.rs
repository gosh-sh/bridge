//! Test Circuit with Real Ethereum Data
//!
//! This tool tests the deposit prover circuit with real Ethereum data.
//!
//! Usage:
//!   cargo run --example test_with_real_data -- --input deposit_proof_input.json

use clap::Parser;
use deposit_prover::{
    prover::{test_circuit_mock, CircuitConfig},
    types::DepositProofInput,
};
use std::fs;

#[derive(Parser, Debug)]
#[command(name = "test-with-real-data")]
#[command(about = "Test circuit with real Ethereum data", long_about = None)]
struct Args {
    /// Input file containing DepositProofInput (JSON)
    #[arg(long)]
    input: String,

    /// Circuit degree (default: 18)
    #[arg(long, default_value = "18")]
    degree: u32,

    /// Max data byte length (default: 256)
    #[arg(long, default_value = "256")]
    max_data_byte_len: usize,

    /// Max log number (default: 20)
    #[arg(long, default_value = "20")]
    max_log_num: usize,
}

fn main() -> anyhow::Result<()> {
    println!("=== Test Circuit with Real Data ===\n");

    let args = Args::parse();

    // Load input data
    println!("Loading input from {}...", args.input);
    let json = fs::read_to_string(&args.input)?;
    let input: DepositProofInput = serde_json::from_str(&json)?;
    println!("✓ Input loaded\n");

    println!("Event Data:");
    println!("  Block Number: {}", input.event_data.block_number);
    println!("  Deposit ID: {}", input.event_data.deposit_id);
    println!("  Sender: 0x{}", hex::encode(input.event_data.sender));
    println!("  Amount: {} wei", input.event_data.amount);
    println!();

    // Create circuit config
    let config = CircuitConfig {
        degree: args.degree,
        max_data_byte_len: args.max_data_byte_len,
        max_log_num: args.max_log_num,
        topic_num_bounds: (0, 4),
    };

    println!("Circuit Configuration:");
    println!("  Degree (k): {}", config.degree);
    println!("  Max data byte len: {}", config.max_data_byte_len);
    println!("  Max log num: {}", config.max_log_num);
    println!();

    // Test with MockProver
    println!("Testing circuit with MockProver...");
    println!("This may take a few minutes...\n");

    match test_circuit_mock(input, &config) {
        Ok(()) => {
            println!("\n✅ SUCCESS!");
            println!("Circuit test passed with real Ethereum data!");
            println!();
            println!("Next steps:");
            println!("1. Generate a full SNARK proof:");
            println!("   cargo run --example generate_proof -- --input {}", args.input);
            println!("2. Verify the proof:");
            println!("   cargo run --example verify_proof -- --proof deposit_proof.bin");
        }
        Err(e) => {
            eprintln!("\n❌ FAILED!");
            eprintln!("Circuit test failed: {}", e);
            eprintln!();
            eprintln!("Possible issues:");
            eprintln!("1. MPT proof is invalid or missing");
            eprintln!("2. Receipt RLP encoding is incorrect");
            eprintln!("3. Event data doesn't match the receipt");
            eprintln!("4. Circuit parameters are too small");
            std::process::exit(1);
        }
    }

    Ok(())
}

