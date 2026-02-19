//! Test Circuit with Real Ethereum Data
//!
//! This tool tests the deposit prover circuit with real Ethereum data.
//!
//! Usage:
//!   # Test with MockProver only (fast)
//!   cargo run --example test_with_real_data -- --input
//! deposit_proof_input.json --mock-only
//!
//!   # Generate full SNARK proof
//!   cargo run --example test_with_real_data -- --input
//! deposit_proof_input.json --generate-proof --output proof_output.json

use std::fs;

use clap::Parser;
use deposit_prover::{
    prover::{generate_proof, test_circuit_mock, CircuitConfig},
    types::DepositProofInput,
};

#[derive(Parser, Debug)]
#[command(name = "test-with-real-data")]
#[command(about = "Test circuit with real Ethereum data", long_about = None)]
struct Args {
    /// Input file containing DepositProofInput (JSON)
    #[arg(long)]
    input: String,

    /// Only run MockProver test (skip proof generation)
    #[arg(long)]
    mock_only: bool,

    /// Generate full SNARK proof
    #[arg(long)]
    generate_proof: bool,

    /// Output file for proof (JSON)
    #[arg(long, default_value = "deposit_proof_output.json")]
    output: String,

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
    // FIX BC-TYPES-001: amount is now [u8; 32]
    println!("  Amount: 0x{}", hex::encode(input.event_data.amount));
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

    match test_circuit_mock(input.clone(), &config) {
        Ok(()) => {
            println!("\n✅ MockProver test PASSED!");
            println!("Circuit is satisfied with real Ethereum data!");
            println!();
        },
        Err(e) => {
            eprintln!("\n❌ MockProver test FAILED!");
            eprintln!("Circuit test failed: {}", e);
            eprintln!();
            eprintln!("Possible issues:");
            eprintln!("1. MPT proof is invalid or missing");
            eprintln!("2. Receipt RLP encoding is incorrect");
            eprintln!("3. Event data doesn't match the receipt");
            eprintln!("4. Circuit parameters are too small");
            std::process::exit(1);
        },
    }

    // If mock-only flag is set, stop here
    if args.mock_only {
        println!("Mock-only mode: Skipping proof generation");
        println!();
        println!("To generate a full SNARK proof, run:");
        println!(
            "  cargo run --example test_with_real_data -- --input {} --generate-proof",
            args.input
        );
        return Ok(());
    }

    // Generate proof if requested
    if args.generate_proof {
        println!("\n=== Generating SNARK Proof ===\n");
        println!("⚠️  This may take 5-10 minutes depending on your hardware...");
        println!();

        match generate_proof(input, &config) {
            Ok(proof_output) => {
                println!("\n✅ Proof generated successfully!");
                println!();
                println!("Public Outputs:");
                println!("  Deposit ID: {}", proof_output.deposit_id);
                println!("  Sender: 0x{}", hex::encode(&proof_output.sender));
                // FIX BC-TYPES-001: amount is now [u8; 32]
                println!("  Amount: 0x{}", hex::encode(proof_output.amount));
                println!(
                    "  Contract: 0x{}",
                    hex::encode(&proof_output.contract_address)
                );
                println!();

                // Save proof to file
                println!("Saving proof to {}...", args.output);
                let proof_json = serde_json::to_string_pretty(&proof_output)?;
                fs::write(&args.output, proof_json)?;
                println!("✓ Proof saved");
                println!();

                println!("Next steps:");
                println!("1. Submit withdrawal transaction with this proof");
                println!("2. Use the proof_bytes field for the withdraw() function");
            },
            Err(e) => {
                eprintln!("\n❌ Proof generation FAILED!");
                eprintln!("Error: {}", e);
                eprintln!();
                eprintln!("Possible issues:");
                eprintln!("1. Insufficient memory (16GB+ recommended)");
                eprintln!("2. KZG parameters not found or corrupted");
                eprintln!("3. Circuit configuration mismatch");
                std::process::exit(1);
            },
        }
    } else {
        println!("To generate a full SNARK proof, add --generate-proof flag");
    }

    Ok(())
}
