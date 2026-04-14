use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};

use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use serde::{Deserialize, Serialize};

/// Proof data in the JSON format expected by the gnark Groth16 wrapper.
///
/// Field names and types match the deposit-prover's `Halo2ProofData` for
/// compatibility with the gnark-wrapper Go code.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Halo2ProofData {
    pub public_inputs: Vec<String>,
    pub proof_bytes: Vec<u8>,
    pub protocol: ProtocolData,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProtocolData {
    pub k: u32,
    pub num_instance: Vec<usize>,
    pub num_witness: Vec<usize>,
    pub num_challenge: Vec<usize>,
    pub preprocessed_commitments: Vec<String>,
}

/// Format a BN254 Fr element as a decimal string (gnark convention).
pub fn format_field_element(fr: &Fr) -> String {
    let bytes = fr.to_bytes();
    let value = num_bigint::BigUint::from_bytes_le(&bytes);
    value.to_str_radix(10)
}

/// Build `Halo2ProofData` from raw proof bytes, instances, and circuit metadata.
pub fn build_proof_data(
    proof_bytes: Vec<u8>,
    instances: &[Fr],
    k: u32,
) -> Halo2ProofData {
    let public_inputs: Vec<String> = instances.iter().map(format_field_element).collect();

    let protocol = ProtocolData {
        k,
        num_instance: vec![instances.len()],
        // Placeholder values — the gnark wrapper's stub Define() only uses
        // public_inputs and k. These fields are reserved for future full
        // in-circuit Halo2 verification.
        num_witness: vec![],
        num_challenge: vec![],
        preprocessed_commitments: vec![],
    };

    Halo2ProofData {
        public_inputs,
        proof_bytes,
        protocol,
    }
}

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
