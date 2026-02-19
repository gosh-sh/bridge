//! Analyze the structure of a Halo2 proof to understand its byte layout.
//!
//! This tool helps us understand how the 8224-byte proof is structured,
//! which is essential for parsing it in the gnark Groth16 circuit.

use clap::Parser;
use deposit_prover::groth16_wrapper::load_and_parse_snark;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the input .snark file
    #[arg(short, long, default_value = "data/deposit_proof_42.snark")]
    input: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    println!("========================================");
    println!("Halo2 Proof Structure Analyzer");
    println!("========================================\n");

    // Load the proof
    let proof_data = load_and_parse_snark(&args.input)?;

    println!("📊 Proof Overview");
    println!("─────────────────────────────────────────");
    println!("Total proof size: {} bytes", proof_data.proof_bytes.len());
    println!("Public inputs: {}", proof_data.public_inputs.len());
    println!("Domain k: {} (2^{} = {} rows)", 
        proof_data.protocol.k,
        proof_data.protocol.k,
        1u64 << proof_data.protocol.k
    );
    println!();

    println!("📋 Protocol Information");
    println!("─────────────────────────────────────────");
    println!("Instance columns: {:?}", proof_data.protocol.num_instance);
    println!("Witness columns: {:?}", proof_data.protocol.num_witness);
    println!("Challenge phases: {:?}", proof_data.protocol.num_challenge);
    println!("Preprocessed commitments: {}", proof_data.protocol.preprocessed_commitments.len());
    println!();

    println!("🔢 Public Inputs");
    println!("─────────────────────────────────────────");
    for (i, input) in proof_data.public_inputs.iter().enumerate() {
        println!("  [{}] {}", i, input);
    }
    println!();

    // Analyze proof byte structure
    println!("🔍 Proof Byte Structure Analysis");
    println!("─────────────────────────────────────────");
    
    let proof_bytes = &proof_data.proof_bytes;
    let total_size = proof_bytes.len();
    
    println!("Total size: {} bytes", total_size);
    println!();
    
    // G1 points are 64 bytes (32 bytes x, 32 bytes y) in compressed form
    // Field elements are 32 bytes
    
    // Try to identify structure based on common patterns
    println!("Estimated structure (based on typical Halo2/SHPLONK proofs):");
    println!();
    
    // Calculate expected sizes
    let num_advice_cols = proof_data.protocol.num_witness.iter().sum::<usize>();
    let num_instance_cols = proof_data.protocol.num_instance.iter().sum::<usize>();
    let num_challenges = proof_data.protocol.num_challenge.iter().sum::<usize>();
    
    println!("Circuit parameters:");
    println!("  - Advice columns: {}", num_advice_cols);
    println!("  - Instance columns: {}", num_instance_cols);
    println!("  - Challenge phases: {}", num_challenges);
    println!();
    
    // Typical SHPLONK proof structure:
    // 1. Advice commitments (one per advice column per phase)
    // 2. Permutation commitments (typically 1)
    // 3. Vanishing commitments (typically 1-3)
    // 4. Evaluations (multiple field elements)
    // 5. Multi-opening proof (2 G1 points for SHPLONK)
    
    println!("Expected components:");
    println!("  1. Advice commitments: ~{} × 64 bytes = {} bytes", 
        num_advice_cols, num_advice_cols * 64);
    println!("  2. Permutation commitments: ~1 × 64 bytes = 64 bytes");
    println!("  3. Vanishing commitments: ~3 × 64 bytes = 192 bytes");
    println!("  4. Evaluations: ~N × 32 bytes (variable)");
    println!("  5. Multi-opening proof: 2 × 64 bytes = 128 bytes");
    println!();
    
    // Try to parse the first few G1 points
    println!("First 256 bytes (hex):");
    for (i, chunk) in proof_bytes.iter().take(256).enumerate() {
        if i % 32 == 0 {
            print!("\n{:04x}: ", i);
        }
        print!("{:02x} ", chunk);
    }
    println!("\n");
    
    // Check if points are in compressed form (first byte indicates compression)
    println!("Compression analysis:");
    let mut offset = 0;
    let mut point_count = 0;
    while offset + 64 <= total_size && point_count < 10 {
        let first_byte = proof_bytes[offset];
        let compression_flag = first_byte & 0x80; // Check MSB
        println!("  Point {}: offset={}, first_byte=0x{:02x}, compressed={}", 
            point_count, offset, first_byte, compression_flag != 0);
        offset += 64;
        point_count += 1;
    }
    println!();
    
    // Calculate remaining bytes
    let estimated_commitments = (num_advice_cols + 1 + 3) * 64; // advice + perm + vanishing
    let estimated_opening = 128; // 2 G1 points
    let estimated_evaluations = total_size - estimated_commitments - estimated_opening;
    
    println!("Size breakdown estimate:");
    println!("  - Commitments: ~{} bytes", estimated_commitments);
    println!("  - Evaluations: ~{} bytes (~{} field elements)", 
        estimated_evaluations, estimated_evaluations / 32);
    println!("  - Opening proof: ~{} bytes", estimated_opening);
    println!("  - Total: {} bytes", total_size);
    println!();
    
    println!("⚠️  Note: This is an ESTIMATE based on typical SHPLONK proofs.");
    println!("    The actual structure depends on the specific circuit configuration.");
    println!("    To get the exact structure, we need to parse the proof using");
    println!("    snark-verifier's deserialization logic.");
    println!();
    
    println!("📚 Next Steps:");
    println!("─────────────────────────────────────────");
    println!("1. Study snark-verifier's proof encoding in Rust");
    println!("2. Implement matching decoder in Go");
    println!("3. Verify decoded structure matches expected format");
    println!("4. Use decoded components in gnark circuit");
    
    Ok(())
}

