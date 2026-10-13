//! Atomic `state.json` persistence (write-temp-then-rename + fsync).

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::error::RelayerError;

/// One span of execution headers the daemon still owes the light client.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwedSpan {
    /// The older anchor: the checkpoint the head stood on before.
    pub older: [u8; 32],
    /// The newer anchor: the checkpoint that moved the head.
    pub newer: [u8; 32],
    /// Submissions this span has already cost the light client.
    #[serde(default)]
    pub paid_attempts: u32,
    /// When its headers first failed to fetch; unset while fetching works.
    #[serde(default)]
    pub fetch_since: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelayerState {
    pub last_finalized_slot: Option<u64>,
    pub last_committee_period: Option<u64>,
    pub last_execution_block_hash: Option<[u8; 32]>,
    pub last_attested_slot: Option<u64>,
    #[serde(default)]
    pub updates_applied: u64,
    /// Period the daemon refused to cross without a rotate proof.
    pub rotate_pending_period: Option<u64>,
    /// Spans still owed to `submitAncestry`, oldest first. A pair is fixed
    /// when its newer end is accepted, so a span is never re-paired with a
    /// later checkpoint, and each carries its own accounting — one cursor and
    /// one counter cannot represent two owed spans, which is how earlier
    /// versions lost them. An entry leaves only when it is settled: sent, or
    /// written off with a warning.
    #[serde(default)]
    pub owed_spans: Vec<OwedSpan>,
    /// Relayer already issued the one-way owner flip.
    #[serde(default)]
    pub owner_flip_done: bool,
    /// `genesis_validators_root` (0x-hex) of the beacon network this state was
    /// built against. A source reporting a different root is refused, so a
    /// mainnet state file is never advanced from a Sepolia node or vice versa.
    #[serde(default)]
    pub genesis_validators_root: Option<String>,
}

impl RelayerState {
    pub fn load(path: &Path) -> Result<Option<Self>, RelayerError> {
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(path)?;
        Ok(Some(serde_json::from_slice(&bytes)?))
    }

    pub fn save(&self, path: &Path) -> Result<(), RelayerError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = sibling_tmp_path(path);
        {
            let mut f = File::create(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(self)?)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn record_update(
        &mut self,
        finalized_slot: u64,
        period: u64,
        exec_hash: [u8; 32],
        attested_slot: u64,
    ) {
        self.last_finalized_slot = Some(finalized_slot);
        self.last_committee_period = Some(period);
        self.last_execution_block_hash = Some(exec_hash);
        self.last_attested_slot = Some(attested_slot);
        self.updates_applied = self.updates_applied.saturating_add(1);
        self.rotate_pending_period = None;
    }

    pub fn record_rotate(&mut self, period: u64) {
        self.last_committee_period = Some(period);
        self.rotate_pending_period = None;
    }

    pub fn record_rotate_pending(&mut self, period: u64) {
        self.rotate_pending_period = Some(period);
    }
}

fn sibling_tmp_path(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

pub fn state_lock_path(state_path: &Path) -> PathBuf {
    let mut p = state_path.as_os_str().to_os_string();
    p.push(".lock");
    PathBuf::from(p)
}

pub struct StateLock {
    _file: File,
}

impl StateLock {
    pub fn acquire(state_path: &Path) -> Result<Self, RelayerError> {
        let lock_path = state_lock_path(state_path);
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;
        file.try_lock_exclusive().map_err(|_| {
            RelayerError::other(format!(
                "state lock busy ({}); another relayer is running",
                lock_path.display()
            ))
        })?;
        Ok(Self {
            _file: file,
        })
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    /// Every field this version added has to be optional on the way in, or a
    /// daemon started on a state file written by the previous build refuses to
    /// load it and stops dead.
    #[test]
    fn a_state_file_written_before_the_queue_still_loads() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::write(
            &path,
            br#"{"last_finalized_slot":100,"last_committee_period":0,
                 "last_execution_block_hash":null,"last_attested_slot":120,
                 "updates_applied":3,"rotate_pending_period":null}"#,
        )
        .unwrap();
        let s = RelayerState::load(&path).unwrap().expect("must load");
        assert_eq!(s.last_finalized_slot, Some(100));
        assert_eq!(s.updates_applied, 3);
        assert!(s.owed_spans.is_empty(), "no spans were owed in that file");
        assert!(!s.owner_flip_done);
    }

    /// And a queue has to come back as it went in: this is the only record of
    /// what the light client is owed, so losing it across a restart loses the
    /// coverage with it.
    #[test]
    fn the_owed_queue_survives_a_save_and_a_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut s = RelayerState::default();
        s.record_update(200, 1, [9u8; 32], 232);
        s.owed_spans.push(OwedSpan {
            older: [1u8; 32],
            newer: [2u8; 32],
            paid_attempts: 2,
            fetch_since: Some(1_700_000_000),
        });
        s.owed_spans.push(OwedSpan {
            older: [2u8; 32],
            newer: [3u8; 32],
            paid_attempts: 0,
            fetch_since: None,
        });
        s.save(&path).unwrap();

        let back = RelayerState::load(&path).unwrap().expect("must load");
        assert_eq!(back.owed_spans, s.owed_spans);
        assert_eq!(back.owed_spans[0].paid_attempts, 2);
        assert_eq!(back.owed_spans[0].fetch_since, Some(1_700_000_000));
    }

    #[test]
    fn roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut s = RelayerState::default();
        s.record_update(100, 0, [7u8; 32], 120);
        s.save(&path).unwrap();
        let loaded = RelayerState::load(&path).unwrap().unwrap();
        assert_eq!(loaded.last_finalized_slot, Some(100));
        assert_eq!(loaded.updates_applied, 1);
    }
}

/// Wall-clock seconds, for bounds that must not move with the poll interval.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
