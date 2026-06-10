//! Circuit 4 (`withdrawByProof`) artefacts from the partner prover daemon.
//!
//! The shellnet orchestrator writes `proofs/proof_event_NNN.json` with a raw
//! Halo2 proof (`proof_hex`) and ten public-instance field elements
//! (`public_instances_hex`). On Ethereum the bridge expects a **256-byte
//! Groth16** proof unless a mock verifier is deployed — see
//! `GROTH16_PROOF_SIZE` and the operator runbook.

use alloy::primitives::{Bytes, U256};
use serde::Deserialize;

use crate::error::RelayerError;

/// On-chain Groth16 proof size enforced by `PrimaryVerifier` /
/// `BridgeWithdrawalVerifier`.
pub const GROTH16_PROOF_SIZE: usize = 256;

/// Ten public inputs for Circuit 4 (single-final-root layout).
pub const WITHDRAWAL_PUBLIC_INPUTS: usize = 10;

/// Parsed `proof_event_*.json` from
/// `acki-nacki-to-eth-bridge-halo2-prover`.
#[derive(Clone, Debug, Deserialize)]
pub struct PartnerWithdrawalProof {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub seq_no: u64,
    pub proof_hex: String,
    pub public_instances_hex: Vec<String>,
    #[serde(default)]
    pub self_verified: bool,
}

/// Mirrors `IBridgeWithdrawalVerifier.WithdrawalPublicInputs`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WithdrawalPublicInputs {
    pub token_id: U256,
    pub amount: U256,
    pub recipient_hi: U256,
    pub recipient_lo: U256,
    pub dst_chain_id: U256,
    pub sender_acc_fr: U256,
    pub dapp_fr: U256,
    pub acc_fr: U256,
    pub nullifier: U256,
    pub final_root: U256,
}

impl PartnerWithdrawalProof {
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, RelayerError> {
        serde_json::from_slice(bytes)
            .map_err(|e| RelayerError::other(format!("parse proof_event JSON: {e}")))
    }

    pub fn proof_bytes(&self) -> Result<Bytes, RelayerError> {
        let raw = decode_hex(&self.proof_hex)?;
        if raw.len() != GROTH16_PROOF_SIZE {
            return Err(RelayerError::other(format!(
                "withdrawal proof is {} bytes; Ethereum bridge expects {}-byte Groth16 \
                 (run gnark-wrappers/circuit-4 prove on the Halo2 export first)",
                raw.len(),
                GROTH16_PROOF_SIZE
            )));
        }
        Ok(Bytes::from(raw))
    }

    pub fn public_inputs(&self) -> Result<WithdrawalPublicInputs, RelayerError> {
        if self.public_instances_hex.len() != WITHDRAWAL_PUBLIC_INPUTS {
            return Err(RelayerError::other(format!(
                "expected {} public_instances_hex entries, got {}",
                WITHDRAWAL_PUBLIC_INPUTS,
                self.public_instances_hex.len()
            )));
        }
        let field = |i: usize| -> Result<U256, RelayerError> {
            fr_hex_to_u256(&self.public_instances_hex[i])
        };
        Ok(WithdrawalPublicInputs {
            token_id: field(0)?,
            amount: field(1)?,
            recipient_hi: field(2)?,
            recipient_lo: field(3)?,
            dst_chain_id: field(4)?,
            sender_acc_fr: field(5)?,
            dapp_fr: field(6)?,
            acc_fr: field(7)?,
            nullifier: field(8)?,
            final_root: field(9)?,
        })
    }
}

fn decode_hex(s: &str) -> Result<Vec<u8>, RelayerError> {
    let trimmed = s.trim();
    let no_prefix = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    hex::decode(no_prefix).map_err(|e| RelayerError::other(format!("bad hex: {e}")))
}

/// Partner `ipc::fr_to_hex` uses 32-byte **little-endian** Fr repr.
pub fn fr_hex_to_u256(hex_str: &str) -> Result<U256, RelayerError> {
    let bytes = decode_hex(hex_str)?;
    if bytes.len() != 32 {
        return Err(RelayerError::other(format!(
            "expected 32-byte Fr hex, got {} bytes",
            bytes.len()
        )));
    }
    let mut le = [0u8; 32];
    le.copy_from_slice(&bytes);
    Ok(U256::from_le_bytes(le))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proof_event_shape() {
        let json = r#"{
            "proof_hex": "aa",
            "public_instances_hex": [
                "0300000000000000000000000000000000000000000000000000000000000000",
                "0100000000000000000000000000000000000000000000000000000000000000",
                "0200000000000000000000000000000000000000000000000000000000000000",
                "0300000000000000000000000000000000000000000000000000000000000000",
                "0400000000000000000000000000000000000000000000000000000000000000",
                "0500000000000000000000000000000000000000000000000000000000000000",
                "0600000000000000000000000000000000000000000000000000000000000000",
                "0700000000000000000000000000000000000000000000000000000000000000",
                "0800000000000000000000000000000000000000000000000000000000000000",
                "0900000000000000000000000000000000000000000000000000000000000000"
            ]
        }"#;
        let p = PartnerWithdrawalProof::from_json_bytes(json.as_bytes()).unwrap();
        let pi = p.public_inputs().unwrap();
        assert_eq!(pi.token_id, U256::from(3u64));
    }
}
