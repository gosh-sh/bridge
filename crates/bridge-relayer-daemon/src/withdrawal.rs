//! Circuit 4 (`withdrawByProofBundle`) artefacts from the partner prover daemon.
//!
//! The shellnet orchestrator writes `proofs/proof_event_NNN.json` with a raw
//! Halo2 proof (`proof_hex`) and thirteen public-instance field elements
//! (`public_instances_hex`) — the 11 pre-multi-thread slots plus the two
//! block-id endpoints `x_block_id` and `y_block_id` added by the multi-hop
//! commit (Circuit 4 = `BridgeEventFinalProof` with `TOTAL_PUBLIC_INPUTS =
//! 13`).
//!
//! The production on-chain verifier is the R15 SHPLONK aggregator
//! (`BridgeWithdrawalAggregatorVerifier`, Yul): it consumes aggregator calldata
//! `instances ‖ proof` where the instance prefix is 12 KZG accumulator limbs +
//! the 13 re-exposed Circuit-4 public inputs + a Poseidon digest of the inner
//! VK witnesses (≥ `SHPLONK_MIN_WITHDRAWAL_INSTANCES` bytes; 26-instance layout
//! post-merge of multi-thread 13-PI + PR #67 inner-VK binding). This mirrors
//! the 1A/1B/2 shape checks in [`crate::proof_validation`].

use std::path::{Path, PathBuf};

use alloy::primitives::{Bytes, U256};
use serde::Deserialize;

use crate::error::RelayerError;

// Historical note (2026-08-16): a `WithdrawalResultGate` struct + a
// `proof_event_*.result.json` polling loop used to live here, gating each
// on-chain `withdrawByProofBundle` submission on `verified && anchor_matched
// && proof_valid` fields written by `bridge-verifier-daemon`. That
// daemon was a Rust mirror of the Solidity verifier used during early
// bring-up when the on-chain verifier did not yet exist; keeping the
// gate meant the production relayer waited for a dev-only sidecar to
// rubber-stamp every proof. Deleted along with the `skip_verified_gate`
// opt-out. Authoritative acceptance is now the on-chain verifier's
// success on the actual `withdrawByProofBundle` transaction (or its
// `eth_call` dry-run) — nothing else.

/// Thirteen public inputs for Circuit 4: the 11 pre-multi-thread slots
/// (`tokenId..anchorLayer`) plus the two block-id endpoints (`x_block_id`,
/// `y_block_id`) added by the multi-hop commit. Matches
/// `bridge_event_prove_circuit::bridge_event_final_proof::TOTAL_PUBLIC_INPUTS`.
pub const WITHDRAWAL_PUBLIC_INPUTS: usize = 13;

/// Minimum length of a Circuit 4 SHPLONK aggregator calldata blob: the instance
/// prefix is 12 KZG accumulator limbs + the 13 re-exposed Circuit-4 public
/// inputs + 1 inner-VK Poseidon digest slot, each a 32-byte field element
/// (the outer proof bytes follow). Matches
/// `BridgeWithdrawalAggregatorVerifier`'s 26-instance layout.
pub const SHPLONK_MIN_WITHDRAWAL_INSTANCES: usize = (12 + WITHDRAWAL_PUBLIC_INPUTS + 1) * 32;

/// Byte length of a Circuit-4 SHPLONK calldata blob after the inner-VK
/// binding and multi-thread 13-PI layout: 26 instance words plus the
/// outer proof. The pre-vk-binding, 11-PI blob was 3 648 B; main's
/// vk-binding-only, 11-PI blob was 3 680 B.
///
/// TODO(merge/feature-multithreading): this value is a provisional forecast
/// (`3_680 + 2 * 32 = 3_744`, adding two instance words for `xBlockId` and
/// `yBlockId`). Re-measure against the regenerated
/// `BridgeWithdrawalAggregatorVerifier_calldata.bin` in the same commit as
/// the post-merge aggregator export and replace this constant.
pub const WITHDRAWAL_CALLDATA_LEN: usize = 3_744;

/// One per-hop `BridgeMultiHopProof` blob as persisted in
/// `proof_event_*.json` under the `hops_hex` array. Mirrors the driver's
/// `HopBlob` (see
/// `crates/bridge-relayer-daemon/src/withdraw_e2e/driver.rs`): two
/// public instances per hop (`hopStartBlockId`, `hopEndBlockId`, each a
/// 32-byte LE Fr repr) plus the raw multi-hop proof bytes.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct HopBlobHex {
    pub proof_hex: String,
    pub public_instances_hex: Vec<String>,
}

/// Number of per-hop public instances (`hopStartBlockId`, `hopEndBlockId`).
pub const MULTI_HOP_PUBLIC_INPUTS: usize = 2;

/// Parsed `proof_event_*.json` from
#[derive(Clone, Debug, Deserialize)]
pub struct PartnerWithdrawalProof {
    #[serde(default)]
    pub seq_no: u64,
    pub proof_hex: String,
    pub public_instances_hex: Vec<String>,
    #[serde(default)]
    pub self_verified: bool,
    /// Ordered `BridgeMultiHopProof` snarks for the cross-thread hop chain.
    /// Empty for same-thread claims (where the FinalProof PIs satisfy
    /// `xBlockId == yBlockId`). Consumed by `withdrawByProofBundle`.
    #[serde(default)]
    pub hops_hex: Vec<HopBlobHex>,
}

/// Mirrors `IBridgeWithdrawalVerifier.WithdrawalPublicInputs`. Trailing
/// `x_block_id` / `y_block_id` slots hold the multi-thread block-id
/// endpoints: same-thread claims satisfy `x_block_id == y_block_id`;
/// cross-thread claims carry the terminal L7 walker endpoint in
/// `y_block_id`.
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
    pub anchor_layer: U256,
    pub x_block_id: U256,
    pub y_block_id: U256,
}

impl PartnerWithdrawalProof {
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, RelayerError> {
        serde_json::from_slice(bytes)
            .map_err(|e| RelayerError::other(format!("parse proof_event JSON: {e}")))
    }

    pub fn proof_bytes(&self) -> Result<Bytes, RelayerError> {
        let raw = decode_hex(&self.proof_hex)?;
        // Production shape: R15 SHPLONK aggregator calldata (`instances ‖ proof`).
        if raw.len() < SHPLONK_MIN_WITHDRAWAL_INSTANCES {
            return Err(RelayerError::other(format!(
                "withdrawal proof is {} bytes; expected SHPLONK aggregator calldata (>= {} bytes: \
                 12 accumulator limbs + {} Circuit-4 public inputs + 1 inner-VK digest, then the \
                 outer proof)",
                raw.len(),
                SHPLONK_MIN_WITHDRAWAL_INSTANCES,
                WITHDRAWAL_PUBLIC_INPUTS,
            )));
        }
        Ok(Bytes::from(raw))
    }

    /// Decode `hops_hex` into the flat `(hopStart, hopEnd)` public-input
    /// pairs the `withdrawByProofBundle` calldata carries. Return type
    /// matches [`EthBridgeClient::submit_withdraw_bundle`]'s
    /// `hop_public_inputs: &[Vec<U256>]`. Same-thread bundles yield an
    /// empty vec — legitimate on-chain path when the FinalProof PIs have
    /// `xBlockId == yBlockId`.
    pub fn hop_pis(&self) -> Result<Vec<Vec<U256>>, RelayerError> {
        self.hops_hex
            .iter()
            .enumerate()
            .map(|(i, hop)| {
                if hop.public_instances_hex.len() != MULTI_HOP_PUBLIC_INPUTS {
                    return Err(RelayerError::other(format!(
                        "hop #{i}: expected {MULTI_HOP_PUBLIC_INPUTS} public_instances_hex \
                         entries, got {}",
                        hop.public_instances_hex.len()
                    )));
                }
                Ok(vec![
                    fr_hex_to_u256(&hop.public_instances_hex[0])?,
                    fr_hex_to_u256(&hop.public_instances_hex[1])?,
                ])
            })
            .collect()
    }

    /// Decode `hops_hex` into the ordered `BridgeMultiHopProof` byte blobs
    /// the on-chain multi-hop verifier consumes.
    pub fn hop_proofs(&self) -> Result<Vec<Bytes>, RelayerError> {
        self.hops_hex
            .iter()
            .map(|hop| Ok(Bytes::from(decode_hex(&hop.proof_hex)?)))
            .collect()
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
            anchor_layer: field(10)?,
            x_block_id: field(11)?,
            y_block_id: field(12)?,
        })
    }
}

/// `true` for `proof_event_*.json` files. The `.result.json` suffix (a
/// sidecar the retired `bridge-verifier-daemon` used to write) is still
/// filtered out so a stray dev-mode sidecar in the same directory does
/// not confuse the discovery scan.
pub fn is_event_proof_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.starts_with("proof_event_") && name.ends_with(".json") && !name.ends_with(".result.json")
}

/// Discover `proof_event_*.json` bundles in `dir`, sorted by filename so
/// lower seqnos are processed first. Missing directory yields an empty
/// list (not an error) so the daemon can start before the prover has
/// produced anything.
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

/// Partner schema v6 `block_id_hex` carries the raw 32-byte BE chain hash
/// (= `Solidity uint256(bytes32(blockId))`), decoded here as big-endian and
/// left unreduced.
///
/// For anything that goes on-chain as a circuit public input, use
/// [`hash_hex_to_block_id_fr`] instead — see the note there.
pub fn hash_hex_to_u256(hex_str: &str) -> Result<U256, RelayerError> {
    let bytes = decode_hex(hex_str)?;
    if bytes.len() != 32 {
        return Err(RelayerError::other(format!(
            "expected 32-byte hash hex, got {} bytes",
            bytes.len()
        )));
    }
    Ok(U256::from_be_slice(&bytes))
}

/// The same hash reduced into BN254 `Fr` — the form `verifyBlock` and
/// `applyBkSetUpdate` expect.
///
/// The older comments in this tree claimed the on-chain verifier reduces the
/// argument itself via `mod(calldataload, f_q)`. That was true while the bridge
/// called a Yul verifier directly, and stopped being true with the R15
/// aggregator adapters: `PrimaryAggregatorVerifier` compares the argument
/// against an instance read out of the proof *before* the pairing, byte for
/// byte, and instances are canonical field elements. Since only
/// `r / 2^256 = 18.9%` of chain hashes are canonical as-is, sending the raw
/// value fails for roughly four blocks in five.
pub fn hash_hex_to_block_id_fr(hex_str: &str) -> Result<U256, RelayerError> {
    Ok(hash_hex_to_u256(hex_str)? % crate::types::BN254_FR_MODULUS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real shellnet block 4,888,576 chain hash exceeds BN254 `r` so the
    /// reduced form differs from the raw form. Captured from
    /// `diag/instances_4888576.txt` inst[12] alongside the GQL
    /// `block.block_id`, confirmed independently by hand-subtraction.
    #[test]
    fn hash_hex_reduces_above_modulus() {
        let raw = "3151be4d584a014e66bbe4f9d2713ff8ac6a2455db7b191c41d5cb452e05bdad";
        let reduced_hex = "00ed6fda77186124ae6b9f4350efe79b84363c0d61c1a88afdf3d5b13e05bdac";
        let expected = U256::from_be_slice(&hex::decode(reduced_hex).unwrap());
        assert_eq!(hash_hex_to_block_id_fr(raw).unwrap(), expected);
        // The un-reduced helper must not touch it — this is what
        // `applyBkSetUpdate` still needs.
        assert_ne!(hash_hex_to_u256(raw).unwrap(), expected);
    }

    /// Block 4,888,064 chain hash was already `< r`, which is why that
    /// key-block succeeded on-chain before the fix. Reduction must be
    /// a no-op for such values.
    #[test]
    fn hash_hex_reduce_below_modulus_is_identity() {
        let raw = "1234abcd5678ef00112233445566778899aabbccddeeff001122334455667788";
        assert!(U256::from_be_slice(&hex::decode(raw).unwrap()) < crate::types::BN254_FR_MODULUS);
        assert_eq!(
            hash_hex_to_block_id_fr(raw).unwrap(),
            hash_hex_to_u256(raw).unwrap(),
        );
    }

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
                "0900000000000000000000000000000000000000000000000000000000000000",
                "0100000000000000000000000000000000000000000000000000000000000000",
                "0b00000000000000000000000000000000000000000000000000000000000000",
                "0c00000000000000000000000000000000000000000000000000000000000000"
            ]
        }"#;
        let p = PartnerWithdrawalProof::from_json_bytes(json.as_bytes()).unwrap();
        let pi = p.public_inputs().unwrap();
        assert_eq!(pi.token_id, U256::from(3u64));
        // Slot 10 (`anchor_layer`) — guard against off-by-one that drops or
        // swaps `anchor_layer` with an adjacent slot. If a decoder mutation
        // put `finalRoot` (a ~256-bit hash) into `anchor_layer`, the
        // on-chain revert would almost always be `InvalidNumLayers` at
        // `AckiNackiBridge.sol:1339-1340` (bounded by `MAX_LAYER_HASHES`),
        // and in the rare small-value case would fall through to
        // `UnknownAnchor` at :1342-1343 — either way masking the real bug
        // as a chain-side error.
        assert_eq!(pi.anchor_layer, U256::from(1u64));
        // Slot 11 = `x_block_id`, slot 12 = `y_block_id`. Distinct values in
        // the fixture prove the trailing multi-thread endpoints round-trip
        // independently — a decoder that mis-orders 11 and 12, or drops
        // either, trips this assertion.
        assert_eq!(pi.x_block_id, U256::from(0x0bu64));
        assert_eq!(pi.y_block_id, U256::from(0x0cu64));
    }

    /// Negative: a `proof_event_*.json` carrying
    /// `WITHDRAWAL_PUBLIC_INPUTS + 1` public instances (one too many for
    /// the current layout) must fail `public_inputs()` — silent
    /// truncation would let a payload with an extra field sneak through,
    /// and any decoder that ignored the trailing entry would still match
    /// the first `WITHDRAWAL_PUBLIC_INPUTS` slots. Guards against a
    /// future layout bump that forgets to update
    /// `WITHDRAWAL_PUBLIC_INPUTS` on the consumer side.
    #[test]
    fn public_inputs_rejects_wrong_instance_count() {
        let mut hexes: Vec<String> = (0..WITHDRAWAL_PUBLIC_INPUTS + 1)
            .map(|i| format!("{:02x}{}", (i + 1) as u8, "00".repeat(31)))
            .collect();
        let p = PartnerWithdrawalProof {
            seq_no: 0,
            proof_hex: "aa".into(),
            public_instances_hex: hexes.clone(),
            self_verified: false,
            hops_hex: Vec::new(),
        };
        assert!(
            p.public_inputs().is_err(),
            "{} instances must not decode",
            WITHDRAWAL_PUBLIC_INPUTS + 1
        );
        // And `WITHDRAWAL_PUBLIC_INPUTS - 1` (one short) also fails.
        hexes.pop();
        hexes.pop();
        let p_short = PartnerWithdrawalProof {
            seq_no: 0,
            proof_hex: "aa".into(),
            public_instances_hex: hexes,
            self_verified: false,
            hops_hex: Vec::new(),
        };
        assert!(
            p_short.public_inputs().is_err(),
            "{} instances must not decode either",
            WITHDRAWAL_PUBLIC_INPUTS - 1
        );
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
    fn proof_bytes_accepts_shplonk_rejects_short() {
        // Production SHPLONK aggregator calldata (instances + outer proof).
        let shplonk = PartnerWithdrawalProof::from_json_bytes(
            proof_json_with(SHPLONK_MIN_WITHDRAWAL_INSTANCES + 3200).as_bytes(),
        )
        .unwrap();
        assert!(shplonk.proof_bytes().is_ok());

        // Post-NB-Q7: the retired 256-byte back-compat lane is gone, so a
        // legacy-sized blob now fails the same short-blob gate as any other
        // undersized input.
        let legacy =
            PartnerWithdrawalProof::from_json_bytes(proof_json_with(256).as_bytes()).unwrap();
        assert!(legacy.proof_bytes().is_err());

        // A blob shorter than the SHPLONK instance prefix is rejected.
        let bad = PartnerWithdrawalProof::from_json_bytes(proof_json_with(300).as_bytes()).unwrap();
        assert!(bad.proof_bytes().is_err());
    }

    /// Legacy `proof_event_*.json` files (pre-multi-hop) lack the `hops_hex`
    /// field entirely. `#[serde(default)]` must let them parse as an empty
    /// vec — otherwise same-thread daemons flying pre-migration bundles
    /// would fail to load their own output.
    #[test]
    fn legacy_proof_event_without_hops_field_parses() {
        let hexes: Vec<String> = (0..WITHDRAWAL_PUBLIC_INPUTS)
            .map(|i| format!("{:02x}{}", (i + 1) as u8, "00".repeat(31)))
            .collect();
        let insts = hexes
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(",");
        let json = format!(
            r#"{{"proof_hex":"aa","public_instances_hex":[{insts}]}}"#
        );
        let p = PartnerWithdrawalProof::from_json_bytes(json.as_bytes()).unwrap();
        assert!(p.hops_hex.is_empty(), "legacy files must parse with empty hops_hex");
        assert!(p.hop_pis().unwrap().is_empty(), "same-thread claim has no hop pis");
        assert!(p.hop_proofs().unwrap().is_empty(), "same-thread claim has no hop proofs");
    }

    /// A `proof_event_*.json` with a populated `hops_hex` array must
    /// round-trip: parse → `hop_pis()` returns each `[hopStart, hopEnd]`
    /// pair as `U256`, and `hop_proofs()` returns each raw proof blob.
    #[test]
    fn cross_thread_proof_event_hops_roundtrip() {
        let hexes: Vec<String> = (0..WITHDRAWAL_PUBLIC_INPUTS)
            .map(|i| format!("{:02x}{}", (i + 1) as u8, "00".repeat(31)))
            .collect();
        let insts = hexes
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(",");
        // Two hops chain-linked: hop0 = [xBlockId, 0x2a], hop1 = [0x2a, yBlockId].
        // Values match the LE Fr repr convention (first byte is low limb).
        let hop0_start = "0b00000000000000000000000000000000000000000000000000000000000000";
        let hop0_end = "2a00000000000000000000000000000000000000000000000000000000000000";
        let hop1_start = hop0_end;
        let hop1_end = "0c00000000000000000000000000000000000000000000000000000000000000";
        let json = format!(
            r#"{{
                "proof_hex":"aa",
                "public_instances_hex":[{insts}],
                "hops_hex":[
                    {{"proof_hex":"deadbeef","public_instances_hex":["{hop0_start}","{hop0_end}"]}},
                    {{"proof_hex":"cafef00d","public_instances_hex":["{hop1_start}","{hop1_end}"]}}
                ]
            }}"#
        );
        let p = PartnerWithdrawalProof::from_json_bytes(json.as_bytes()).unwrap();
        assert_eq!(p.hops_hex.len(), 2);
        let pis = p.hop_pis().unwrap();
        assert_eq!(pis.len(), 2);
        assert_eq!(pis[0].len(), 2);
        assert_eq!(pis[0][0], U256::from(0x0bu64));
        assert_eq!(pis[0][1], U256::from(0x2au64));
        assert_eq!(pis[1][0], U256::from(0x2au64));
        assert_eq!(pis[1][1], U256::from(0x0cu64));
        let proofs = p.hop_proofs().unwrap();
        assert_eq!(proofs.len(), 2);
        assert_eq!(proofs[0].as_ref(), &hex::decode("deadbeef").unwrap()[..]);
        assert_eq!(proofs[1].as_ref(), &hex::decode("cafef00d").unwrap()[..]);
    }

    /// A hop entry with the wrong number of public instances must not decode.
    /// Guards against a schema drift that silently drops the tail endpoint.
    #[test]
    fn hop_pis_rejects_wrong_instance_count() {
        let p = PartnerWithdrawalProof {
            seq_no: 0,
            proof_hex: "aa".into(),
            public_instances_hex: (0..WITHDRAWAL_PUBLIC_INPUTS)
                .map(|i| format!("{:02x}{}", (i + 1) as u8, "00".repeat(31)))
                .collect(),
            self_verified: false,
            hops_hex: vec![HopBlobHex {
                proof_hex: "aa".into(),
                public_instances_hex: vec!["0b00000000000000000000000000000000000000000000000000000000000000".into()],
            }],
        };
        assert!(p.hop_pis().is_err(), "hop with 1 PI must not decode");
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
