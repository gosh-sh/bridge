//! Halo2 proof parser for gnark Groth16 wrapper.
//!
//! This module parses the Snark struct from snark-verifier-sdk and extracts
//! the proof components needed for verification in a gnark Groth16 circuit.

use std::{
    fs::File,
    io::{BufReader, BufWriter, Write},
    path::Path,
};

use halo2_base::halo2_proofs::halo2curves::bn256::{Fr, G1Affine};
use serde::{Deserialize, Serialize};
use snark_verifier_sdk::Snark;

/// Parsed Halo2 proof data in a format suitable for gnark consumption.
///
/// This struct contains all the information needed by the gnark Groth16 circuit
/// to verify a Halo2/PLONK proof.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Halo2ProofData {
    /// Public inputs (instances) - for deposit circuit, this is 7 field
    /// elements
    pub public_inputs: Vec<String>,

    /// The raw proof bytes (contains commitments and evaluations)
    pub proof_bytes: Vec<u8>,

    /// Protocol information (verification key data)
    pub protocol: ProtocolData,
}

/// Protocol data extracted from snark-verifier's Protocol struct.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProtocolData {
    /// Domain size (k value, where domain size = 2^k)
    pub k: u32,

    /// Number of public inputs per instance column
    pub num_instance: Vec<usize>,

    /// Number of witness columns
    pub num_witness: Vec<usize>,

    /// Number of challenges
    pub num_challenge: Vec<usize>,

    /// Preprocessed commitments (verification key commitments)
    /// Serialized as hex strings for JSON compatibility
    pub preprocessed_commitments: Vec<String>,
}

/// Parse a Snark from snark-verifier-sdk into Halo2ProofData for gnark.
///
/// # Arguments
/// * `snark` - The Snark struct containing protocol, instances, and proof
///
/// # Returns
/// * `Halo2ProofData` - Parsed proof data ready for gnark consumption
pub fn parse_snark_for_gnark(snark: &Snark) -> Halo2ProofData {
    let protocol = &snark.protocol;
    let instances = &snark.instances;
    let proof = &snark.proof;
    // Extract public inputs (flatten all instance columns)
    let public_inputs: Vec<String> = instances
        .iter()
        .flat_map(|column| column.iter())
        .map(|fr| format_field_element(fr))
        .collect();

    // Extract protocol data
    let protocol_data = ProtocolData {
        k: protocol.domain.k as u32,
        num_instance: protocol.num_instance.clone(),
        num_witness: protocol.num_witness.clone(),
        num_challenge: protocol.num_challenge.clone(),
        preprocessed_commitments: protocol
            .preprocessed
            .iter()
            .map(|commitment| format_g1_affine(commitment))
            .collect(),
    };

    Halo2ProofData {
        public_inputs,
        proof_bytes: proof.to_vec(),
        protocol: protocol_data,
    }
}

/// Format a field element as a decimal string for gnark.
///
/// gnark expects field elements as decimal strings (not hex).
fn format_field_element(fr: &Fr) -> String {
    // Convert Fr to bytes (little-endian)
    let bytes = fr.to_bytes();

    // Convert to big integer and then to decimal string
    let value = num_bigint::BigUint::from_bytes_le(&bytes);
    value.to_str_radix(10)
}

/// Format a G1Affine point as a hex string for gnark.
///
/// Format: "0x<x_hex>,0x<y_hex>" where x and y are the affine coordinates.
/// We serialize the point using serde and then convert to hex.
fn format_g1_affine(point: &G1Affine) -> String {
    // Serialize the point to bytes using bincode
    let bytes = bincode::serialize(point).unwrap_or_default();
    hex::encode(bytes)
}

/// Load a Snark from a bincode file and parse it for gnark.
///
/// # Arguments
/// * `snark_path` - Path to the .snark file (bincode serialized)
///
/// # Returns
/// * `Result<Halo2ProofData>` - Parsed proof data or error
pub fn load_and_parse_snark(snark_path: impl AsRef<Path>) -> anyhow::Result<Halo2ProofData> {
    // Load the Snark from file
    let file = File::open(snark_path.as_ref())?;
    let reader = BufReader::new(file);
    let snark: Snark = bincode::deserialize_from(reader)?;

    // Parse for gnark
    Ok(parse_snark_for_gnark(&snark))
}

/// Save Halo2ProofData to a JSON file for gnark consumption.
///
/// # Arguments
/// * `proof_data` - The parsed proof data
/// * `output_path` - Path to save the JSON file
pub fn save_proof_data_json(
    proof_data: &Halo2ProofData,
    output_path: impl AsRef<Path>,
) -> anyhow::Result<()> {
    let file = File::create(output_path.as_ref())?;
    let mut writer = BufWriter::new(file);

    serde_json::to_writer_pretty(&mut writer, proof_data)?;
    writer.flush()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use halo2_base::halo2_proofs::halo2curves::bn256::Fr;

    use super::*;

    #[test]
    fn test_format_field_element() {
        // Test with zero
        let zero = Fr::zero();
        assert_eq!(format_field_element(&zero), "0");

        // Test with one
        let one = Fr::one();
        assert_eq!(format_field_element(&one), "1");
    }

    #[test]
    fn test_load_existing_proof() {
        // Test loading the existing deposit_proof_42.snark
        let proof_path = "data/deposit_proof_42.snark";

        if std::path::Path::new(proof_path).exists() {
            let result = load_and_parse_snark(proof_path);
            assert!(result.is_ok(), "Failed to load proof: {:?}", result.err());

            let proof_data = result.unwrap();

            // Verify we have 7 public inputs (depositId, sender, amount, contract_address,
            // block_hash_high, block_hash_low, promise_commit)
            assert_eq!(
                proof_data.public_inputs.len(),
                7,
                "Expected 7 public inputs"
            );

            // Verify protocol data
            assert_eq!(
                proof_data.protocol.num_instance,
                vec![7],
                "Expected num_instance = [7]"
            );

            println!(
                "Successfully parsed proof with {} public inputs",
                proof_data.public_inputs.len()
            );
            println!("Domain k: {}", proof_data.protocol.k);
            println!("Proof size: {} bytes", proof_data.proof_bytes.len());
        } else {
            println!("Skipping test - proof file not found: {}", proof_path);
        }
    }
}
