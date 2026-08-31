//! Atomic `state.json` persistence (write-temp-then-rename + fsync).

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::error::RelayerError;

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
