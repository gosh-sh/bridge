//! On-disk relayer state for crash-recovery.
//!
//! The authoritative "was this deposit finalized?" record lives on the AN
//! side as the `usedDepositIds` nullifier set; the local state is a *cache*
//! plus a *retry log* so the relayer can resume near where it left off
//! without re-proving everything from `depositId = 0`:
//!
//! - `last_processed_deposit_id` mirrors the highest `depositId` we observed
//!   finalized on AN (either by our own submission or by `is_finalized`).
//! - `last_attempt_deposit_id` records the most recent attempt regardless of
//!   outcome.
//! - `attempts_since_progress` increments on every non-success outcome and
//!   resets on a finalize; it caps the relayer's willingness to spin on one
//!   deposit.
//!
//! Concurrency note: like the AN→ETH relayer, this is a single-process
//! daemon. The file is overwritten atomically (write-temp-then-rename). The
//! AN-side nullifier makes double-submission *safe* (the second submit is
//! rejected), so even a stale local state can't cause a double-spend.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::RelayerError;

/// Locally-persisted relayer progress.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RelayerState {
    /// Highest `depositId` known finalized on AN. `None` before the first
    /// finalize of this relayer's lifetime.
    pub last_processed_deposit_id: Option<u64>,
    /// Most recent `depositId` attempted (may be ahead of
    /// `last_processed_deposit_id` if the latest attempt failed).
    pub last_attempt_deposit_id: Option<u64>,
    /// Consecutive non-success outcomes for the current target. Reset to 0
    /// on every finalized deposit.
    pub attempts_since_progress: u32,
}

impl RelayerState {
    /// Read the state file. Returns `Ok(None)` if the file does not exist
    /// (fresh start), `Err` on corruption.
    pub fn load(path: &Path) -> Result<Option<Self>, RelayerError> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomically persist to disk: write to `<path>.tmp`, then rename.
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

    /// The next `depositId` the relayer should target.
    ///
    /// `last_processed + 1`, or `start` when nothing has been processed yet.
    pub fn next_target(&self, start: u64) -> u64 {
        match self.last_processed_deposit_id {
            Some(last) => last.saturating_add(1),
            None => start,
        }
    }

    /// Mark a deposit as finalized. Resets the failure counter.
    pub fn record_progress(&mut self, deposit_id: u64) {
        self.last_processed_deposit_id = Some(deposit_id);
        self.last_attempt_deposit_id = Some(deposit_id);
        self.attempts_since_progress = 0;
    }

    /// Mark an attempt that did not finalize the deposit (not yet available,
    /// AN rejection, or a transient error).
    pub fn record_attempt(&mut self, deposit_id: u64) {
        self.last_attempt_deposit_id = Some(deposit_id);
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
    use tempfile::tempdir;

    use super::*;

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
        assert_eq!(loaded.last_processed_deposit_id, Some(7));
        assert_eq!(loaded.last_attempt_deposit_id, Some(7));
        assert_eq!(loaded.attempts_since_progress, 0);
    }

    #[test]
    fn next_target_uses_start_then_increments() {
        let mut s = RelayerState::default();
        assert_eq!(s.next_target(0), 0);
        assert_eq!(s.next_target(5), 5);
        s.record_progress(5);
        assert_eq!(s.next_target(0), 6);
    }

    #[test]
    fn record_attempt_increments_until_progress() {
        let mut s = RelayerState::default();
        s.record_attempt(3);
        s.record_attempt(3);
        s.record_attempt(3);
        assert_eq!(s.attempts_since_progress, 3);
        s.record_progress(3);
        assert_eq!(s.attempts_since_progress, 0);
    }

    #[test]
    fn corrupted_state_json_fails_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, b"{not-json").unwrap();
        assert!(RelayerState::load(&path).is_err());
    }
}
