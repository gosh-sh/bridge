//! Detailed proof parser that extracts all components from a Halo2 proof.
//!
//! This example loads a Halo2 proof and parses it to extract:
//! - Witness commitments (grouped by phase)
//! - Quotient commitments
//! - Evaluations
//! - SHPLONK opening proof (W, W')
//!
//! The goal is to understand the exact structure so we can implement
//! verification in gnark.

use std::{fs::File, io::BufReader};

use halo2_base::halo2_proofs::{
    arithmetic::Field,
    halo2curves::{
        bn256::{Fr, G1Affine},
        group::ff::PrimeField,
    },
};
use snark_verifier_sdk::Snark;

fn main() -> anyhow::Result<()> {
    let proof_path = "data/deposit_proof_42.snark";

    println!("Loading proof from: {}", proof_path);

    // Load the Snark
    let file = File::open(proof_path)?;
    let reader = BufReader::new(file);
    let snark: Snark = bincode::deserialize_from(reader)?;

    println!("\n=== PROTOCOL INFO ===");
    println!("Domain k: {}", snark.protocol.domain.k);
    println!(
        "Domain size: 2^{} = {}",
        snark.protocol.domain.k,
        1 << snark.protocol.domain.k
    );
    println!("num_instance: {:?}", snark.protocol.num_instance);
    println!("num_witness: {:?}", snark.protocol.num_witness);
    println!("num_challenge: {:?}", snark.protocol.num_challenge);
    println!(
        "Preprocessed commitments: {}",
        snark.protocol.preprocessed.len()
    );

    // Calculate expected sizes
    let total_witness: usize = snark.protocol.num_witness.iter().sum();
    let total_challenges: usize = snark.protocol.num_challenge.iter().sum();

    println!("\nTotal witness columns: {}", total_witness);
    println!("Total challenges: {}", total_challenges);

    println!("\n=== PUBLIC INPUTS ===");
    for (i, instance_column) in snark.instances.iter().enumerate() {
        println!("Instance column {}: {} values", i, instance_column.len());
        for (j, value) in instance_column.iter().enumerate() {
            println!("  [{}]: {}", j, format_fr(value));
        }
    }

    println!("\n=== PROOF BYTES ===");
    println!("Total proof size: {} bytes", snark.proof.len());

    // Try to parse the proof structure
    // Based on snark-verifier's PlonkProof::read(), the proof contains:
    // 1. Witness commitments (grouped by phase)
    // 2. Quotient commitments
    // 3. Evaluations
    // 4. SHPLONK proof (W, W')

    let mut offset = 0;

    // Parse witness commitments
    println!("\n=== WITNESS COMMITMENTS ===");
    for (phase, &num_witness) in snark.protocol.num_witness.iter().enumerate() {
        println!("Phase {}: {} commitments", phase, num_witness);
        for i in 0..num_witness {
            if offset + 64 <= snark.proof.len() {
                let point = parse_g1_point(&snark.proof[offset..offset + 64]);
                println!(
                    "  Witness[{}][{}] @ offset {}: {}",
                    phase,
                    i,
                    offset,
                    format_g1(&point)
                );
                offset += 64;
            } else {
                println!("  ERROR: Not enough bytes for witness commitment");
                break;
            }
        }
    }

    // Parse quotient commitments
    // The number of quotient chunks depends on the circuit degree
    // For degree k, we typically have ceil((k+1)/k) = 2 or 3 chunks
    let num_quotient_chunks = estimate_quotient_chunks(snark.protocol.domain.k as u32);
    println!("\n=== QUOTIENT COMMITMENTS ===");
    println!("Estimated quotient chunks: {}", num_quotient_chunks);
    for i in 0..num_quotient_chunks {
        if offset + 64 <= snark.proof.len() {
            let point = parse_g1_point(&snark.proof[offset..offset + 64]);
            println!(
                "  Quotient[{}] @ offset {}: {}",
                i,
                offset,
                format_g1(&point)
            );
            offset += 64;
        } else {
            println!("  ERROR: Not enough bytes for quotient commitment");
            break;
        }
    }

    // Parse evaluations
    println!("\n=== EVALUATIONS ===");
    let remaining_bytes = snark.proof.len() - offset - 128; // Reserve 128 bytes for W and W'
    let num_evaluations = remaining_bytes / 32;
    println!(
        "Estimated evaluations: {} (remaining {} bytes)",
        num_evaluations, remaining_bytes
    );

    for i in 0..num_evaluations.min(10) {
        // Show first 10
        if offset + 32 <= snark.proof.len() - 128 {
            let eval = parse_fr(&snark.proof[offset..offset + 32]);
            println!("  Eval[{}] @ offset {}: {}", i, offset, format_fr(&eval));
            offset += 32;
        }
    }

    if num_evaluations > 10 {
        println!("  ... ({} more evaluations)", num_evaluations - 10);
        offset += (num_evaluations - 10) * 32;
    }

    // Parse SHPLONK proof (W, W')
    println!("\n=== SHPLONK OPENING PROOF ===");
    if offset + 64 <= snark.proof.len() {
        let w = parse_g1_point(&snark.proof[offset..offset + 64]);
        println!("W @ offset {}: {}", offset, format_g1(&w));
        offset += 64;
    }

    if offset + 64 <= snark.proof.len() {
        let w_prime = parse_g1_point(&snark.proof[offset..offset + 64]);
        println!("W' @ offset {}: {}", offset, format_g1(&w_prime));
        offset += 64;
    }

    println!("\nTotal bytes parsed: {}", offset);
    println!("Proof size: {}", snark.proof.len());

    if offset != snark.proof.len() {
        println!(
            "WARNING: Mismatch! {} bytes unaccounted for",
            snark.proof.len() - offset
        );
    } else {
        println!("✓ All bytes accounted for!");
    }

    Ok(())
}

fn estimate_quotient_chunks(_k: u32) -> usize {
    // For PLONK, the quotient polynomial degree is roughly (n-1) + (n-1) + (n-1) =
    // 3n-3 where n = 2^k. We split this into chunks of degree n-1.
    // So we need ceil(3n-3 / (n-1)) = 3 chunks typically.
    3
}

fn parse_g1_point(bytes: &[u8]) -> G1Affine {
    // Try to deserialize as G1Affine
    // Note: This might fail if the encoding is different
    bincode::deserialize(bytes).unwrap_or_else(|_| G1Affine::generator())
}

fn parse_fr(bytes: &[u8]) -> Fr {
    // Try to deserialize as Fr
    bincode::deserialize(bytes).unwrap_or_else(|_| Fr::ZERO)
}

fn format_fr(fr: &Fr) -> String {
    let bytes = fr.to_repr();
    let value = num_bigint::BigUint::from_bytes_le(bytes.as_ref());
    let s = value.to_str_radix(10);
    if s.len() > 20 {
        format!("{}...{}", &s[..10], &s[s.len() - 10..])
    } else {
        s
    }
}

fn format_g1(point: &G1Affine) -> String {
    let bytes = bincode::serialize(point).unwrap_or_default();
    if bytes.len() >= 8 {
        format!("G1({:?}...)", hex::encode(&bytes[..8]))
    } else {
        "G1(?)".to_string()
    }
}
