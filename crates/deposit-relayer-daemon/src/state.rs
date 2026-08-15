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
//! daemon. The file is overwritten atomically (write-temp-then-rename + fsync).
//! A deployment binding (`chain_id`, `bridge_address`, `dapp_id`) prevents
//! reusing state across bridge redeploys. An advisory lock blocks two daemons
//! from sharing one state file.

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use alloy::primitives::Address;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::error::RelayerError;

/// Deployment identity stamped into `state.json` so a cursor cannot be reused
/// after a bridge redeploy or dappId change.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeploymentIdentity {
    pub chain_id: u64,
    pub bridge_address: String,
    pub dapp_id: String,
}

impl DeploymentIdentity {
    pub fn new(chain_id: u64, bridge_address: Address, dapp_id: impl Into<String>) -> Self {
        Self {
            chain_id,
            bridge_address: format!("{bridge_address:#x}"),
            dapp_id: dapp_id.into(),
        }
    }
}

/// Locally-persisted relayer progress.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RelayerState {
    /// Set on first daemon start; must match the live config on subsequent
    /// runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment: Option<DeploymentIdentity>,
    /// Highest `depositId` known finalized on AN. `None` before the first
    /// finalize of this relayer's lifetime.
    pub last_processed_deposit_id: Option<u64>,
    /// Most recent `depositId` attempted (may be ahead of
    /// `last_processed_deposit_id` if the latest attempt failed).
    pub last_attempt_deposit_id: Option<u64>,
    /// Consecutive non-success outcomes for the current target. Reset to 0
    /// on every finalized deposit.
    pub attempts_since_progress: u32,
    /// Highest Ethereum block (inclusive) scanned by [`EthLogSource`] on the
    /// last fetch attempt. Lets the daemon resume log scans from the tail
    /// instead of re-walking from the bridge deploy block every tick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scanned_through_block: Option<u64>,
    /// Deposit ids the daemon advanced past after `--skip-after-attempts`.
    /// Operators must finalise these manually via `finalize-one`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parked_deposit_ids: Vec<u64>,
}

impl RelayerState {
    /// Read the state file. Returns `Ok(None)` if the file does not exist
    /// (fresh start), `Err` on corruption.
    pub fn load(path: &Path) -> Result<Option<Self>, RelayerError> {
        match fs::read(path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Validate or stamp deployment binding. Legacy state files without
    /// `deployment` are upgraded on first save.
    pub fn ensure_deployment(
        &mut self,
        expected: &DeploymentIdentity,
        force: bool,
    ) -> Result<(), RelayerError> {
        match &self.deployment {
            None => {
                self.deployment = Some(expected.clone());
                Ok(())
            },
            Some(stored) if stored == expected => Ok(()),
            Some(stored) if force => {
                if stored != expected {
                    self.reset_for_new_deployment();
                }
                self.deployment = Some(expected.clone());
                Ok(())
            },
            Some(stored) => Err(RelayerError::other(format!(
                "state.json deployment mismatch (stored chain={} bridge={} dapp_id={}; expected \
                 chain={} bridge={} dapp_id={}). Use --force-state to override.",
                stored.chain_id,
                stored.bridge_address,
                stored.dapp_id,
                expected.chain_id,
                expected.bridge_address,
                expected.dapp_id,
            ))),
        }
    }

    /// Atomically persist to disk: write to `<path>.tmp`, fsync, then rename.
    pub fn save(&self, path: &Path) -> Result<(), RelayerError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = sibling_tmp_path(path);
        let bytes = serde_json::to_vec_pretty(self)?;
        {
            let mut file = File::create(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        if let Some(parent) = path.parent() {
            if let Ok(dir) = File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        fs::rename(&tmp, path)?;
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

    /// Park a stuck deposit and advance the cursor past it. The id is recorded
    /// in `parked_deposit_ids` for operator follow-up.
    pub fn record_skip(&mut self, deposit_id: u64) {
        if !self.parked_deposit_ids.contains(&deposit_id) {
            self.parked_deposit_ids.push(deposit_id);
        }
        self.last_processed_deposit_id = Some(deposit_id);
        self.last_attempt_deposit_id = Some(deposit_id);
        self.attempts_since_progress = 0;
    }

    /// Clear progress cursors when `--force-state` overrides a deployment
    /// mismatch (TD-18). Prevents reusing `last_processed` across bridge
    /// redeploys or `dappId` namespace changes.
    pub fn reset_for_new_deployment(&mut self) {
        self.last_processed_deposit_id = None;
        self.last_attempt_deposit_id = None;
        self.attempts_since_progress = 0;
        self.scanned_through_block = None;
        self.parked_deposit_ids.clear();
    }
}

/// Advisory exclusive lock for a single-writer daemon. Held for the process
/// lifetime; released on drop.
pub struct StateLock {
    _file: File,
}

impl StateLock {
    /// Acquire an exclusive lock on `<state_path>.lock`.
    pub fn acquire(state_path: &Path) -> Result<Self, RelayerError> {
        let lock_path = state_lock_path(state_path);
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        // The lock file is a sentinel — only the flock matters, so keep whatever
        // bytes are already there rather than rewriting it on every acquire.
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(RelayerError::from)?;
        file.try_lock_exclusive()
            .map_err(|e| RelayerError::other(format!("state lock {:?}: {e}", lock_path)))?;
        Ok(Self {
            _file: file,
        })
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

fn state_lock_path(state_path: &Path) -> PathBuf {
    let mut lock = state_path.to_path_buf();
    let mut name = state_path
        .file_name()
        .map(|s| s.to_owned())
        .unwrap_or_else(|| std::ffi::OsString::from("state.json"));
    name.push(".lock");
    lock.set_file_name(name);
    lock
}

#[cfg(test)]
mod tests {
    use alloy::primitives::Address;
    use tempfile::tempdir;

    use super::*;

    fn deployment(chain: u64) -> DeploymentIdentity {
        DeploymentIdentity::new(chain, Address::repeat_byte(0x99), "0x1a1a1a1a1a")
    }

    #[test]
    fn roundtrip_persists_state() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");

        assert_eq!(RelayerState::load(&path).unwrap(), None);

        let mut s = RelayerState::default();
        s.ensure_deployment(&deployment(11155111), false).unwrap();
        s.record_attempt(7);
        s.record_progress(7);
        s.save(&path).unwrap();

        let loaded = RelayerState::load(&path).unwrap().unwrap();
        assert_eq!(loaded.last_processed_deposit_id, Some(7));
        assert_eq!(loaded.deployment, Some(deployment(11155111)));
    }

    #[test]
    fn deployment_mismatch_rejected() {
        let mut s = RelayerState {
            deployment: Some(deployment(1)),
            ..RelayerState::default()
        };
        let err = s.ensure_deployment(&deployment(2), false).unwrap_err();
        assert!(err.to_string().contains("deployment mismatch"));
    }

    #[test]
    fn force_deployment_mismatch_resets_progress() {
        let mut s = RelayerState {
            deployment: Some(deployment(1)),
            last_processed_deposit_id: Some(5),
            last_attempt_deposit_id: Some(5),
            attempts_since_progress: 3,
            scanned_through_block: Some(42),
            parked_deposit_ids: vec![2],
            ..RelayerState::default()
        };
        let new_dep = DeploymentIdentity::new(
            2,
            Address::repeat_byte(0x99),
            "0xBBBBBBBBBBBB",
        );
        s.ensure_deployment(&new_dep, true).unwrap();
        assert_eq!(s.deployment, Some(new_dep));
        assert_eq!(s.last_processed_deposit_id, None);
        assert_eq!(s.next_target(0), 0);
        assert_eq!(s.scanned_through_block, None);
        assert!(s.parked_deposit_ids.is_empty());
    }

    #[test]
    fn partial_tmp_file_ignored_when_state_json_valid() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let tmp = dir.path().join("state.json.tmp");

        let mut s = RelayerState::default();
        s.ensure_deployment(&deployment(11155111), false).unwrap();
        s.record_progress(3);
        s.save(&path).unwrap();

        std::fs::write(&tmp, b"{\"last_processed_deposit_id\": 99").unwrap();

        let loaded = RelayerState::load(&path).unwrap().unwrap();
        assert_eq!(loaded.last_processed_deposit_id, Some(3));
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
    fn record_skip_advances_and_parks() {
        let mut s = RelayerState::default();
        s.record_attempt(3);
        s.record_attempt(3);
        s.record_skip(3);
        assert_eq!(s.last_processed_deposit_id, Some(3));
        assert_eq!(s.next_target(0), 4);
        assert_eq!(s.parked_deposit_ids, vec![3]);
        assert_eq!(s.attempts_since_progress, 0);
    }

    #[test]
    fn state_lock_blocks_second_writer() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let _first = StateLock::acquire(&path).unwrap();
        assert!(StateLock::acquire(&path).is_err());
    }
}
