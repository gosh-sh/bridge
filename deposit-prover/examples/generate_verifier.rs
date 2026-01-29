//! Generate Solidity Verifier Contract
//!
//! This example generates a Solidity verifier contract that can verify
//! deposit proofs on-chain.
//!
//! Usage:
//!   cargo run --example generate_verifier
//!
//! The verifier will be saved to: ../contracts/DepositVerifier.sol

use deposit_prover::prover::{generate_solidity_verifier, CircuitConfig};
use std::path::Path;

fn main() {
    println!("=== Deposit Prover: Solidity Verifier Generator ===\n");

    // Use default circuit configuration
    let config = CircuitConfig::default();
    
    println!("Circuit Configuration:");
    println!("  - Degree (k): {}", config.degree);
    println!("  - Max data byte length: {}", config.max_data_byte_len);
    println!("  - Max log number: {}", config.max_log_num);
    println!("  - Topic number bounds: {:?}", config.topic_num_bounds);
    println!();

    // Output path for the Solidity verifier
    let output_path = Path::new("../contracts/DepositVerifier.sol");
    
    println!("Generating Solidity verifier...");
    println!("This may take several minutes on first run (generating proving key)...\n");

    match generate_solidity_verifier(&config, output_path) {
        Ok(()) => {
            println!("\n✅ SUCCESS!");
            println!("Solidity verifier contract generated at: {:?}", output_path);
            println!("\nNext steps:");
            println!("1. Review the generated contract");
            println!("2. Deploy it to Ethereum using Hardhat/Foundry");
            println!("3. Update AckiNackiBridge.sol to use the verifier address");
        }
        Err(e) => {
            eprintln!("\n❌ ERROR: {}", e);
            std::process::exit(1);
        }
    }
}

