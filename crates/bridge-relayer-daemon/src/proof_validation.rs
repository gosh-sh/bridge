//! `verifyBlock` proof shape checks for hybrid production wiring.
//!
//! - Circuit 1A + 2: R15 SHPLONK aggregator calldata (`instances ‖ proof`).
//! - Circuit 1B fallback: 256-byte gnark Groth16 marshal-solidity blob.

use crate::{
    error::RelayerError,
    types::FinalizationType,
    withdrawal::GROTH16_PROOF_SIZE,
};

/// Minimum instance prefix for a primary attestation SHPLONK bundle (12 acc + 4 inner).
pub const SHPLONK_MIN_PRIMARY_INSTANCES: usize = (12 + 4) * 32;

/// Minimum instance prefix for layer-hashes SHPLONK bundle (12 acc + 14 inner).
pub const SHPLONK_MIN_LAYER_INSTANCES: usize = (12 + 14) * 32;

pub fn validate_attestation_proof(
    fin_type: FinalizationType,
    proof: &[u8],
) -> Result<(), RelayerError> {
    match fin_type {
        FinalizationType::Fallback => {
            if proof.len() != GROTH16_PROOF_SIZE {
                return Err(RelayerError::other(format!(
                    "fallback attestation proof must be {GROTH16_PROOF_SIZE} bytes (Groth16), got {}",
                    proof.len()
                )));
            }
        },
        FinalizationType::Primary => {
            if proof.len() == GROTH16_PROOF_SIZE {
                return Ok(());
            }
            if proof.len() < SHPLONK_MIN_PRIMARY_INSTANCES {
                return Err(RelayerError::other(format!(
                    "primary attestation proof too short for SHPLONK aggregator calldata: {} bytes \
                     (need >= {SHPLONK_MIN_PRIMARY_INSTANCES} or {GROTH16_PROOF_SIZE} Groth16)",
                    proof.len()
                )));
            }
        },
    }
    Ok(())
}

pub fn validate_layer_hashes_proof(proof: &[u8]) -> Result<(), RelayerError> {
    if proof.len() == GROTH16_PROOF_SIZE {
        return Ok(());
    }
    if proof.len() < SHPLONK_MIN_LAYER_INSTANCES {
        return Err(RelayerError::other(format!(
            "layer-hashes proof too short for SHPLONK aggregator calldata: {} bytes (need >= \
             {SHPLONK_MIN_LAYER_INSTANCES} or {GROTH16_PROOF_SIZE} Groth16)",
            proof.len()
        )));
    }
    Ok(())
}

pub fn decode_verify_block_proof(
    hex_str: &str,
    fin_type: FinalizationType,
    is_layer_hashes: bool,
    accept_halo2: bool,
) -> Result<Vec<u8>, RelayerError> {
    let raw = decode_hex(hex_str)?;
    if accept_halo2 {
        return Ok(raw);
    }
    if is_layer_hashes {
        validate_layer_hashes_proof(&raw)?;
    } else {
        validate_attestation_proof(fin_type, &raw)?;
    }
    Ok(raw)
}

fn decode_hex(s: &str) -> Result<Vec<u8>, RelayerError> {
    let trimmed = s.trim();
    let no_prefix = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    hex::decode(no_prefix).map_err(|e| RelayerError::other(format!("bad hex proof: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groth16_fallback_ok() {
        validate_attestation_proof(FinalizationType::Fallback, &[0u8; 256]).unwrap();
    }

    #[test]
    fn shplonk_primary_ok() {
        validate_attestation_proof(FinalizationType::Primary, &[0u8; 3840]).unwrap();
    }

    #[test]
    fn fallback_rejects_shplonk_size() {
        assert!(validate_attestation_proof(FinalizationType::Fallback, &[0u8; 3840]).is_err());
    }
}
