//! Proof + instances export utilities for the gnark Groth16 wrapping pipeline.
//!
//! Mirrors `layer_hashes_prover::proof_export` byte-for-byte: same JSON field
//! names and types, so a single Go gnark-wrapper template ingests both layer-
//! hash proofs and Circuit 1B (fallback attestation) proofs.

use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};

use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use halo2_base::halo2_proofs::halo2curves::ff::PrimeField;
use serde::{Deserialize, Serialize};

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

/// Build [`Halo2ProofData`] from raw proof bytes, public instances, and the
/// circuit's K parameter.
pub fn build_proof_data(proof_bytes: Vec<u8>, instances: &[Fr], k: u32) -> Halo2ProofData {
    let public_inputs: Vec<String> = instances.iter().map(format_field_element).collect();

    let protocol = ProtocolData {
        k,
        num_instance: vec![instances.len()],
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

/// Save raw Fr instances as a flat `Vec<u8>` (each Fr -> 32-byte LE), the same
/// format `convert_proof.rs` expects for `--instances`.
pub fn save_instances_binary(
    instances: &[Fr],
    output_path: impl AsRef<Path>,
) -> anyhow::Result<()> {
    let mut bytes: Vec<u8> = Vec::with_capacity(instances.len() * 32);
    for fr in instances {
        bytes.extend_from_slice(fr.to_bytes().as_ref());
    }
    std::fs::write(output_path, bytes)?;
    Ok(())
}

/// Read a `[Fr]` from a flat `Vec<u8>` (32-byte LE chunks). Inverse of
/// [`save_instances_binary`].
pub fn load_instances_binary(path: impl AsRef<Path>) -> anyhow::Result<Vec<Fr>> {
    let bytes = std::fs::read(path)?;
    if !bytes.len().is_multiple_of(32) {
        anyhow::bail!(
            "instances file size {} is not a multiple of 32 bytes",
            bytes.len()
        );
    }
    let mut out = Vec::with_capacity(bytes.len() / 32);
    for chunk in bytes.chunks_exact(32) {
        let mut repr = <Fr as PrimeField>::Repr::default();
        repr.as_mut().copy_from_slice(chunk);
        out.push(Fr::from_repr(repr).expect("invalid Fr encoding in instances file"));
    }
    Ok(out)
}
