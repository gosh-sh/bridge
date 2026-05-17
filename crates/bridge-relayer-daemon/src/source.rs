//! [`BlockSource`] — abstraction over "where do AN blocks come from?".
//!
//! Phase 5.1 ships:
//! - [`InMemoryBlockSource`] — pre-baked map keyed by seqNo. Used by the unit
//!   tests in this crate to drive multi-block scenarios in < 10 ms each.
//! - [`FixturesBlockSource`] — reads pre-generated bound proof artefacts from
//!   disk (the Phase 4.1 outputs of
//!   `bridge-prover-orchestrator/proofs/bound/...` plus the gnark JSON produced
//!   by `circuit-1a/circuit-2`). Yields a single canned block keyed by
//!   `block_seq_no = 1`.
//!
//! Phase 5.2 will add `LiveBlockSource` backed by GraphQL + BOC parsing
//! + halo2 + gnark.

use std::{collections::BTreeMap, path::Path, sync::Mutex};

use alloy::primitives::{Bytes, U256};
use async_trait::async_trait;
use serde::Deserialize;

use crate::{
    error::RelayerError,
    types::{AnBlockData, FinalizationType, MAX_LAYER_HASHES},
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
/// `bridge-prover-orchestrator/proofs/bound/{primary,layer-hashes}` plus
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
    /// Build from a directory layout produced by Phase 4.1:
    ///
    /// ```text
    ///   <dir>/bound_scenario.json
    ///   <dir>/primary/groth16_output.json
    ///   <dir>/layer-hashes/groth16_output.json
    /// ```
    ///
    /// The scenario is unconditionally tagged as Primary (the Phase 4.1
    /// fixture binary doesn't generate a Fallback wrap). For Fallback
    /// smoke testing, swap the proof file at runtime via
    /// `with_fin_type` / `with_attestation_proof` — kept off for now to
    /// avoid cargo-culting an API ahead of Phase 5.2.
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
// Helpers
// ─────────────────────────────────────────────────────────────────────

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
}
