use std::fs;

use deposit_prover::types::DepositProofOutput;
use snark_verifier_sdk::Snark;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <proof_json_path>", args[0]);
        std::process::exit(1);
    }

    let proof_json_path = &args[1];

    // Read JSON file
    let json_str = fs::read_to_string(proof_json_path).expect("Failed to read proof JSON file");

    // Parse JSON
    let proof_output: DepositProofOutput =
        serde_json::from_str(&json_str).expect("Failed to parse JSON");

    // Deserialize SNARK
    let snark: Snark =
        bincode::deserialize(&proof_output.proof).expect("Failed to deserialize SNARK");

    // Print instances
    println!("SNARK instances:");
    println!("  Number of instance columns: {}", snark.instances.len());
    for (i, column) in snark.instances.iter().enumerate() {
        println!("  Column {}: {} instances", i, column.len());
        for (j, instance) in column.iter().enumerate() {
            println!("    Instance {}: {:?}", j, instance);
        }
    }
}
