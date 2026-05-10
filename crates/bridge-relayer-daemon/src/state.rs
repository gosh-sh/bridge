//! On-disk relayer state for crash-recovery.
//!
//! The single source of truth for "did this block reach the bridge?" is
//! the bridge contract itself (`storedLastSeenBlockSeqNo`); the local
//! state is a *cache* + a *retry log*. Specifically:
//!
//! - `last_processed_seqno` mirrors the bridge's `storedLastSeenBlockSeqNo`
//!   after every successful submission, so we can detect divergence at
//!   startup (e.g. another relayer instance advanced the bridge past us).
//! - `last_attempt_seqno` records the seqno of the most recent attempt,
//!   regardless of outcome. Used for metrics and to spot persistent
//!   failures.
//! - `attempts_since_progress` increments on every non-success outcome
//!   and resets after a successful submission; it caps the relayer's
//!   willingness to spin on one block.
//!
//! Concurrency note: the relayer is intended to be a single-process
//! daemon; the state file is overwritten atomically (write-temp-then-
//! rename) on each update. Multi-instance HA is **not** supported in
//! Phase 5.1.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::RelayerError;

/// Locally-persisted relayer progress.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RelayerState {
    /// Last seqno *acknowledged on-chain* by the bridge. `None` before
    /// the first submission of this relayer's lifetime.
    pub last_processed_seqno: Option<u64>,
    /// Most recent seqno attempted (may be ahead of `last_processed_seqno`
    /// if the latest attempt failed).
    pub last_attempt_seqno: Option<u64>,
    /// Consecutive non-success outcomes for the current target. Reset
    /// to 0 on every accepted block.
    pub attempts_since_progress: u32,
}

impl RelayerState {
    /// Read the state file. Returns `Ok(None)` if the file does not
    /// exist (fresh start), `Err` on corruption.
    pub fn load(path: &Path) -> Result<Option<Self>, RelayerError> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomically persist to disk: write to `<path>.tmp`, then rename.
    /// Best-effort durability — we don't fsync here; a real production
    /// relayer would.
    pub fn save(&self, path: &Path) -> Result<(), RelayerError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = sibling_tmp_path(path);
        let bytes = serde_json::to_vec_pretty(self)?;
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Mark a successful submission of `seqno`. Resets failure counter.
    pub fn record_progress(&mut self, seqno: u64) {
        self.last_processed_seqno = Some(seqno);
        self.last_attempt_seqno = Some(seqno);
        self.attempts_since_progress = 0;
    }

    /// Mark an attempt that did not advance the on-chain anchor (could be
    /// "not yet available", a verifier-rejection, or an RPC error).
    pub fn record_attempt(&mut self, seqno: u64) {
        self.last_attempt_seqno = Some(seqno);
        self.attempts_since_progress = self.attempts_since_progress.saturating_add(1);
    }
}

fn sibling_tmp_path(path: &Path) -> PathBuf {
    let mut tmp = path.to_path_buf();
    let mut name = path
        .file_name()
        .map(|s| s.to_owned())
        .unwrap_or_else(|| std::ffi::OsString::from("state.json"));
    name.push(".tmp");
    tmp.set_file_name(name);
    tmp
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn roundtrip_persists_state() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");

        assert_eq!(RelayerState::load(&path).unwrap(), None);

        let mut s = RelayerState::default();
        s.record_attempt(7);
        s.record_progress(7);
        s.save(&path).unwrap();

        let loaded = RelayerState::load(&path).unwrap().unwrap();
        assert_eq!(loaded.last_processed_seqno, Some(7));
        assert_eq!(loaded.last_attempt_seqno, Some(7));
        assert_eq!(loaded.attempts_since_progress, 0);
    }

    #[test]
    fn record_attempt_increments_counter_until_progress() {
        let mut s = RelayerState::default();
        s.record_attempt(3);
        s.record_attempt(3);
        s.record_attempt(3);
        assert_eq!(s.attempts_since_progress, 3);
        s.record_progress(3);
        assert_eq!(s.attempts_since_progress, 0);
    }
}
