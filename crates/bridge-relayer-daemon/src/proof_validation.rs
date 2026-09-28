//! `verifyBlock` proof shape checks for the production all-SHPLONK wiring.
//!
//! Circuit 1A, 1B, and 2 are all R15 SHPLONK aggregator calldata
//! (`instances ‖ proof`). Circuit 1B is keygen'd at K=21 so its aggregated Yul
//! fits EIP-170 — the gnark Groth16 fallback hybrid is retired.

use crate::{error::RelayerError, types::FinalizationType};

/// Minimum instance prefix for an attestation SHPLONK bundle
/// (12 acc + 4 inner + 1 inner-VK digest). Both Circuit 1A (primary) and
/// Circuit 1B (fallback) expose 4 public inputs, and the aggregator appends
/// a Poseidon digest of the inner VK witnesses that the on-chain adapter
/// checks against its pinned `vkDigest`.
pub const SHPLONK_MIN_ATTESTATION_INSTANCES: usize = (12 + 4 + 1) * 32;

/// Minimum instance prefix for layer-hashes SHPLONK bundle
/// (12 acc + 14 inner + 1 inner-VK digest).
pub const SHPLONK_MIN_LAYER_INSTANCES: usize = (12 + 14 + 1) * 32;

pub fn validate_attestation_proof(
    fin_type: FinalizationType,
    proof: &[u8],
) -> Result<(), RelayerError> {
    if proof.len() < SHPLONK_MIN_ATTESTATION_INSTANCES {
        let circuit = match fin_type {
            FinalizationType::Primary => "primary (1A)",
            FinalizationType::Fallback => "fallback (1B)",
        };
        return Err(RelayerError::other(format!(
            "{circuit} attestation proof too short for SHPLONK aggregator calldata: {} bytes \
             (need >= {SHPLONK_MIN_ATTESTATION_INSTANCES})",
            proof.len()
        )));
    }
    Ok(())
}

pub fn validate_layer_hashes_proof(proof: &[u8]) -> Result<(), RelayerError> {
    if proof.len() < SHPLONK_MIN_LAYER_INSTANCES {
        return Err(RelayerError::other(format!(
            "layer-hashes proof too short for SHPLONK aggregator calldata: {} bytes (need >= \
             {SHPLONK_MIN_LAYER_INSTANCES})",
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
    fn shplonk_attestation_ok() {
        validate_attestation_proof(FinalizationType::Primary, &[0u8; 3840]).unwrap();
        validate_attestation_proof(FinalizationType::Fallback, &[0u8; 3840]).unwrap();
    }

    #[test]
    fn attestation_rejects_short_blob() {
        // Below SHPLONK_MIN_ATTESTATION_INSTANCES (=544). There is no
        // 256-byte back-compat lane — an undersized input fails the same
        // short-blob gate.
        assert!(validate_attestation_proof(FinalizationType::Fallback, &[0u8; 300]).is_err());
        assert!(validate_attestation_proof(FinalizationType::Primary, &[0u8; 300]).is_err());
        assert!(validate_attestation_proof(FinalizationType::Primary, &[0u8; 256]).is_err());
    }

    /// Exact boundary: exactly `SHPLONK_MIN_ATTESTATION_INSTANCES` bytes must
    /// pass; one byte short must fail. Pins the off-by-one after any future
    /// layout change (e.g. a re-exposed public input added or removed).
    #[test]
    fn attestation_exact_boundary() {
        let one_short = vec![0u8; SHPLONK_MIN_ATTESTATION_INSTANCES - 1];
        let at_min = vec![0u8; SHPLONK_MIN_ATTESTATION_INSTANCES];
        assert!(validate_attestation_proof(FinalizationType::Primary, &one_short).is_err());
        assert!(validate_attestation_proof(FinalizationType::Primary, &at_min).is_ok());
        assert!(validate_attestation_proof(FinalizationType::Fallback, &one_short).is_err());
        assert!(validate_attestation_proof(FinalizationType::Fallback, &at_min).is_ok());
    }

    #[test]
    fn layer_hashes_exact_boundary() {
        let one_short = vec![0u8; SHPLONK_MIN_LAYER_INSTANCES - 1];
        let at_min = vec![0u8; SHPLONK_MIN_LAYER_INSTANCES];
        assert!(validate_layer_hashes_proof(&one_short).is_err());
        assert!(validate_layer_hashes_proof(&at_min).is_ok());
    }
}
