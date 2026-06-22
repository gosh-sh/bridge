
//! Genesis-seed plumbing for the on-disk state mirror.
//!
//! In the production analog, the Ethereum bridge contract receives its genesis
//! `GlobalHistoryData` once at deployment via constructor arguments — the
//! deployer is trusted to provide the first key block's layer hashes, height,
//! seq_no, and the active BK-set commitment. From that moment on, the contract
//! advances exclusively through verified proofs.
//!
//! Here the prover daemon plays the role of "deployer": it fetches the first
//! key block envelope from the node, derives the seed, applies it to its own
//! `BridgeState`, **and persists the seed as a JSON file**. The verifier
//! daemon, which has no node connection, then loads that file on cold start
//! and applies the same seed — guaranteeing that both mirrors share the same
//! anchor point (the first key block at seq_no = W, one entry per active
//! layer) and from there advance in lockstep via verified proofs. (Phase 1
//! gap: the prover had this anchor in state but the verifier did not, so the
//! verifier started one key block behind.)
//!
//! The seed file is written **once** on cold start. Subsequent restarts pick
//! up persisted `BridgeState` directly and never re-read the seed.
//!
//! Wire format is JSON with explicit `schema_version`. `[u8; 32]` fields
//! serialize as arrays of 32 ints, matching the existing `BridgeState`
//! serialization so the two files are eyeball-comparable.

use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::bridge_state::BridgeState;

/// Current schema version of the on-disk seed file. Bump when the layout of
/// `BootstrapSeed` changes in a non-additive way.
pub const SEED_SCHEMA_VERSION: u32 = 1;

/// Default path for the persisted seed. Daemons may override but typically
/// both read/write the same `state/bootstrap_seed.json`.
pub const DEFAULT_SEED_PATH: &str = "./state/bootstrap_seed.json";

/// Genesis seed for `BridgeState`.
///
/// Mirrors the constructor arguments of the on-chain bridge contract:
/// the per-layer hashes published by the first key block, that block's
/// height + seq_no, and the BK-set commitment in effect at that height.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BootstrapSeed {
    /// Wire-format version. Currently always [`SEED_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// `(root_hash, layer)` pairs from `common_section.history_proofs`.
    /// Order does not matter — `BridgeState::append_bundle` routes each pair
    /// into its own per-layer window.
    pub layer_hashes: Vec<([u8; 32], u8)>,
    /// `BlockHeight.height` of the first key block (authoritative; not
    /// `seq_no`). On a single-thread testbed `block_height == seq_no`.
    pub block_height: u64,
    /// `seq_no` of the first key block. On `poseidon_dex` this is W (= 8).
    pub block_seq_no: u64,
    /// Poseidon commitment of the BK set active at the seed block.
    pub bk_set_commitment: [u8; 32],
}

impl BootstrapSeed {
    /// Apply this seed to `state` exactly as if a verified key block had
    /// arrived: append each per-layer hash, update cursors, mark initialized.
    ///
    /// Idempotent on a freshly-constructed `BridgeState`; calling it on an
    /// already-initialized state advances cursors and will *not* unwind
    /// existing history, so callers should guard with `!state.initialized`.
    pub fn apply(&self, state: &mut BridgeState) {
        state.append_bundle(
            &self.layer_hashes,
            self.block_height,
            self.block_seq_no,
            self.bk_set_commitment,
        );
    }

    /// Atomically persist the seed to `path` (write `.tmp`, then `rename`).
    /// Same pattern as `BridgeState::save` — concurrent readers see either
    /// the previous or the new file, never a torn one.
    pub fn save(&self, path: &str) -> anyhow::Result<()> {
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let json = serde_json::to_string_pretty(self)?;
        let tmp = format!("{}.tmp", path);
        std::fs::write(&tmp, json).with_context(|| format!("failed to write {}", tmp))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("failed to rename {} -> {}", tmp, path))?;
        Ok(())
    }

    /// Load a previously-written seed file. Returns `Ok(None)` when the file
    /// does not exist (typical on a fresh test rig before the prover has run);
    /// returns `Err` only on I/O or parse failure.
    pub fn load(path: &str) -> anyhow::Result<Option<Self>> {
        if !Path::new(path).exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read seed file {}", path))?;
        let seed: BootstrapSeed = serde_json::from_str(&data)
            .with_context(|| format!("failed to parse seed file {}", path))?;
        if seed.schema_version != SEED_SCHEMA_VERSION {
            anyhow::bail!(
                "bootstrap seed at {} has schema_version={} but daemon expects {}",
                path,
                seed.schema_version,
                SEED_SCHEMA_VERSION
            );
        }
        Ok(Some(seed))
    }
}

/// Build a [`BootstrapSeed`] from the first key block envelope fetched via
/// GraphQL. Used by the prover daemon; the verifier loads the saved file
/// instead of calling this directly.
///
/// `first_key_seqno` must be the seq_no of the first key block (= `W` on a
/// single-thread testbed). `bk_set_commitment` is computed by the caller from
/// the BK set in effect at startup.
pub async fn fetch_from_node(
    gql: &crate::gql_client::GqlClient,
    first_key_seqno: u64,
    bk_set_commitment: [u8; 32],
) -> anyhow::Result<BootstrapSeed> {
    let block = gql
        .query_proof_block_by_seqno(first_key_seqno)
        .await
        .with_context(|| {
            format!("could not fetch first key block at seq_no={}", first_key_seqno)
        })?;
    let block_height = block.height;
    let layer_hashes: Vec<([u8; 32], u8)> = block
        .history_proofs
        .iter()
        .map(|(&layer, root)| (*root, layer))
        .collect();
    Ok(BootstrapSeed {
        schema_version: SEED_SCHEMA_VERSION,
        layer_hashes,
        block_height,
        block_seq_no: first_key_seqno,
        bk_set_commitment,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_marks_state_initialized() {
        let mut state = BridgeState::new(8);
        let seed = BootstrapSeed {
            schema_version: SEED_SCHEMA_VERSION,
            layer_hashes: vec![([7u8; 32], 1)],
            block_height: 8,
            block_seq_no: 8,
            bk_set_commitment: [9u8; 32],
        };
        assert!(!state.initialized);
        seed.apply(&mut state);
        assert!(state.initialized);
        assert_eq!(state.stored_last_seen_block_seq_no, 8);
        assert_eq!(state.stored_last_seen_block_height, 8);
        assert_eq!(state.stored_bk_set_commitment, [9u8; 32]);
        assert_eq!(state.window(1).data_len, 1);
        assert_eq!(state.window(1).latest(), Some([7u8; 32]));
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join("bootstrap_seed_roundtrip");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("seed.json");
        let path_s = path.to_str().unwrap();

        let seed = BootstrapSeed {
            schema_version: SEED_SCHEMA_VERSION,
            layer_hashes: vec![([1u8; 32], 1), ([2u8; 32], 2)],
            block_height: 8,
            block_seq_no: 8,
            bk_set_commitment: [3u8; 32],
        };
        seed.save(path_s).unwrap();
        let loaded = BootstrapSeed::load(path_s).unwrap().unwrap();
        assert_eq!(seed, loaded);
    }

    #[test]
    fn load_missing_returns_none() {
        let path = std::env::temp_dir().join("definitely_not_present_bootstrap_seed.json");
        let _ = std::fs::remove_file(&path);
        assert!(BootstrapSeed::load(path.to_str().unwrap()).unwrap().is_none());
    }
}
