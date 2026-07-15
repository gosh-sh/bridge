//! Circuit 4 (`withdrawByProof`) artefacts from the partner prover daemon.
//!
//! The shellnet orchestrator writes `proofs/proof_event_NNN.json` with a raw
//! Halo2 proof (`proof_hex`) and ten public-instance field elements
//! (`public_instances_hex`).
//!
//! The production on-chain verifier is the R15 SHPLONK aggregator
//! (`BridgeWithdrawalAggregatorVerifier`, Yul): it consumes aggregator calldata
//! `instances ‖ proof` where the instance prefix is 12 KZG accumulator limbs +
//! the 10 re-exposed Circuit-4 public inputs (≥
//! `SHPLONK_MIN_WITHDRAWAL_INSTANCES` bytes). This mirrors the 1A/1B/2 shape
//! checks in [`crate::proof_validation`]. A legacy 256-byte blob
//! (`GROTH16_PROOF_SIZE`) is still accepted for back-compat with the retired
//! per-circuit Groth16 adapter / mock-verifier smoke path — it is **not** the
//! production shape.

use std::path::{Path, PathBuf};

use alloy::primitives::{Bytes, U256};
use serde::Deserialize;

use crate::error::RelayerError;

/// Legacy 256-byte proof size (retired per-circuit Groth16 adapter / mock
/// verifier smoke path). Accepted for back-compat only; the production path is
/// the SHPLONK aggregator calldata (`SHPLONK_MIN_WITHDRAWAL_INSTANCES`).
pub const GROTH16_PROOF_SIZE: usize = 256;

/// Ten public inputs for Circuit 4 (single-final-root layout).
pub const WITHDRAWAL_PUBLIC_INPUTS: usize = 10;

/// Minimum length of a Circuit 4 SHPLONK aggregator calldata blob: the instance
/// prefix is 12 KZG accumulator limbs + the 10 re-exposed Circuit-4 public
/// inputs, each a 32-byte field element (the outer proof bytes follow). Matches
/// `BridgeWithdrawalAggregatorVerifier`'s 22-instance layout.
pub const SHPLONK_MIN_WITHDRAWAL_INSTANCES: usize = (12 + WITHDRAWAL_PUBLIC_INPUTS) * 32;

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
        // Production shape: R15 SHPLONK aggregator calldata (`instances ‖ proof`).
        // Legacy 256-byte blob accepted only for the retired Groth16 adapter /
        // mock-verifier smoke path.
        if raw.len() != GROTH16_PROOF_SIZE && raw.len() < SHPLONK_MIN_WITHDRAWAL_INSTANCES {
            return Err(RelayerError::other(format!(
                "withdrawal proof is {} bytes; expected SHPLONK aggregator calldata (>= {} bytes: \
                 12 accumulator limbs + {} Circuit-4 public inputs, then the outer proof) or a \
                 legacy {}-byte blob",
                raw.len(),
                SHPLONK_MIN_WITHDRAWAL_INSTANCES,
                WITHDRAWAL_PUBLIC_INPUTS,
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

/// Verifier ACK written next to each `proof_event_NNN.json` as
/// `proof_event_NNN.result.json` by `bridge-verifier-daemon`. The
/// withdraw daemon gates submission on `verified && anchor_matched &&
/// proof_valid` unless `--skip-verified-gate` is passed.
#[derive(Clone, Debug, Deserialize)]
pub struct WithdrawalResultGate {
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub anchor_matched: bool,
    #[serde(default)]
    pub proof_valid: bool,
}

impl WithdrawalResultGate {
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, RelayerError> {
        serde_json::from_slice(bytes)
            .map_err(|e| RelayerError::other(format!("parse proof_event result JSON: {e}")))
    }

    /// The gate is satisfied only when the verifier confirmed all three.
    pub fn is_accepted(&self) -> bool {
        self.verified && self.anchor_matched && self.proof_valid
    }
}

/// `true` for `proof_event_*.json` files that are *not* the sibling
/// `proof_event_*.result.json` ACK.
pub fn is_event_proof_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.starts_with("proof_event_") && name.ends_with(".json") && !name.ends_with(".result.json")
}

/// Map `…/proof_event_NNN.json` → `…/proof_event_NNN.result.json`.
pub fn result_path_for(proof_path: &Path) -> PathBuf {
    let mut s = proof_path.as_os_str().to_os_string();
    // strip trailing `.json`, append `.result.json`
    let as_str = s.to_string_lossy().to_string();
    if let Some(stem) = as_str.strip_suffix(".json") {
        return PathBuf::from(format!("{stem}.result.json"));
    }
    s.push(".result.json");
    PathBuf::from(s)
}

/// Discover `proof_event_*.json` bundles in `dir`, sorted by filename so
/// lower seqnos are processed first. The sibling `*.result.json` ACKs are
/// excluded. Missing directory yields an empty list (not an error) so the
/// daemon can start before the prover has produced anything.
pub fn discover_event_proofs(dir: &Path) -> Result<Vec<PathBuf>, RelayerError> {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(RelayerError::other(format!(
                "read_dir {}: {e}",
                dir.display()
            )))
        },
    };
    let mut out = Vec::new();
    for entry in read {
        let entry = entry.map_err(|e| RelayerError::other(format!("dir entry: {e}")))?;
        let path = entry.path();
        if is_event_proof_file(&path) {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
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

    fn proof_json_with(proof_len: usize) -> String {
        let proof_hex = hex::encode(vec![0xABu8; proof_len]);
        let insts: Vec<String> = (0..WITHDRAWAL_PUBLIC_INPUTS)
            .map(|i| format!("{:02x}{}", (i + 1) as u8, "00".repeat(31)))
            .collect();
        format!(
            r#"{{"proof_hex":"{proof_hex}","public_instances_hex":[{}]}}"#,
            insts
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(",")
        )
    }

    #[test]
    fn proof_bytes_accepts_shplonk_and_legacy_rejects_between() {
        // Legacy 256-byte back-compat blob.
        let legacy =
            PartnerWithdrawalProof::from_json_bytes(proof_json_with(GROTH16_PROOF_SIZE).as_bytes())
                .unwrap();
        assert_eq!(legacy.proof_bytes().unwrap().len(), GROTH16_PROOF_SIZE);

        // Production SHPLONK aggregator calldata (instances + outer proof).
        let shplonk = PartnerWithdrawalProof::from_json_bytes(
            proof_json_with(SHPLONK_MIN_WITHDRAWAL_INSTANCES + 3200).as_bytes(),
        )
        .unwrap();
        assert!(shplonk.proof_bytes().is_ok());

        // A blob that is neither legacy-256 nor a valid SHPLONK prefix is rejected.
        let bad = PartnerWithdrawalProof::from_json_bytes(proof_json_with(300).as_bytes()).unwrap();
        assert!(bad.proof_bytes().is_err());
    }

    #[test]
    fn result_gate_requires_all_three() {
        let g = WithdrawalResultGate::from_json_bytes(
            br#"{"verified":true,"anchor_matched":true,"proof_valid":true}"#,
        )
        .unwrap();
        assert!(g.is_accepted());
        let bad = WithdrawalResultGate::from_json_bytes(
            br#"{"verified":true,"anchor_matched":false,"proof_valid":true}"#,
        )
        .unwrap();
        assert!(!bad.is_accepted());
        // Missing fields default to false → not accepted.
        let empty = WithdrawalResultGate::from_json_bytes(b"{}").unwrap();
        assert!(!empty.is_accepted());
    }

    #[test]
    fn event_proof_file_classification() {
        assert!(is_event_proof_file(Path::new("/x/proof_event_000000.json")));
        assert!(!is_event_proof_file(Path::new(
            "/x/proof_event_000000.result.json"
        )));
        assert!(!is_event_proof_file(Path::new("/x/proof_001536.json")));
        assert!(!is_event_proof_file(Path::new("/x/result_001536.json")));
    }

    #[test]
    fn result_path_mapping() {
        assert_eq!(
            result_path_for(Path::new("/x/proof_event_000007.json")),
            PathBuf::from("/x/proof_event_000007.result.json")
        );
    }

    #[test]
    fn discover_sorts_and_filters() {
        let dir = std::env::temp_dir().join(format!("wd_discover_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in [
            "proof_event_000002.json",
            "proof_event_000000.json",
            "proof_event_000000.result.json",
            "proof_001536.json",
            "not_a_proof.txt",
        ] {
            std::fs::write(dir.join(f), b"{}").unwrap();
        }
        let found = discover_event_proofs(&dir).unwrap();
        let names: Vec<_> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec![
            "proof_event_000000.json",
            "proof_event_000002.json"
        ]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn discover_missing_dir_is_empty() {
        let found = discover_event_proofs(Path::new("/nonexistent/xyz/proofs")).unwrap();
        assert!(found.is_empty());
    }
}
