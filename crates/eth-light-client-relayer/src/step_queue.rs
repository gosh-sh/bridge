//! Checkpoints captured from the beacon node and not yet on the light client.
//!
//! The beacon node serves only its latest `finality_update`, so a checkpoint
//! the daemon does not take while it is the latest is gone from that endpoint
//! for good. Every tick takes the latest one into this queue before anything
//! else, and a step proof runs in the background while the next ones are
//! taken. Each entry is a file in the queue directory, written before the
//! entry counts as taken and removed once the light client has it or the
//! daemon gives it up, so a restart proves what the previous run had not.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::{
    error::RelayerError,
    types::{parse_finality_update, BeaconChainParams, FinalityUpdate},
};

/// Slots of one beacon epoch: one checkpoint each.
pub const SLOTS_PER_EPOCH: u64 = 32;

/// The epoch whose checkpoint the block at `slot` is. A checkpoint is the
/// block at the epoch's first slot, or the last block before it when that
/// slot is empty, so a slot just under a boundary belongs to the next epoch.
pub fn checkpoint_epoch(slot: u64) -> u64 {
    slot.div_ceil(SLOTS_PER_EPOCH)
}

/// Checkpoints strictly between two, by their finalized slots.
pub fn missed_between(older_slot: u64, newer_slot: u64) -> u64 {
    checkpoint_epoch(newer_slot)
        .saturating_sub(checkpoint_epoch(older_slot))
        .saturating_sub(1)
}

/// What one queue file holds: the beacon answer as it came, the committee it
/// is proven against and the signing domain the source resolved.
#[derive(Serialize, Deserialize)]
struct Captured {
    raw_json: String,
    committee_json: String,
    chain: Option<CapturedChain>,
}

#[derive(Serialize, Deserialize)]
struct CapturedChain {
    fork_version: String,
    genesis_validators_root: String,
    gloas_fork_slot: u64,
}

impl Captured {
    fn of(update: &FinalityUpdate) -> Self {
        Self {
            raw_json: update.raw_json.clone(),
            committee_json: update.committee_json.clone(),
            chain: update.chain.map(|c| CapturedChain {
                fork_version: hex::encode(c.fork_version),
                genesis_validators_root: hex::encode(c.genesis_validators_root),
                gloas_fork_slot: c.gloas_fork_slot,
            }),
        }
    }

    fn into_update(self) -> Result<FinalityUpdate, RelayerError> {
        let mut update = parse_finality_update(&self.raw_json)?;
        update.committee_json = self.committee_json;
        update.chain = match self.chain {
            None => None,
            Some(c) => Some(BeaconChainParams {
                fork_version: hex_array(&c.fork_version)?,
                genesis_validators_root: hex_array(&c.genesis_validators_root)?,
                gloas_fork_slot: c.gloas_fork_slot,
            }),
        };
        Ok(update)
    }
}

fn hex_array<const N: usize>(s: &str) -> Result<[u8; N], RelayerError> {
    hex::decode(s)
        .map_err(|e| RelayerError::other(format!("queued checkpoint: {e}")))?
        .try_into()
        .map_err(|_| RelayerError::other(format!("queued checkpoint: expected {N} bytes")))
}

/// The queue: entries by finalized slot, oldest first, one file each.
pub struct StepQueue {
    dir: PathBuf,
    entries: Vec<FinalityUpdate>,
}

impl StepQueue {
    /// The queue directory next to the state file: `<state file>.steps/`.
    pub fn dir_for(state_path: &Path) -> PathBuf {
        let mut dir = state_path.as_os_str().to_os_string();
        dir.push(".steps");
        PathBuf::from(dir)
    }

    /// Read what a previous run left. A file that does not parse is moved
    /// aside, not deleted, and the rest of the queue still loads.
    pub fn open(dir: PathBuf) -> Result<Self, RelayerError> {
        fs::create_dir_all(&dir)?;
        let mut entries = Vec::new();
        for item in fs::read_dir(&dir)? {
            let path = item?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let read = fs::read(&path)
                .map_err(RelayerError::from)
                .and_then(|b| serde_json::from_slice::<Captured>(&b).map_err(RelayerError::from))
                .and_then(Captured::into_update);
            match read {
                Ok(update) => entries.push(update),
                Err(e) => {
                    let aside = path.with_extension("unreadable");
                    warn!(file = %path.display(), error = %e, "queued checkpoint unreadable; moved aside");
                    let _ = fs::rename(&path, aside);
                },
            }
        }
        entries.sort_by_key(|u| u.finalized_slot);
        entries.dedup_by_key(|u| u.finalized_slot);
        Ok(Self {
            dir,
            entries,
        })
    }

    fn file(&self, finalized_slot: u64) -> PathBuf {
        self.dir.join(format!("{finalized_slot:012}.json"))
    }

    /// Take a checkpoint in: on disk first, then in the queue.
    pub fn push(&mut self, update: FinalityUpdate) -> Result<(), RelayerError> {
        let path = self.file(update.finalized_slot);
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec(&Captured::of(&update))?)?;
        fs::rename(&tmp, &path)?;
        let at = self
            .entries
            .partition_point(|u| u.finalized_slot < update.finalized_slot);
        if self
            .entries
            .get(at)
            .is_some_and(|u| u.finalized_slot == update.finalized_slot)
        {
            self.entries[at] = update;
        } else {
            self.entries.insert(at, update);
        }
        Ok(())
    }

    /// Drop the entry of `finalized_slot`, file and all.
    pub fn remove(&mut self, finalized_slot: u64) -> Result<(), RelayerError> {
        self.entries.retain(|u| u.finalized_slot != finalized_slot);
        match fs::remove_file(self.file(finalized_slot)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    pub fn front(&self) -> Option<&FinalityUpdate> {
        self.entries.first()
    }

    pub fn last_slot(&self) -> Option<u64> {
        self.entries.last().map(|u| u.finalized_slot)
    }

    pub fn iter(&self) -> impl Iterator<Item = &FinalityUpdate> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn update(finalized: u64) -> FinalityUpdate {
        let json = format!(
            r#"{{"data":{{"attested_header":{{"beacon":{{"slot":"{a}","state_root":"0x{r}"}}}},
            "finalized_header":{{"beacon":{{"slot":"{f}","state_root":"0x{r}"}},
            "execution":{{"block_hash":"0x{finalized:064x}"}}}},
            "sync_aggregate":{{"sync_committee_bits":"0x{bits}"}},"signature_slot":"{s}"}}}}"#,
            a = finalized + 64,
            f = finalized,
            s = finalized + 65,
            r = "11".repeat(32),
            bits = "ff".repeat(64),
        );
        let mut u = parse_finality_update(&json).expect("fixture parses");
        u.committee_json = "{\"committee\":1}".into();
        u.chain = Some(BeaconChainParams {
            fork_version: [1, 2, 3, 4],
            genesis_validators_root: [9; 32],
            gloas_fork_slot: 77,
        });
        u
    }

    #[test]
    fn a_checkpoint_survives_a_restart_as_it_was_taken() {
        let dir = tempdir().unwrap();
        let mut q = StepQueue::open(dir.path().join("q")).unwrap();
        q.push(update(96)).unwrap();
        q.push(update(64)).unwrap();
        let back = StepQueue::open(dir.path().join("q")).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back.front().unwrap().finalized_slot, 64);
        assert_eq!(back.front(), q.front());
        assert_eq!(back.last_slot(), Some(96));
    }

    #[test]
    fn a_removed_checkpoint_is_gone_from_disk_too() {
        let dir = tempdir().unwrap();
        let mut q = StepQueue::open(dir.path().join("q")).unwrap();
        q.push(update(64)).unwrap();
        q.push(update(96)).unwrap();
        q.remove(64).unwrap();
        q.remove(64).unwrap();
        let back = StepQueue::open(dir.path().join("q")).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back.front().unwrap().finalized_slot, 96);
    }

    #[test]
    fn an_unreadable_file_is_moved_aside_and_the_rest_loads() {
        let dir = tempdir().unwrap();
        let mut q = StepQueue::open(dir.path().join("q")).unwrap();
        q.push(update(64)).unwrap();
        fs::write(dir.path().join("q").join("000000000032.json"), b"{").unwrap();
        let back = StepQueue::open(dir.path().join("q")).unwrap();
        assert_eq!(back.len(), 1);
        assert!(dir
            .path()
            .join("q")
            .join("000000000032.unreadable")
            .exists());
    }

    #[test]
    fn checkpoints_between_count_epochs_not_slots() {
        assert_eq!(missed_between(64, 96), 0);
        assert_eq!(missed_between(64, 160), 2);
        // An empty first slot puts the checkpoint just under the boundary.
        assert_eq!(missed_between(63, 96), 0);
        assert_eq!(missed_between(64, 95), 0);
        assert_eq!(checkpoint_epoch(0), 0);
        assert_eq!(checkpoint_epoch(1), 1);
        assert_eq!(checkpoint_epoch(32), 1);
    }
}
