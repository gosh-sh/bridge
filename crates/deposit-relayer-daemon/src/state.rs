//! On-disk relayer state for crash-recovery.
//!
//! The authoritative "was this deposit finalized?" record lives on the AN
//! side as the `usedDepositIds` nullifier set; the local state is a *cache*
//! plus a *retry log* so the relayer can resume near where it left off
//! without re-proving everything from `depositId = 0`:
//!
//! - `last_processed_deposit_id` is the high-water mark of deposits that are
//!   done (finalized, parked, or unprovable).
//! - `outstanding_deposits` is the sparse set of ids that failed and still have
//!   to be retried. Each entry has its own attempt count and
//!   `retry_after_unix`. A stuck deposit stays there and retries on its own
//!   schedule; it does not block a later id.
//! - `last_attempt_deposit_id` records the most recent attempt regardless of
//!   outcome.
//! - `attempts_since_progress` mirrors the attempt count of the last failed
//!   target (Prometheus / `--skip-after-attempts`). Waiting for a deposit that
//!   is not made yet is not an attempt, and neither is a submit still pending
//!   on AN.
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
    time::{SystemTime, UNIX_EPOCH},
};

use alloy::primitives::Address;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::error::RelayerError;

/// Seconds since the Unix epoch. Used for per-id retry deadlines.
pub fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

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

/// A deposit that failed and is still to be retried.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OutstandingDeposit {
    pub deposit_id: u64,
    pub attempts: u32,
    pub retry_after_unix: u64,
    /// Ethereum block that holds this deposit's log, when known. The log-scan
    /// cursor must not move past it while the id is outstanding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_number: Option<u64>,
}

/// Locally-persisted relayer progress.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RelayerState {
    /// Set on first daemon start; must match the live config on subsequent
    /// runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment: Option<DeploymentIdentity>,
    /// Highest `depositId` known finalized, parked, or unprovable on AN.
    /// `None` before the first such outcome of this relayer's lifetime.
    pub last_processed_deposit_id: Option<u64>,
    /// Most recent `depositId` attempted (may be ahead of
    /// `last_processed_deposit_id` if the latest attempt failed).
    pub last_attempt_deposit_id: Option<u64>,
    /// Consecutive failed attempts at the last failed target. Reset to 0 on
    /// every finalized deposit. Mirrors the matching
    /// [`OutstandingDeposit::attempts`].
    pub attempts_since_progress: u32,
    /// No Ethereum block up to and including this one holds a deposit the
    /// relayer still has to deliver, so the next `eth_getLogs` scan starts
    /// after it. Set to the confirmed head while the next deposit is not made
    /// yet, and to a finalized deposit's block minus one (a later `depositId`
    /// can share that block). `None` = scan from `--from-block`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_done_through_block: Option<u64>,
    /// Deposit ids the daemon abandoned after `--skip-after-attempts`.
    /// Operators must finalise these manually via `finalize-one`. Parked ids
    /// stay parked across restarts; they are not loaded back as outstanding.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parked_deposit_ids: Vec<u64>,
    /// Deposit ids the circuit cannot prove, advanced past at once.
    /// `finalize-one` cannot help these; only the EVM bridge operator can
    /// return their funds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unprovable_deposit_ids: Vec<u64>,
    /// Failed deposits still to retry. Bounded by the number of unfinished
    /// ids, not by every deposit ever made.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outstanding_deposits: Vec<OutstandingDeposit>,
}

impl RelayerState {
    /// Read the state file. Returns `Ok(None)` if the file does not exist
    /// (fresh start), `Err` on corruption.
    pub fn load(path: &Path) -> Result<Option<Self>, RelayerError> {
        match fs::read(path) {
            Ok(bytes) => Ok(Some(Self::from_json(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn from_json(bytes: &[u8]) -> Result<Self, RelayerError> {
        let raw: serde_json::Value = serde_json::from_slice(bytes)?;
        // Every file an older daemon saved carries `scanned_through_block`.
        // That daemon also counted each poll that found no deposit as a failed
        // attempt, so its count says nothing about the current target: start
        // it over rather than let one real failure reach the skip threshold.
        let written_by_older_daemon = raw.get("scanned_through_block").is_some();
        let mut state: Self = serde_json::from_value(raw)?;
        if written_by_older_daemon {
            state.attempts_since_progress = 0;
        }
        Ok(state)
    }

    /// Validate or stamp deployment binding. Legacy state files without
    /// `deployment` are upgraded on first save.
    ///
    /// With `force`, a mismatched file is started over for the new deployment
    /// and the discarded state is returned: its cursor counts deposits of
    /// another bridge, and keeping it would skip the new bridge's first ids.
    pub fn ensure_deployment(
        &mut self,
        expected: &DeploymentIdentity,
        force: bool,
    ) -> Result<Option<RelayerState>, RelayerError> {
        match &self.deployment {
            None => {
                self.deployment = Some(expected.clone());
                Ok(None)
            },
            Some(stored) if stored == expected => Ok(None),
            Some(_) if force => {
                let fresh = RelayerState {
                    deployment: Some(expected.clone()),
                    ..RelayerState::default()
                };
                Ok(Some(std::mem::replace(self, fresh)))
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

    /// The next `depositId` the relayer should target, using wall-clock now.
    pub fn next_target(&self, start: u64) -> u64 {
        self.next_target_at(start, unix_now_secs())
    }

    /// The next new `depositId` the relayer should try at `now`.
    ///
    /// Walks `last_processed + 1` (or `start`), skipping parked, unprovable,
    /// and outstanding ids. Outstanding ids are retried separately when this
    /// walk has nothing new to fetch, so two failing deposits cannot starve
    /// later ones.
    pub fn next_target_at(&self, start: u64, _now: u64) -> u64 {
        let mut id = match self.last_processed_deposit_id {
            Some(last) => last.saturating_add(1),
            None => start,
        };
        while self.is_skipped(id) || self.outstanding_contains(id) {
            let next = id.saturating_add(1);
            if next == id {
                break;
            }
            id = next;
        }
        id
    }

    /// Mark a deposit as finalized. Resets the failure counter and drops it
    /// from the outstanding set. The high-water mark only moves forward.
    pub fn record_progress(&mut self, deposit_id: u64) {
        self.raise_last_processed(deposit_id);
        self.last_attempt_deposit_id = Some(deposit_id);
        self.attempts_since_progress = 0;
        self.remove_outstanding(deposit_id);
    }

    /// Mark an attempt that failed the deposit (proof failure, AN rejection).
    /// The id stays outstanding with a per-id backoff; the high-water mark
    /// does not move, so a later id can be tried while this one waits.
    pub fn record_attempt(&mut self, deposit_id: u64, now: u64, backoff_secs: u64) {
        self.record_attempt_at(deposit_id, now, backoff_secs, None);
    }

    /// Like [`record_attempt`], and remembers the deposit's Ethereum block
    /// so a later retry can scan from that block.
    pub fn record_attempt_at(
        &mut self,
        deposit_id: u64,
        now: u64,
        backoff_secs: u64,
        block_number: Option<u64>,
    ) {
        self.last_attempt_deposit_id = Some(deposit_id);
        let attempts = self
            .outstanding_deposits
            .iter()
            .find(|o| o.deposit_id == deposit_id)
            .map(|o| o.attempts.saturating_add(1))
            .unwrap_or(1);
        self.attempts_since_progress = attempts;
        let shift = attempts.saturating_sub(1).min(6);
        let delay = backoff_secs.saturating_mul(1u64 << shift);
        self.upsert_outstanding(
            deposit_id,
            attempts,
            now.saturating_add(delay),
            block_number,
        );
    }

    /// Mark a submit whose outcome is not known yet (confirmation timed out,
    /// or the lane is paused). It says nothing against the deposit, so it does
    /// not count toward `--skip-after-attempts`.
    pub fn record_pending(&mut self, deposit_id: u64) {
        self.last_attempt_deposit_id = Some(deposit_id);
    }

    /// Park a stuck deposit and advance the cursor past it. The id is recorded
    /// in `parked_deposit_ids` for operator follow-up and dropped from
    /// outstanding so it is not retried automatically.
    pub fn record_skip(&mut self, deposit_id: u64) {
        if !self.parked_deposit_ids.contains(&deposit_id) {
            self.parked_deposit_ids.push(deposit_id);
        }
        self.remove_outstanding(deposit_id);
        self.advance_past(deposit_id);
    }

    /// Record a deposit the circuit cannot prove and advance the cursor past
    /// it. The id goes to `unprovable_deposit_ids`, not `parked_deposit_ids`.
    pub fn record_unprovable(&mut self, deposit_id: u64) {
        if !self.unprovable_deposit_ids.contains(&deposit_id) {
            self.unprovable_deposit_ids.push(deposit_id);
        }
        self.remove_outstanding(deposit_id);
        self.advance_past(deposit_id);
    }

    /// Attempt count stored for `deposit_id`, or `attempts_since_progress`
    /// when it is not outstanding.
    pub fn outstanding_attempts(&self, deposit_id: u64) -> u32 {
        self.outstanding_deposits
            .iter()
            .find(|o| o.deposit_id == deposit_id)
            .map(|o| o.attempts)
            .unwrap_or(self.attempts_since_progress)
    }

    fn advance_past(&mut self, deposit_id: u64) {
        self.raise_last_processed(deposit_id);
        self.last_attempt_deposit_id = Some(deposit_id);
        self.attempts_since_progress = 0;
    }

    fn raise_last_processed(&mut self, deposit_id: u64) {
        self.last_processed_deposit_id = Some(match self.last_processed_deposit_id {
            Some(last) => last.max(deposit_id),
            None => deposit_id,
        });
    }

    fn upsert_outstanding(
        &mut self,
        deposit_id: u64,
        attempts: u32,
        retry_after_unix: u64,
        block_number: Option<u64>,
    ) {
        if let Some(existing) = self
            .outstanding_deposits
            .iter_mut()
            .find(|o| o.deposit_id == deposit_id)
        {
            existing.attempts = attempts;
            existing.retry_after_unix = retry_after_unix;
            if block_number.is_some() {
                existing.block_number = block_number;
            }
            return;
        }
        self.outstanding_deposits.push(OutstandingDeposit {
            deposit_id,
            attempts,
            retry_after_unix,
            block_number,
        });
        self.outstanding_deposits.sort_by_key(|o| o.deposit_id);
    }

    fn remove_outstanding(&mut self, deposit_id: u64) {
        self.outstanding_deposits
            .retain(|o| o.deposit_id != deposit_id);
    }

    fn outstanding_contains(&self, deposit_id: u64) -> bool {
        self.outstanding_deposits
            .iter()
            .any(|o| o.deposit_id == deposit_id)
    }

    fn is_skipped(&self, deposit_id: u64) -> bool {
        self.parked_deposit_ids.contains(&deposit_id)
            || self.unprovable_deposit_ids.contains(&deposit_id)
    }

    pub fn ready_outstanding(&self, now: u64) -> Option<u64> {
        self.outstanding_deposits
            .iter()
            .filter(|o| o.retry_after_unix <= now && !self.is_skipped(o.deposit_id))
            .map(|o| o.deposit_id)
            .min()
    }

    /// Lowest Ethereum block among outstanding deposits, when any is known.
    pub fn min_outstanding_block(&self) -> Option<u64> {
        self.outstanding_deposits
            .iter()
            .filter_map(|o| o.block_number)
            .min()
    }

    /// Known Ethereum block for an outstanding id.
    pub fn outstanding_block(&self, deposit_id: u64) -> Option<u64> {
        self.outstanding_deposits
            .iter()
            .find(|o| o.deposit_id == deposit_id)
            .and_then(|o| o.block_number)
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
        s.record_attempt(7, 0, 0);
        s.record_progress(7);
        s.save(&path).unwrap();

        let loaded = RelayerState::load(&path).unwrap().unwrap();
        assert_eq!(loaded.last_processed_deposit_id, Some(7));
        assert_eq!(loaded.deployment, Some(deployment(11155111)));
        assert!(loaded.outstanding_deposits.is_empty());
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
    fn forced_deployment_change_starts_the_state_over() {
        let mut s = RelayerState {
            deployment: Some(deployment(1)),
            scan_done_through_block: Some(500),
            parked_deposit_ids: vec![4],
            ..RelayerState::default()
        };
        s.record_progress(100);

        let dropped = s.ensure_deployment(&deployment(2), true).unwrap().unwrap();
        assert_eq!(dropped.last_processed_deposit_id, Some(100));
        assert_eq!(dropped.parked_deposit_ids, vec![4]);
        assert_eq!(s, RelayerState {
            deployment: Some(deployment(2)),
            ..RelayerState::default()
        });
        assert_eq!(s.next_target(0), 0);

        assert_eq!(s.ensure_deployment(&deployment(2), true).unwrap(), None);
    }

    #[test]
    fn next_target_uses_start_then_increments() {
        let mut s = RelayerState::default();
        assert_eq!(s.next_target_at(0, 0), 0);
        assert_eq!(s.next_target_at(5, 0), 5);
        s.record_progress(5);
        assert_eq!(s.next_target_at(0, 0), 6);
    }

    #[test]
    fn record_skip_advances_and_parks() {
        let mut s = RelayerState::default();
        s.record_attempt(3, 0, 0);
        s.record_attempt(3, 0, 0);
        s.record_skip(3);
        assert_eq!(s.last_processed_deposit_id, Some(3));
        assert_eq!(s.next_target_at(0, 0), 4);
        assert_eq!(s.parked_deposit_ids, vec![3]);
        assert!(s.unprovable_deposit_ids.is_empty());
        assert!(s.outstanding_deposits.is_empty());
        assert_eq!(s.attempts_since_progress, 0);
    }

    #[test]
    fn record_unprovable_advances_without_parking() {
        let mut s = RelayerState::default();
        s.record_attempt(3, 0, 0);
        s.record_unprovable(3);
        assert_eq!(s.next_target_at(0, 0), 4);
        assert_eq!(s.unprovable_deposit_ids, vec![3]);
        assert!(s.parked_deposit_ids.is_empty());
        assert!(s.outstanding_deposits.is_empty());
        assert_eq!(s.attempts_since_progress, 0);
    }

    #[test]
    fn a_failed_id_does_not_block_the_next_one() {
        let mut s = RelayerState::default();
        s.record_progress(2);
        s.record_attempt(3, 10, 5);
        assert_eq!(s.last_processed_deposit_id, Some(2));
        assert_eq!(s.outstanding_deposits.len(), 1);
        assert_eq!(s.outstanding_deposits[0].deposit_id, 3);
        assert_eq!(s.outstanding_deposits[0].retry_after_unix, 15);
        // Walk past 3 whether or not its backoff has elapsed.
        assert_eq!(s.next_target_at(0, 10), 4);
        assert_eq!(s.next_target_at(0, 15), 4);
        assert_eq!(s.ready_outstanding(10), None);
        assert_eq!(s.ready_outstanding(15), Some(3));
        s.record_progress(4);
        assert_eq!(s.last_processed_deposit_id, Some(4));
        assert_eq!(s.next_target_at(0, 10), 5);
        assert_eq!(s.ready_outstanding(15), Some(3));
        s.record_progress(3);
        assert!(s.outstanding_deposits.is_empty());
        assert_eq!(s.last_processed_deposit_id, Some(4));
        assert_eq!(s.next_target_at(0, 15), 5);
    }

    #[test]
    fn parked_ids_stay_parked_on_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"{"last_processed_deposit_id":6,"last_attempt_deposit_id":6,
                "attempts_since_progress":0,"parked_deposit_ids":[4,6]}"#,
        )
        .unwrap();

        let mut loaded = RelayerState::load(&path).unwrap().unwrap();
        assert_eq!(loaded.parked_deposit_ids, vec![4, 6]);
        assert!(loaded.outstanding_deposits.is_empty());
        assert_eq!(loaded.next_target_at(0, 0), 7);

        loaded.record_unprovable(7);
        loaded.save(&path).unwrap();
        let reloaded = RelayerState::load(&path).unwrap().unwrap();
        assert_eq!(reloaded.parked_deposit_ids, vec![4, 6]);
        assert_eq!(reloaded.unprovable_deposit_ids, vec![7]);
        assert!(reloaded.outstanding_deposits.is_empty());
    }

    #[test]
    fn a_current_file_keeps_parked_ids() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut s = RelayerState::default();
        s.record_skip(4);
        s.record_attempt(6, 0, 0);
        s.save(&path).unwrap();

        let loaded = RelayerState::load(&path).unwrap().unwrap();
        assert_eq!(loaded.parked_deposit_ids, vec![4]);
        assert_eq!(
            loaded
                .outstanding_deposits
                .iter()
                .map(|o| o.deposit_id)
                .collect::<Vec<_>>(),
            vec![6]
        );
    }

    #[test]
    fn the_old_scan_field_is_dropped() {
        // An older daemon stored the confirmed head it last scanned to, which
        // could be past a deposit it never delivered. Its value must not
        // become the new cursor, and the first save removes it.
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"{"last_processed_deposit_id":6,"last_attempt_deposit_id":7,
                "attempts_since_progress":3,"scanned_through_block":11800417}"#,
        )
        .unwrap();

        let loaded = RelayerState::load(&path).unwrap().unwrap();
        assert_eq!(loaded.scan_done_through_block, None);
        assert_eq!(loaded.next_target_at(0, 0), 7);
        // Its attempt count included polls that found no deposit.
        assert_eq!(loaded.attempts_since_progress, 0);

        loaded.save(&path).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("scanned_through_block"), "{raw}");
    }

    #[test]
    fn a_current_file_keeps_its_attempt_count() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut s = RelayerState {
            scan_done_through_block: Some(11800417),
            ..RelayerState::default()
        };
        for _ in 0..3 {
            s.record_attempt(7, 0, 0);
        }
        s.save(&path).unwrap();

        let loaded = RelayerState::load(&path).unwrap().unwrap();
        assert_eq!(loaded.attempts_since_progress, 3);
        assert_eq!(loaded.outstanding_attempts(7), 3);
        assert_eq!(loaded.scan_done_through_block, Some(11800417));
    }

    #[test]
    fn state_lock_blocks_second_writer() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let _first = StateLock::acquire(&path).unwrap();
        assert!(StateLock::acquire(&path).is_err());
    }
}
