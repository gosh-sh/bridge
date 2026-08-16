//! [`BlockSource`] — abstraction over "where do AN blocks come from?".
//!
//! Phase 5.1 ships:
//! - [`InMemoryBlockSource`] — pre-baked map keyed by seqNo. Used by the unit
//!   tests in this crate to drive multi-block scenarios in < 10 ms each.
//! - [`FixturesBlockSource`] — reads pre-generated bound proof artefacts from
//!   disk (the Phase 4.1 outputs of
//!   `bridge-snark-utils/proofs/bound/...` plus the gnark JSON produced
//!   by `circuit-1a/circuit-2`). Yields a single canned block keyed by
//!   `block_seq_no = 1`.
//!
//! Phase 5.2 will add `LiveBlockSource` backed by GraphQL + BOC parsing
//! + halo2 + gnark.
//!
//! Phase 5.2 scaffolding also ships [`ProverProofsBlockSource`], which reads
//! the partner `bridge-prover-daemon` JSON under `proofs/proof_<seqno>.json`.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

use alloy::primitives::{Bytes, U256};
use async_trait::async_trait;
use serde::Deserialize;

use crate::{
    error::RelayerError,
    proof_validation,
    types::{AnBlockData, FinalizationType, MAX_LAYER_HASHES},
    withdrawal::{fr_hex_to_u256, hash_hex_to_block_id_fr},
};

/// Asynchronous source of AN block payloads.
///
/// `fetch(seqno)` returns:
/// - `Ok(Some(block))` — a fully-prepared payload we can submit;
/// - `Ok(None)` — the block isn't available yet (not finalised, proof not
///   generated, partner node still syncing); the relayer waits.
/// - `Err(RelayerError)` — terminal error inside the source.
#[async_trait]
pub trait BlockSource: Send + Sync {
    async fn fetch(&self, target_seq_no: u64) -> Result<Option<AnBlockData>, RelayerError>;

    /// Acknowledge that `seq_no` was accepted on-chain. Default is a no-op
    /// (file-driven sources have no driver cursor). [`crate::live_source::LiveBlockSource`]
    /// advances `LiveProverDriver` and persists prover state.
    async fn ack_last_bundle(&self, _seq_no: u64) -> Result<(), RelayerError> {
        Ok(())
    }

    /// Optional post-ack snapshot of the driver's `BridgeState` for
    /// history-consistency checks. Default: no snapshot (skip Check A).
    async fn driver_snapshot(
        &self,
    ) -> Option<bridge_prover_lib::bridge_state::BridgeState> {
        None
    }
}

// ─────────────────────────────────────────────────────────────────────
// In-memory source for unit tests
// ─────────────────────────────────────────────────────────────────────

/// Pre-baked map keyed by seqNo. Mutable through interior `Mutex` so
/// tests can stage / mutate blocks at runtime.
pub struct InMemoryBlockSource {
    blocks: Mutex<BTreeMap<u64, AnBlockData>>,
}

impl InMemoryBlockSource {
    pub fn new() -> Self {
        Self {
            blocks: Mutex::new(BTreeMap::new()),
        }
    }

    /// Stage a block. Overwrites any prior entry for the same seqNo.
    pub fn insert(&self, block: AnBlockData) {
        self.blocks
            .lock()
            .expect("poisoned lock")
            .insert(block.block_seq_no, block);
    }

    /// Number of blocks currently staged.
    pub fn len(&self) -> usize {
        self.blocks.lock().expect("poisoned lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for InMemoryBlockSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BlockSource for InMemoryBlockSource {
    async fn fetch(&self, target_seq_no: u64) -> Result<Option<AnBlockData>, RelayerError> {
        Ok(self
            .blocks
            .lock()
            .expect("poisoned lock")
            .get(&target_seq_no)
            .cloned())
    }
}

// ─────────────────────────────────────────────────────────────────────
// Fixtures source — single canned block from disk artefacts
// ─────────────────────────────────────────────────────────────────────

/// Reads a single bound block from
/// `bridge-snark-utils/proofs/bound/{primary,layer-hashes}` plus
/// the gnark `groth16_output.json` files produced by
/// `circuit-1a`/`circuit-2` wrappers.
///
/// Use this for smoke-testing the live submission path against Anvil
/// without re-running halo2+gnark for every test invocation.
pub struct FixturesBlockSource {
    /// The canned block (always seqNo = 1 from the Phase 4.1 fixture).
    block: AnBlockData,
}

#[derive(Deserialize)]
struct BoundScenarioJson {
    block_seq_no: u32,
    block_id_decimal: String,
    bk_set_poseidon_decimal: String,
    layer_hash_decimals: Vec<String>,
    prev_max_level_layer_hash_decimal: String,
    num_layers: usize,
}

#[derive(Deserialize)]
struct Groth16OutputJson {
    /// Hex-encoded marshal-solidity proof bytes (with or without 0x prefix).
    proof: String,
}

impl FixturesBlockSource {
    /// Prefer R15 SHPLONK calldata from `verifiers_dir` when both
    /// `*_calldata.bin` files exist; otherwise fall back to legacy Groth16
    /// JSON under `fixtures_dir`.
    pub fn open(
        fixtures_dir: impl AsRef<Path>,
        verifiers_dir: Option<impl AsRef<Path>>,
    ) -> Result<Self, RelayerError> {
        let fixtures_dir = fixtures_dir.as_ref();
        if let Some(vdir) = verifiers_dir {
            return Self::from_hybrid_dirs(fixtures_dir, vdir);
        }
        if let Some(root) = fixtures_dir.ancestors().find(|p| {
            p.join("contracts/ethereum/verifiers/PrimaryAggregatorVerifier_calldata.bin")
                .is_file()
        }) {
            let vdir = root.join("contracts/ethereum/verifiers");
            if vdir
                .join("LayerHashesAggregatorVerifier_calldata.bin")
                .is_file()
            {
                return Self::from_hybrid_dirs(fixtures_dir, &vdir);
            }
        }
        Self::from_dir(fixtures_dir)
    }

    /// Build from Phase 4.1 bound artefacts + R15 SHPLONK calldata under
    /// `verifiers_dir` (hybrid production layout).
    pub fn from_hybrid_dirs(
        bound_dir: impl AsRef<Path>,
        verifiers_dir: impl AsRef<Path>,
    ) -> Result<Self, RelayerError> {
        let bound_dir = bound_dir.as_ref();
        let verifiers_dir = verifiers_dir.as_ref();
        let scenario_path = bound_dir.join("bound_scenario.json");
        let scenario: BoundScenarioJson = serde_json::from_slice(&std::fs::read(&scenario_path)?)?;

        let primary_calldata = verifiers_dir.join("PrimaryAggregatorVerifier_calldata.bin");
        let lh_calldata = verifiers_dir.join("LayerHashesAggregatorVerifier_calldata.bin");

        let primary_proof_bytes = if primary_calldata.is_file() {
            std::fs::read(&primary_calldata)?
        } else {
            let primary_proof_path = bound_dir.join("primary").join("groth16_output.json");
            let primary_out: Groth16OutputJson =
                serde_json::from_slice(&std::fs::read(&primary_proof_path)?)?;
            decode_hex(&primary_out.proof)?
        };

        let lh_proof_bytes = if lh_calldata.is_file() {
            std::fs::read(&lh_calldata)?
        } else {
            let lh_proof_path = bound_dir.join("layer-hashes").join("groth16_output.json");
            let lh_out: Groth16OutputJson =
                serde_json::from_slice(&std::fs::read(&lh_proof_path)?)?;
            decode_hex(&lh_out.proof)?
        };

        proof_validation::validate_attestation_proof(
            FinalizationType::Primary,
            &primary_proof_bytes,
        )?;
        proof_validation::validate_layer_hashes_proof(&lh_proof_bytes)?;

        Self::block_from_scenario(scenario, primary_proof_bytes, lh_proof_bytes)
    }

    /// Legacy layout: `primary/groth16_output.json` +
    /// `layer-hashes/groth16_output.json`.
    ///
    /// The scenario is tagged Primary (Phase 4.1 fixture). For hybrid R15
    /// deploys use [`Self::open`] or [`Self::from_hybrid_dirs`].
    pub fn from_dir(dir: impl AsRef<Path>) -> Result<Self, RelayerError> {
        let dir = dir.as_ref();
        let scenario_path = dir.join("bound_scenario.json");
        let primary_proof_path = dir.join("primary").join("groth16_output.json");
        let lh_proof_path = dir.join("layer-hashes").join("groth16_output.json");

        let scenario: BoundScenarioJson = serde_json::from_slice(&std::fs::read(&scenario_path)?)?;
        let primary_out: Groth16OutputJson =
            serde_json::from_slice(&std::fs::read(&primary_proof_path)?)?;
        let lh_out: Groth16OutputJson = serde_json::from_slice(&std::fs::read(&lh_proof_path)?)?;

        let primary_proof_bytes = decode_hex(&primary_out.proof)?;
        let lh_proof_bytes = decode_hex(&lh_out.proof)?;

        Self::block_from_scenario(scenario, primary_proof_bytes, lh_proof_bytes)
    }

    fn block_from_scenario(
        scenario: BoundScenarioJson,
        primary_proof_bytes: Vec<u8>,
        lh_proof_bytes: Vec<u8>,
    ) -> Result<Self, RelayerError> {
        if scenario.num_layers == 0 || scenario.num_layers > MAX_LAYER_HASHES {
            return Err(RelayerError::other(format!(
                "fixture num_layers {} out of 1..=10",
                scenario.num_layers
            )));
        }
        let mut layer_hashes = [U256::ZERO; MAX_LAYER_HASHES];
        for (i, dec) in scenario.layer_hash_decimals.iter().enumerate() {
            if i >= MAX_LAYER_HASHES {
                break;
            }
            layer_hashes[i] = parse_dec_u256(dec)?;
        }

        let block = AnBlockData {
            fin_type: FinalizationType::Primary,
            block_id: parse_dec_u256(&scenario.block_id_decimal)?,
            bk_set_commitment: parse_dec_u256(&scenario.bk_set_poseidon_decimal)?,
            block_seq_no: scenario.block_seq_no as u64,
            num_layers: scenario.num_layers as u8,
            layer_hashes,
            prev_max_level_layer_hash: parse_dec_u256(&scenario.prev_max_level_layer_hash_decimal)?,
            attestation_proof: Bytes::from(primary_proof_bytes),
            layer_hashes_proof: Bytes::from(lh_proof_bytes),
        };
        block.validate_shape()?;

        Ok(Self {
            block,
        })
    }

    pub fn block(&self) -> &AnBlockData {
        &self.block
    }
}

#[async_trait]
impl BlockSource for FixturesBlockSource {
    async fn fetch(&self, target_seq_no: u64) -> Result<Option<AnBlockData>, RelayerError> {
        if target_seq_no == self.block.block_seq_no {
            Ok(Some(self.block.clone()))
        } else {
            Ok(None)
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Partner prover daemon proofs — `proofs/proof_<seqno>.json`
// ─────────────────────────────────────────────────────────────────────

/// JSON written by `acki-nacki-to-eth-bridge-halo2-prover/bridge-prover-daemon`
/// (`bridge-prover-lib::ipc::ProofRequest`).
#[derive(Deserialize)]
struct PartnerProofRequest {
    block_seq_no: u32,
    #[serde(default, rename = "last_seen_block_seqno")]
    _last_seen_block_seqno: u32,
    block_id_hex: String,
    primary_proof_hex: String,
    layer_proof_hex: String,
    bk_set_poseidon_hash_hex: String,
    num_layers: u8,
    layer_hash_frs_hex: Vec<String>,
    prev_max_level_layer_hash_hex: String,
}

/// Reads proof bundles from the partner prover's `proofs/` directory.
///
/// Expects **R15 SHPLONK aggregator calldata** (`instances ‖ proof`) in the
/// JSON — see [`crate::proof_validation`] for the per-circuit length gates.
/// Set `accept_halo2_proofs` only for dry-runs against a local mock bridge
/// where the shape gates should be bypassed.
pub struct ProverProofsBlockSource {
    proofs_dir: PathBuf,
    /// When true, skip `result_<seqno>.json` verification gate.
    skip_verified_gate: bool,
    /// When true, bypass the SHPLONK shape gates — for diagnostics against a
    /// local mock bridge.
    accept_halo2_proofs: bool,
}

impl ProverProofsBlockSource {
    pub fn new(proofs_dir: impl Into<PathBuf>) -> Self {
        Self {
            proofs_dir: proofs_dir.into(),
            skip_verified_gate: false,
            accept_halo2_proofs: false,
        }
    }

    pub fn skip_verified_gate(mut self, skip: bool) -> Self {
        self.skip_verified_gate = skip;
        self
    }

    pub fn accept_halo2_proofs(mut self, accept: bool) -> Self {
        self.accept_halo2_proofs = accept;
        self
    }

    fn proof_path(&self, seq_no: u64) -> PathBuf {
        self.proofs_dir.join(format!("proof_{seq_no}.json"))
    }

    /// Find the smallest bundle seqno `N >= target` for which a
    /// `proof_<N>.json` file exists.
    ///
    /// AN key-block proofs are emitted only for key blocks (512-spaced on
    /// shellnet), so a naive `proof_{last_seen+1}.json` lookup never
    /// advances. Falling forward to the next available proof is correct
    /// for the Circuit-1A `last_seen` binding: each key-block proof bakes
    /// the *previous* key block as its `last_seen`, which is exactly the
    /// bridge's current `storedLastSeenBlockSeqNo`, so consecutive proofs
    /// chain cleanly regardless of the numeric gap.
    fn next_available_seq_no(&self, target: u64) -> Option<u64> {
        let entries = std::fs::read_dir(&self.proofs_dir).ok()?;
        let mut best: Option<u64> = None;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(rest) = name.strip_prefix("proof_") else {
                continue;
            };
            let Some(num) = rest.strip_suffix(".json") else {
                continue;
            };
            // `proof_event_*.json` / non-numeric names parse-fail and skip.
            let Ok(seq) = num.parse::<u64>() else {
                continue;
            };
            if seq >= target && best.map(|b| seq < b).unwrap_or(true) {
                best = Some(seq);
            }
        }
        best
    }

    fn result_path(&self, seq_no: u64) -> PathBuf {
        self.proofs_dir.join(format!("result_{seq_no}.json"))
    }

    fn load_block(&self, seq_no: u64) -> Result<Option<AnBlockData>, RelayerError> {
        let path = self.proof_path(seq_no);
        if !path.exists() {
            return Ok(None);
        }

        if !self.skip_verified_gate {
            let result_path = self.result_path(seq_no);
            if result_path.exists() {
                let result: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&result_path)?)?;
                let primary = result
                    .get("primary_verified")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let layer = result
                    .get("layer_verified")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if !(primary && layer) {
                    return Err(RelayerError::other(format!(
                        "result_{seq_no}.json exists but verification failed (primary={primary}, \
                         layer={layer})"
                    )));
                }
            }
        }

        let req: PartnerProofRequest = serde_json::from_slice(&std::fs::read(&path)?)
            .map_err(|e| RelayerError::other(format!("parse {}: {e}", path.display())))?;

        if req.block_seq_no as u64 != seq_no {
            return Err(RelayerError::other(format!(
                "proof file seq mismatch: path={seq_no}, json={}",
                req.block_seq_no
            )));
        }

        let primary = proof_validation::decode_verify_block_proof(
            &req.primary_proof_hex,
            FinalizationType::Primary,
            false,
            self.accept_halo2_proofs,
        )?;
        let layer = proof_validation::decode_verify_block_proof(
            &req.layer_proof_hex,
            FinalizationType::Primary,
            true,
            self.accept_halo2_proofs,
        )?;

        if req.num_layers == 0 || req.num_layers as usize > MAX_LAYER_HASHES {
            return Err(RelayerError::other(format!(
                "num_layers {} out of 1..=10",
                req.num_layers
            )));
        }

        let mut layer_hashes = [U256::ZERO; MAX_LAYER_HASHES];
        for (i, hex_fr) in req.layer_hash_frs_hex.iter().enumerate() {
            if i >= MAX_LAYER_HASHES {
                break;
            }
            layer_hashes[i] = fr_hex_to_u256(hex_fr)?;
        }

        let block = AnBlockData {
            fin_type: FinalizationType::Primary,
            // Schema v6: `block_id_hex` = raw 32-byte BE chain hash, reduced
            // into `Fr` because the adapter compares it against the proof's
            // instance byte-for-byte (see `hash_hex_to_block_id_fr`).
            block_id: hash_hex_to_block_id_fr(&req.block_id_hex)?,
            bk_set_commitment: fr_hex_to_u256(&req.bk_set_poseidon_hash_hex)?,
            block_seq_no: seq_no,
            num_layers: req.num_layers,
            layer_hashes,
            prev_max_level_layer_hash: fr_hex_to_u256(&req.prev_max_level_layer_hash_hex)?,
            attestation_proof: Bytes::from(primary),
            layer_hashes_proof: Bytes::from(layer),
        };
        block.validate_shape()?;
        Ok(Some(block))
    }
}

#[async_trait]
impl BlockSource for ProverProofsBlockSource {
    async fn fetch(&self, target_seq_no: u64) -> Result<Option<AnBlockData>, RelayerError> {
        // Fall forward to the next available key-block proof `>= target`.
        // Exact-hit (`proof_{target}.json`) is a special case of this.
        match self.next_available_seq_no(target_seq_no) {
            Some(seq_no) => self.load_block(seq_no),
            None => Ok(None),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Partner prover bk-update bundles — `proofs/bkupd_<seqno>.json`
// ─────────────────────────────────────────────────────────────────────

/// JSON written by `acki-nacki-to-eth-bridge-halo2-prover/bridge-prover-daemon`
/// (`bridge-prover-lib::ipc::BkUpdateRequest`). Schema v6: single
/// `block_id_hex` carries the raw 32-byte BE chain hash — the pre-v6 dual
/// (`block_id_hex` Fr LE + `block_id_hash_hex` raw BE) has collapsed.
#[derive(Deserialize)]
struct PartnerBkUpdateRequest {
    block_seq_no: u32,
    #[serde(default, rename = "block_height")]
    _block_height: u64,
    #[serde(default, rename = "last_seen_bk_update_seqno")]
    _last_seen_bk_update_seqno: u32,
    block_id_hex: String,
    #[serde(default = "default_attestation_primary")]
    attestation_circuit: String,
    primary_proof_hex: String,
    old_bk_set_poseidon_hash_hex: String,
    new_bk_set_poseidon_hash_hex: String,
    merkle_sibling_h01_hex: String,
    merkle_sibling_h4_7_hex: String,
    merkle_sibling_h8_15_hex: String,
}

fn default_attestation_primary() -> String {
    "primary".to_string()
}

/// Reads BK-set rotation bundles from the partner prover's `proofs/` directory.
pub struct BkUpdateProofsSource {
    proofs_dir: PathBuf,
    skip_verified_gate: bool,
    accept_halo2_proofs: bool,
}

impl BkUpdateProofsSource {
    pub fn new(proofs_dir: impl Into<PathBuf>) -> Self {
        Self {
            proofs_dir: proofs_dir.into(),
            skip_verified_gate: false,
            accept_halo2_proofs: false,
        }
    }

    pub fn skip_verified_gate(mut self, skip: bool) -> Self {
        self.skip_verified_gate = skip;
        self
    }

    pub fn accept_halo2_proofs(mut self, accept: bool) -> Self {
        self.accept_halo2_proofs = accept;
        self
    }

    fn bkupd_path(&self, seq_no: u64) -> PathBuf {
        self.proofs_dir.join(format!("bkupd_{seq_no:06}.json"))
    }

    fn bkupd_result_path(&self, seq_no: u64) -> PathBuf {
        self.proofs_dir
            .join(format!("bkupd_result_{seq_no:06}.json"))
    }

    /// Load a single bk-update bundle keyed by `block_seq_no`.
    pub fn load_update(
        &self,
        seq_no: u64,
    ) -> Result<Option<crate::types::BkSetUpdateData>, RelayerError> {
        let path = self.bkupd_path(seq_no);
        if !path.exists() {
            return Ok(None);
        }

        if !self.skip_verified_gate {
            let result_path = self.bkupd_result_path(seq_no);
            if result_path.exists() {
                let result: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&result_path)?)?;
                let verify_ok = result
                    .get("verify_ok")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if !verify_ok {
                    return Err(RelayerError::other(format!(
                        "bkupd_result_{seq_no:06}.json exists but verify_ok=false"
                    )));
                }
            }
        }

        let req: PartnerBkUpdateRequest = serde_json::from_slice(&std::fs::read(&path)?)
            .map_err(|e| RelayerError::other(format!("parse {}: {e}", path.display())))?;

        if req.block_seq_no as u64 != seq_no {
            return Err(RelayerError::other(format!(
                "bkupd file seq mismatch: path={seq_no}, json={}",
                req.block_seq_no
            )));
        }

        let fin_type = match req.attestation_circuit.as_str() {
            "primary" | "Primary" | "1a" => FinalizationType::Primary,
            "fallback" | "Fallback" | "1b" => FinalizationType::Fallback,
            other => {
                return Err(RelayerError::other(format!(
                    "unknown attestation_circuit {other:?}; expected primary|fallback"
                )));
            },
        };

        let attestation_proof = proof_validation::decode_verify_block_proof(
            &req.primary_proof_hex,
            fin_type,
            false,
            self.accept_halo2_proofs,
        )?;

        fn hex32_to_array(hex_str: &str, label: &str) -> Result<[u8; 32], RelayerError> {
            let raw = decode_hex(hex_str)?;
            if raw.len() != 32 {
                return Err(RelayerError::other(format!(
                    "{label} must be 32 bytes, got {}",
                    raw.len()
                )));
            }
            let mut out = [0u8; 32];
            out.copy_from_slice(&raw);
            Ok(out)
        }

        Ok(Some(crate::types::BkSetUpdateData {
            fin_type,
            // Same convention as the bundle path above: the `Fr` image, which
            // is also what the contract's reduced Merkle root is compared
            // against inside `applyBkSetUpdate`.
            block_id: hash_hex_to_block_id_fr(&req.block_id_hex)?,
            block_seq_no: seq_no,
            old_commitment_l2: fr_hex_to_u256(&req.old_bk_set_poseidon_hash_hex)?,
            new_commitment_l3: fr_hex_to_u256(&req.new_bk_set_poseidon_hash_hex)?,
            sibling_h01: hex32_to_array(&req.merkle_sibling_h01_hex, "merkle_sibling_h01")?,
            sibling_h4_7: hex32_to_array(&req.merkle_sibling_h4_7_hex, "merkle_sibling_h4_7")?,
            sibling_h8_15: hex32_to_array(&req.merkle_sibling_h8_15_hex, "merkle_sibling_h8_15")?,
            attestation_proof: Bytes::from(attestation_proof),
        }))
    }
}

#[async_trait]
pub trait BkUpdateSource: Send + Sync {
    async fn fetch_bk_update(
        &self,
        target_seq_no: u64,
    ) -> Result<Option<crate::types::BkSetUpdateData>, RelayerError>;

    /// Acknowledge that a BK-set update was applied on-chain. Default no-op.
    async fn ack_last_bk_update(&self, _seq_no: u64) -> Result<(), RelayerError> {
        Ok(())
    }
}

#[async_trait]
impl BkUpdateSource for BkUpdateProofsSource {
    async fn fetch_bk_update(
        &self,
        target_seq_no: u64,
    ) -> Result<Option<crate::types::BkSetUpdateData>, RelayerError> {
        self.load_update(target_seq_no)
    }
}

/// Always-empty BK-update source for file-driven / unit-test relayers that
/// only exercise the verifyBlock lane.
#[derive(Debug, Default, Clone, Copy)]
pub struct EmptyBkUpdateSource;

#[async_trait]
impl BkUpdateSource for EmptyBkUpdateSource {
    async fn fetch_bk_update(
        &self,
        _target_seq_no: u64,
    ) -> Result<Option<crate::types::BkSetUpdateData>, RelayerError> {
        Ok(None)
    }
}

fn decode_hex(s: &str) -> Result<Vec<u8>, RelayerError> {
    let trimmed = s.trim();
    let no_prefix = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    hex::decode(no_prefix).map_err(|e| RelayerError::other(format!("bad hex proof: {e}")))
}

fn parse_dec_u256(s: &str) -> Result<U256, RelayerError> {
    use std::str::FromStr;
    // alloy's `U256` implements `FromStr` (base 10). The old ethers helper
    // `U256::from_dec_str` is replaced by this trait method.
    U256::from_str(s.trim()).map_err(|e| RelayerError::other(format!("bad decimal U256: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AnBlockData, FinalizationType};

    fn dummy_block(seq: u64) -> AnBlockData {
        let mut layer_hashes = [U256::ZERO; MAX_LAYER_HASHES];
        layer_hashes[0] = U256::from(seq * 2 + 1);
        AnBlockData {
            fin_type: FinalizationType::Primary,
            block_id: U256::from(seq),
            bk_set_commitment: U256::from(0xBE5E7u64),
            block_seq_no: seq,
            num_layers: 1,
            layer_hashes,
            prev_max_level_layer_hash: U256::ZERO,
            attestation_proof: Bytes::from(vec![0u8; 256]),
            layer_hashes_proof: Bytes::from(vec![0u8; 256]),
        }
    }

    #[tokio::test]
    async fn in_memory_source_returns_none_when_missing() {
        let src = InMemoryBlockSource::new();
        assert!(src.fetch(1).await.unwrap().is_none());
        src.insert(dummy_block(2));
        assert!(src.fetch(1).await.unwrap().is_none());
        assert!(src.fetch(2).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn fixtures_source_only_serves_its_seqno() {
        // Construct a synthetic on-disk layout in a tempdir.
        let dir = tempfile::tempdir().unwrap();
        let primary = dir.path().join("primary");
        let lh = dir.path().join("layer-hashes");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&lh).unwrap();
        std::fs::write(
            primary.join("groth16_output.json"),
            r#"{"proof":"0xdeadbeef"}"#,
        )
        .unwrap();
        std::fs::write(lh.join("groth16_output.json"), r#"{"proof":"0xcafebabe"}"#).unwrap();
        std::fs::write(
            dir.path().join("bound_scenario.json"),
            serde_json::to_string(&serde_json::json!({
                "bk_set_size": 5,
                "num_layers": 1,
                "num_chain_steps": 1,
                "block_seq_no": 1,
                "last_seen_block_seqno": 0,
                "block_id_decimal": "42",
                "bk_set_poseidon_decimal": "777",
                "layer_hash_decimals": ["111","0","0","0","0","0","0","0","0","0"],
                "prev_max_level_layer_hash_decimal": "0",
                "primary_proof_bytes": 4,
                "layer_hashes_proof_bytes": 4
            }))
            .unwrap(),
        )
        .unwrap();

        let src = FixturesBlockSource::from_dir(dir.path()).unwrap();
        assert!(src.fetch(0).await.unwrap().is_none());
        let b = src.fetch(1).await.unwrap().unwrap();
        assert_eq!(b.block_seq_no, 1);
        assert_eq!(b.block_id, U256::from(42));
        assert_eq!(b.layer_hashes[0], U256::from(111));
        assert!(src.fetch(2).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn prover_proofs_source_loads_shplonk_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let proof = serde_json::json!({
            "schema_version": 2,
            "block_seq_no": 512,
            "block_height": 512,
            "last_seen_block_seqno": 0,
            "block_id_hex": "0200000000000000000000000000000000000000000000000000000000000000",
            "primary_proof_hex": "0x".to_string() + &"ab".repeat(2048),
            "layer_proof_hex": "0x".to_string() + &"cd".repeat(2048),
            "bk_set_poseidon_hash_hex": "0400000000000000000000000000000000000000000000000000000000000000",
            "num_layers": 1,
            "layer_hash_frs_hex": [
                "0500000000000000000000000000000000000000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000"
            ],
            "prev_max_level_layer_hash_hex": "0000000000000000000000000000000000000000000000000000000000000000"
        });
        std::fs::write(
            dir.path().join("proof_512.json"),
            serde_json::to_string(&proof).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("result_512.json"),
            r#"{"block_seq_no":512,"primary_verified":true,"layer_verified":true,"error":null}"#,
        )
        .unwrap();

        let src = ProverProofsBlockSource::new(dir.path());
        let b = src.fetch(512).await.unwrap().unwrap();
        assert_eq!(b.block_seq_no, 512);
        assert_eq!(b.attestation_proof.len(), 2048);
    }

    fn write_bundle_proof(dir: &std::path::Path, seq_no: u64) {
        let proof = serde_json::json!({
            "schema_version": 2,
            "block_seq_no": seq_no,
            "block_height": seq_no,
            "last_seen_block_seqno": 0,
            "block_id_hex": "0200000000000000000000000000000000000000000000000000000000000000",
            "primary_proof_hex": "0x".to_string() + &"ab".repeat(2048),
            "layer_proof_hex": "0x".to_string() + &"cd".repeat(2048),
            "bk_set_poseidon_hash_hex": "0400000000000000000000000000000000000000000000000000000000000000",
            "num_layers": 1,
            "layer_hash_frs_hex": vec![
                "0500000000000000000000000000000000000000000000000000000000000000".to_string(),
            ],
            "prev_max_level_layer_hash_hex": "0000000000000000000000000000000000000000000000000000000000000000"
        });
        std::fs::write(
            dir.join(format!("proof_{seq_no}.json")),
            serde_json::to_string(&proof).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn prover_proofs_source_falls_forward_to_next_key_block() {
        // AN emits key-block proofs 512-spaced; the relayer's cursor
        // (`last_seen + 1`) never lands exactly on a proof file, so the
        // source must fall forward to the next available one.
        let dir = tempfile::tempdir().unwrap();
        write_bundle_proof(dir.path(), 1_084_416);
        write_bundle_proof(dir.path(), 1_084_928);

        let src = ProverProofsBlockSource::new(dir.path()).skip_verified_gate(true);

        // Cursor just past a previous key block → next available is 1_084_416.
        let b = src.fetch(1_083_905).await.unwrap().unwrap();
        assert_eq!(b.block_seq_no, 1_084_416);

        // Cursor just past 1_084_416 → next available is 1_084_928.
        let b = src.fetch(1_084_417).await.unwrap().unwrap();
        assert_eq!(b.block_seq_no, 1_084_928);

        // Exact hit still works.
        let b = src.fetch(1_084_416).await.unwrap().unwrap();
        assert_eq!(b.block_seq_no, 1_084_416);

        // Past the last proof → nothing available.
        assert!(src.fetch(1_084_929).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn bkupd_source_parses_partner_json() {
        let dir = tempfile::tempdir().unwrap();
        let proof_hex = "0x".to_string() + &"ab".repeat(2048);
        let bkupd = serde_json::json!({
            "schema_version": 7,
            "block_seq_no": 24,
            "attestation_circuit": "primary",
            "block_id_hex": "0100000000000000000000000000000000000000000000000000000000000000",
            "primary_proof_hex": proof_hex,
            "old_bk_set_poseidon_hash_hex": "0200000000000000000000000000000000000000000000000000000000000000",
            "new_bk_set_poseidon_hash_hex": "0300000000000000000000000000000000000000000000000000000000000000",
            "merkle_sibling_h01_hex": "0x".to_string() + &"aa".repeat(32),
            "merkle_sibling_h4_7_hex": "0x".to_string() + &"bb".repeat(32),
            "merkle_sibling_h8_15_hex": "0x".to_string() + &"cc".repeat(32),
        });
        std::fs::write(
            dir.path().join("bkupd_000024.json"),
            serde_json::to_string(&bkupd).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("bkupd_result_000024.json"),
            r#"{"block_seq_no":24,"verify_ok":true}"#,
        )
        .unwrap();

        let src = BkUpdateProofsSource::new(dir.path());
        let u = src.fetch_bk_update(24).await.unwrap().unwrap();
        assert_eq!(u.block_seq_no, 24);
        assert_eq!(u.fin_type, FinalizationType::Primary);
        assert_eq!(u.attestation_proof.len(), 2048);
        assert_eq!(u.sibling_h01[0], 0xaa);
        assert_eq!(u.sibling_h4_7[0], 0xbb);
        assert_eq!(u.sibling_h8_15[0], 0xcc);
    }
}
