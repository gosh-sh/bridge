//! Prover-private BK pubkey table.
//!
//! Holds the `signer_index → 48-byte compressed BLS pubkey` map that the
//! prover needs to construct Circuit 1a/1b witnesses (the circuit consumes
//! the pubkey bytes of each signer to do the BLS aggregation check).
//!
//! This file is **prover-only** — the verifier daemon (which models the
//! future ETH bridge contract) NEVER loads it, because the contract will
//! only ever store the **commitment**, not the full pubkey table. The
//! commitment lives in `BridgeState::stored_bk_set_commitment`.
//!
//! At every checkpoint the prover must satisfy:
//! ```text
//! Poseidon( ProverBkSet::pubkeys ) == BridgeState::stored_bk_set_commitment
//! ```
//! This is enforced by [`ProverBkSet::check_matches`] and is the only thing
//! that ties the prover's private working set to the contract-mirror state.
//!
//! Persistence: JSON at `state/prover_bk_set.json`, written atomically via
//! `tmp + rename` (same pattern as `BridgeState::save`).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::bridge_state::BridgeState;
use crate::poseidon;

/// Current `ProverBkSet` schema version. Bumped if the on-disk shape changes.
pub const PROVER_BK_SET_SCHEMA_VERSION: u32 = 1;

fn default_schema_version() -> u32 { PROVER_BK_SET_SCHEMA_VERSION }

/// On-disk format for the prover-private pubkey table. Keys are
/// `signer_index`; values are 48-byte compressed BLS G1 pubkeys serialised
/// as lowercase hex strings for readability.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProverBkSet {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    /// Poseidon commitment of `pubkeys`. Must agree with
    /// `BridgeState::stored_bk_set_commitment` at every read site.
    pub commitment: [u8; 32],

    /// `signer_index -> 48-byte compressed BLS pubkey` (hex-encoded in JSON,
    /// `BTreeMap` so the serialised form is deterministic).
    pub pubkeys_hex: BTreeMap<u16, String>,

    /// seq_no of the last bk-update block applied to this table. Zero on
    /// first bootstrap.
    pub last_applied_update_seq_no: u64,
}

impl ProverBkSet {
    /// Build from a fresh `HashMap` (e.g. the output of
    /// `bk_set_fetcher::fetch_bk_set`). Computes the Poseidon commitment
    /// from the provided pubkeys.
    pub fn from_pubkeys(
        pubkeys: &HashMap<u16, Vec<u8>>,
        last_applied_update_seq_no: u64,
    ) -> Self {
        let (_fr, commitment) = poseidon::compute_bk_set_poseidon(pubkeys);
        let pubkeys_hex = pubkeys
            .iter()
            .map(|(idx, pk)| (*idx, hex::encode(pk)))
            .collect();
        Self {
            schema_version: PROVER_BK_SET_SCHEMA_VERSION,
            commitment,
            pubkeys_hex,
            last_applied_update_seq_no,
        }
    }

    /// Decode `pubkeys_hex` into a `HashMap<u16, Vec<u8>>` suitable for
    /// passing to the prover / Circuit 1 witness builder.
    pub fn pubkeys(&self) -> anyhow::Result<HashMap<u16, Vec<u8>>> {
        let mut out = HashMap::with_capacity(self.pubkeys_hex.len());
        for (idx, hex_str) in &self.pubkeys_hex {
            let bytes = hex::decode(hex_str)
                .with_context(|| format!("invalid hex for signer {idx}"))?;
            anyhow::ensure!(
                bytes.len() == 48,
                "pubkey for signer {idx} has {} bytes, expected 48",
                bytes.len()
            );
            out.insert(*idx, bytes);
        }
        Ok(out)
    }

    /// Sanity check against the contract-mirror state. Called at every read
    /// site: the commitment stored in `BridgeState` is authoritative; if our
    /// pubkey table no longer hashes to it, we are out of sync and must
    /// re-bootstrap the table from `bkSetUpdates`.
    pub fn check_matches(&self, contract_state: &BridgeState) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.commitment == contract_state.stored_bk_set_commitment,
            "prover_bk_set commitment {} does not match contract-mirror commitment {}",
            hex::encode(self.commitment),
            hex::encode(contract_state.stored_bk_set_commitment),
        );
        // Re-derive the commitment from `pubkeys_hex` and require equality
        // with the stored field — guards against a hand-edited or
        // corrupted JSON.
        let pubkeys = self.pubkeys()?;
        let (_fr, recomputed) = poseidon::compute_bk_set_poseidon(&pubkeys);
        anyhow::ensure!(
            recomputed == self.commitment,
            "prover_bk_set self-inconsistent: pubkeys hash to {} but stored commitment is {}",
            hex::encode(recomputed),
            hex::encode(self.commitment),
        );
        Ok(())
    }

    /// Rotate to a new pubkey set after a verified bk-update bundle.
    ///
    /// Caller is responsible for verifying that `Poseidon(new_pubkeys) ==
    /// new_commitment` BEFORE calling this — `rotate` only updates the
    /// in-memory state. (It does re-derive the commitment as a defensive
    /// check, but the trust anchor is the caller's match against L3.)
    pub fn rotate(
        &mut self,
        new_commitment: [u8; 32],
        new_pubkeys: HashMap<u16, Vec<u8>>,
        update_block_seq_no: u64,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            update_block_seq_no > self.last_applied_update_seq_no,
            "rotate seq_no {} is not strictly greater than last applied {}",
            update_block_seq_no,
            self.last_applied_update_seq_no,
        );
        let (_fr, recomputed) = poseidon::compute_bk_set_poseidon(&new_pubkeys);
        anyhow::ensure!(
            recomputed == new_commitment,
            "rotate: Poseidon(new_pubkeys) = {} != declared new commitment {}",
            hex::encode(recomputed),
            hex::encode(new_commitment),
        );
        self.commitment = new_commitment;
        self.pubkeys_hex = new_pubkeys
            .iter()
            .map(|(idx, pk)| (*idx, hex::encode(pk)))
            .collect();
        self.last_applied_update_seq_no = update_block_seq_no;
        Ok(())
    }

    /// Load from disk. Returns `Ok(None)` if the file does not exist (lets
    /// the caller decide between "first bootstrap" and "re-derive from
    /// bkSetUpdates").
    pub fn load(path: &str) -> anyhow::Result<Option<Self>> {
        if !Path::new(path).exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {path}"))?;
        let v: Self = serde_json::from_str(&data)
            .with_context(|| format!("failed to parse {path}"))?;
        anyhow::ensure!(
            v.schema_version == PROVER_BK_SET_SCHEMA_VERSION,
            "prover_bk_set at {} has schema_version={} but daemon expects {}",
            path,
            v.schema_version,
            PROVER_BK_SET_SCHEMA_VERSION,
        );
        Ok(Some(v))
    }

    /// Atomic JSON save (`tmp + rename`).
    pub fn save(&self, path: &str) -> anyhow::Result<()> {
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let json = serde_json::to_string_pretty(self)?;
        let tmp = format!("{path}.tmp");
        std::fs::write(&tmp, json).with_context(|| format!("failed to write {tmp}"))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("failed to rename {tmp} -> {path}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_test_data_gen::generator::{build_bk_set_map, generate_bls_keypairs};

    /// Generate `n` *valid* BLS pubkeys (real keypairs from
    /// `bridge-test-data-gen`). Required because `compute_bk_set_poseidon`
    /// deserialises pubkeys as G1 points and panics on garbage bytes.
    fn real_bk_set(n: usize) -> std::collections::HashMap<u16, Vec<u8>> {
        build_bk_set_map(&generate_bls_keypairs(n))
    }

    #[test]
    fn check_matches_passes_when_in_sync() {
        let pubkeys = real_bk_set(5);
        let pbs = ProverBkSet::from_pubkeys(&pubkeys, 0);
        let mut state = BridgeState::new(8);
        state.stored_bk_set_commitment = pbs.commitment;
        pbs.check_matches(&state).unwrap();
    }

    #[test]
    fn check_matches_fails_on_commitment_drift() {
        let pubkeys = real_bk_set(5);
        let pbs = ProverBkSet::from_pubkeys(&pubkeys, 0);
        let mut state = BridgeState::new(8);
        state.stored_bk_set_commitment = [42u8; 32];
        let err = pbs.check_matches(&state).unwrap_err();
        assert!(format!("{err}").contains("does not match contract-mirror"));
    }

    #[test]
    fn rotate_replaces_pubkeys_and_commitment() {
        let pk_old = real_bk_set(3);
        let mut pbs = ProverBkSet::from_pubkeys(&pk_old, 0);

        let pk_new = real_bk_set(5);
        let (_fr, new_c) = poseidon::compute_bk_set_poseidon(&pk_new);

        pbs.rotate(new_c, pk_new.clone(), 1024).unwrap();
        assert_eq!(pbs.commitment, new_c);
        assert_eq!(pbs.last_applied_update_seq_no, 1024);
        assert_eq!(pbs.pubkeys().unwrap().len(), 5);
    }

    #[test]
    fn rotate_rejects_mismatched_commitment() {
        let pk_old = real_bk_set(3);
        let mut pbs = ProverBkSet::from_pubkeys(&pk_old, 0);

        let pk_new = real_bk_set(5);
        let bogus = [123u8; 32];
        let err = pbs.rotate(bogus, pk_new, 1024).unwrap_err();
        assert!(format!("{err}").contains("declared new commitment"));
    }

    #[test]
    fn rotate_rejects_replay() {
        let pk_old = real_bk_set(3);
        let mut pbs = ProverBkSet::from_pubkeys(&pk_old, 1024);

        let pk_new = real_bk_set(3);
        let (_fr, new_c) = poseidon::compute_bk_set_poseidon(&pk_new);
        let err = pbs.rotate(new_c, pk_new, 1024).unwrap_err();
        assert!(format!("{err}").contains("not strictly greater"));
    }

    #[test]
    fn load_save_roundtrip() {
        let pubkeys = real_bk_set(4);
        let pbs = ProverBkSet::from_pubkeys(&pubkeys, 42);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prover_bk_set.json");
        let path_str = path.to_str().unwrap();
        pbs.save(path_str).unwrap();

        let loaded = ProverBkSet::load(path_str).unwrap().unwrap();
        assert_eq!(loaded.commitment, pbs.commitment);
        assert_eq!(loaded.last_applied_update_seq_no, 42);
        assert_eq!(loaded.pubkeys_hex.len(), 4);
    }

    #[test]
    fn load_returns_none_for_missing() {
        let r = ProverBkSet::load("/nonexistent/prover_bk_set.json").unwrap();
        assert!(r.is_none());
    }
}
