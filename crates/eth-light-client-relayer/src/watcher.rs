//! Watches the beacon node for finalized checkpoints, apart from everything
//! else the daemon does.
//!
//! The beacon node serves only its latest `finality_update`, and a new
//! checkpoint is finalized every epoch (6.4 min). The watcher asks for it
//! every slot and writes each checkpoint it has not taken yet into the queue
//! on disk as soon as it appears. Proving, sending to Acki Nacki, the
//! daemon's backoff and the rotate run in another task and never hold it up.

use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use tracing::{error, info, warn};

use crate::{
    error::RelayerError,
    source::BeaconSource,
    step_queue::{missed_between, StepQueue},
    types::FinalityUpdate,
};

/// How often the watcher asks the beacon node: one slot.
pub const WATCH_INTERVAL: Duration = Duration::from_secs(12);

/// Members of the 512-key sync committee.
pub const SYNC_COMMITTEE_SIZE: u64 = 512;

/// Signatures an update needs for `submitUpdate` to take it: two thirds of
/// the committee (`ERR_LOW_PARTICIPATION`). A version signed by fewer is
/// refused whatever is retried, so it is not taken; the beacon node serves
/// the same checkpoint again with the signatures of later slots.
pub const MIN_PARTICIPATION: u64 = (2 * SYNC_COMMITTEE_SIZE).div_ceil(3);

/// What the watcher and the daemon share.
pub(crate) struct Taken {
    pub(crate) queue: StepQueue,
    /// The newest finalized slot the light client has, as the daemon
    /// recorded it.
    pub(crate) recorded: Option<u64>,
    /// The beacon head as last seen.
    pub(crate) latest: Option<FinalityUpdate>,
    /// The `genesis_validators_root` the state file is pinned to.
    pub(crate) network: Option<String>,
    /// The checkpoint last reported as signed too thinly, so it is reported
    /// once.
    thin: Option<u64>,
}

impl Taken {
    pub(crate) fn new(queue: StepQueue, recorded: Option<u64>, network: Option<String>) -> Self {
        Self {
            queue,
            recorded,
            latest: None,
            network,
            thin: None,
        }
    }

    /// Refuse a beacon node of another network than the one the state file
    /// was built on; pin the first one seen.
    fn guard_network(&mut self, update: &FinalityUpdate) -> Result<(), RelayerError> {
        let Some(chain) = &update.chain else {
            return Ok(());
        };
        let seen = chain.genesis_validators_root_hex();
        match &self.network {
            Some(known) if *known != seen => Err(RelayerError::other(format!(
                "beacon network mismatch: state file was built on genesis_validators_root \
                 {known}, source reports {seen}; refusing to mix networks"
            ))),
            Some(_) => Ok(()),
            None => {
                info!(
                    genesis_validators_root = %seen,
                    fork_version = %chain.fork_version_hex(),
                    "pinning state file to beacon network"
                );
                self.network = Some(seen);
                Ok(())
            },
        }
    }

    /// Take the beacon head into the queue when it is a checkpoint the queue
    /// and the light client do not have yet, and say so loudly when
    /// checkpoints were missed before it.
    fn capture(&mut self, latest: &FinalityUpdate) -> Result<(), RelayerError> {
        // Not just equality. A beacon node restarting into sync serves a
        // finalized slot behind the one on record; queuing it would prove a
        // checkpoint the head has passed.
        if let Some(recorded) = self.recorded {
            if latest.finalized_slot < recorded {
                warn!(
                    reported = latest.finalized_slot,
                    recorded, "beacon source reports a finalized slot behind the one on record"
                );
            }
        }
        let newest = match (self.queue.last_slot(), self.recorded) {
            (Some(q), Some(r)) => Some(q.max(r)),
            (q, r) => q.or(r),
        };
        if newest.is_some_and(|n| latest.finalized_slot <= n) {
            return Ok(());
        }
        if latest.participation < MIN_PARTICIPATION {
            if self.thin != Some(latest.finalized_slot) {
                self.thin = Some(latest.finalized_slot);
                warn!(
                    slot = latest.finalized_slot,
                    signed = latest.participation,
                    needed = MIN_PARTICIPATION,
                    "checkpoint signed by too few of the committee for the light client; waiting \
                     for a version signed by more"
                );
            }
            return Ok(());
        }
        if let Some(newest) = newest {
            let missed = missed_between(newest, latest.finalized_slot);
            if missed > 0 {
                error!(
                    after_slot = newest,
                    before_slot = latest.finalized_slot,
                    missed,
                    "checkpoints missed (the daemon or the beacon node was down); their blocks \
                     are closed with submitAncestry back from the next checkpoint on chain"
                );
            }
        }
        self.queue.push(latest.clone())?;
        info!(
            slot = latest.finalized_slot,
            queued = self.queue.len(),
            "checkpoint queued"
        );
        Ok(())
    }
}

/// The beacon watcher. Clones share the queue with the daemon they came
/// from.
pub struct Watcher<S> {
    source: Arc<S>,
    taken: Arc<Mutex<Taken>>,
}

impl<S> Clone for Watcher<S> {
    fn clone(&self) -> Self {
        Self {
            source: Arc::clone(&self.source),
            taken: Arc::clone(&self.taken),
        }
    }
}

impl<S: BeaconSource> Watcher<S> {
    pub(crate) fn new(source: Arc<S>, taken: Arc<Mutex<Taken>>) -> Self {
        Self {
            source,
            taken,
        }
    }

    /// Ask the beacon node once and take its checkpoint if it is new.
    pub async fn poll(&self) -> Result<FinalityUpdate, RelayerError> {
        let latest = self.source.fetch_finality().await?;
        let mut taken = lock(&self.taken);
        taken.guard_network(&latest)?;
        taken.latest = Some(latest.clone());
        taken.capture(&latest)?;
        Ok(latest)
    }

    /// Poll every `interval` until the task is dropped. A failed poll is
    /// tried again at the same pace: the next checkpoint is never further
    /// away than one interval.
    pub async fn run(self, interval: Duration) {
        loop {
            if let Err(e) = self.poll().await {
                warn!(error = %e, "beacon finality_update not taken; asking again");
            }
            tokio::time::sleep(interval).await;
        }
    }
}

impl<S: BeaconSource + 'static> Watcher<S> {
    /// [`Self::run`] in a task of its own, stopped when the handle is
    /// dropped.
    pub fn spawn(self, interval: Duration) -> WatcherTask {
        WatcherTask(tokio::spawn(self.run(interval)))
    }
}

/// A running watcher; dropping it stops it.
pub struct WatcherTask(tokio::task::JoinHandle<()>);

impl WatcherTask {
    /// Wait until the watcher stops, which it does only by panicking, and
    /// say why. Cancel-safe: the daemon polls it beside its own work.
    pub async fn stopped(&mut self) -> String {
        match (&mut self.0).await {
            Ok(()) => "it returned".into(),
            Err(e) if e.is_panic() => {
                let panic = e.into_panic();
                panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "it panicked".into())
            },
            Err(e) => e.to_string(),
        }
    }
}

impl Drop for WatcherTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) fn lock(taken: &Mutex<Taken>) -> MutexGuard<'_, Taken> {
    taken.lock().expect("checkpoint queue lock poisoned")
}
