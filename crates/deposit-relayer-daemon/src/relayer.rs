//! Relayer main loop (EVM→AN deposit direction).
//!
//! [`Relayer::tick`] is the testable single-step entry point. For the next
//! target `deposit_id` it:
//!
//! 1. asks the [`DepositSource`] for the confirmed `Deposit` event;
//! 2. cheaply checks whether AN already finalized it
//!    ([`AnSubmitter::is_finalized`]) so a replay skips the expensive proving
//!    step; then waits for [`AnSubmitter::is_block_accepted`] before proving;
//! 3. generates the AN-consumable proof triple via the [`ProofGenerator`];
//! 4. submits it to AN via the [`AnSubmitter`] (`finalizeDeposit`);
//! 5. on success — advances [`RelayerState`] and persists it; otherwise records
//!    the attempt on that id with backoff. A failed deposit stays outstanding
//!    and does not block a later id. A deposit that is not made yet is waited
//!    for, not counted as an attempt.
//!
//! [`Relayer::run_loop`] calls `tick` in a loop with a configurable delay and
//! a "max ticks" budget for tests; [`crate::daemon`] adds the long-running
//! backoff + shutdown wrapper.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use crate::{
    error::RelayerError,
    metrics::{self as prom, StageTimer},
    prover::ProofGenerator,
    source::{advance_scan_cursor, DepositSource},
    state::{unix_now_secs, DeploymentIdentity, RelayerState},
    submitter::{AnSubmitter, SubmitOutcome},
};

/// Static configuration for one relayer instance.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelayerConfig {
    /// On-disk path for `state.json`. Recovery on startup reads this.
    pub state_path: PathBuf,
    /// The first `deposit_id` to target on a fresh start (no persisted
    /// state). Usually `0` (the bridge's first deposit).
    #[serde(default)]
    pub start_deposit_id: u64,
    /// Interval between attempts in [`Relayer::run_loop`].
    #[serde(default = "default_poll_interval")]
    pub poll_interval: Duration,
    /// Deployment binding stamped into `state.json` (daemon only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment: Option<DeploymentIdentity>,
    /// Override a mismatched deployment binding in an existing state file.
    #[serde(default)]
    pub force_state: bool,
    /// After this many consecutive failures on the same deposit, park it and
    /// stop retrying it. `None` or `0` disables skipping (default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_after_attempts: Option<u32>,
    /// Base delay, in seconds, before a failed deposit is retried. Doubled
    /// on each subsequent failure of that id, capped at 64×. `0` retries
    /// immediately and disables age-based anchor-wait delays (tests). Anchor
    /// waits otherwise also use age / 4 seconds, capped at 60. Default 2.
    #[serde(default = "default_outstanding_backoff_secs")]
    pub outstanding_backoff_secs: u64,
    /// Seconds of observed anchor waiting before warning. `0` (default)
    /// warns immediately; repeats are limited to once per 15 minutes per id.
    #[serde(default)]
    pub anchor_wait_warn_after_secs: u64,
    /// Scan cursor shared with [`EthLogSource`](crate::source::EthLogSource),
    /// persisted as [`RelayerState::scan_done_through_block`]. After a
    /// finalize the relayer moves it to the deposit's block minus one. `None`
    /// inside the mutex means "scan from `--from-block`".
    #[serde(skip)]
    pub scan_cursor: Option<Arc<Mutex<Option<u64>>>>,
}

fn default_poll_interval() -> Duration {
    Duration::from_secs(2)
}

fn default_outstanding_backoff_secs() -> u64 {
    2
}

const ANCHOR_WAIT_WARN_REPEAT_SECS: u64 = 15 * 60;

impl RelayerConfig {
    /// Sensible defaults: start at deposit 0, 2-second polling.
    pub fn new(state_path: impl Into<PathBuf>) -> Self {
        Self {
            state_path: state_path.into(),
            start_deposit_id: 0,
            poll_interval: default_poll_interval(),
            deployment: None,
            force_state: false,
            skip_after_attempts: None,
            outstanding_backoff_secs: default_outstanding_backoff_secs(),
            anchor_wait_warn_after_secs: 0,
            scan_cursor: None,
        }
    }
}

/// What happened during a single [`Relayer::tick`] call.
#[derive(Clone, Debug)]
pub enum TickOutcome {
    /// The deposit was proven and finalised on AN; the cursor advanced.
    Finalized {
        deposit_id: u64,
        tx_hash: Option<[u8; 32]>,
    },
    /// The nullifier was already set on AN (another relayer beat us, or a
    /// prior submit we didn't observe succeeded). Cursor advances; no
    /// proving was done.
    AlreadyFinalized { deposit_id: u64 },
    /// The deposit is not made on Ethereum yet, or not `--confirmations`
    /// deep. Not a failure: it does not count toward `--skip-after-attempts`.
    NotYetAvailable { deposit_id: u64 },
    /// The confirmed deposit's block has not been anchored on AN yet. No
    /// proving or submission was attempted, and this does not count as failure.
    /// `first_wait` is true only if no start time is recorded for this wait;
    /// an existing wait stays non-new after restart. The daemon uses this
    /// flag to reset prior backoff for a new wait, while repeat waits can
    /// preserve active failure backoff.
    WaitingForAnchor { deposit_id: u64, first_wait: bool },
    /// Proof generation failed (witness fetch blip, circuit error). The
    /// relayer records the attempt and retries later; persistent failures
    /// surface via the attempt counter / warning.
    ProofFailed { deposit_id: u64, reason: String },
    /// AN rejected the finalize submission (verifier rejection, malformed
    /// call). Recorded as an attempt; a corrected re-prove may succeed.
    AnRejected { deposit_id: u64, reason: String },
    /// AN accepted the tx but confirmation timed out while still pending.
    /// The id stays outstanding with backoff so later deposits are not held
    /// behind it; it does not count toward `--skip-after-attempts`.
    AnPending { deposit_id: u64, reason: String },
    /// The deposit was parked after `--skip-after-attempts` and is no longer
    /// retried. Operator must run `finalize-one` manually.
    Skipped { deposit_id: u64, reason: String },
    /// The circuit cannot prove this deposit. Recorded in
    /// `unprovable_deposit_ids` and advanced past at once; there is no
    /// bundle, so `finalize-one` cannot help. Only the EVM bridge operator
    /// can return the funds.
    Unprovable { deposit_id: u64, reason: String },
}

/// Single relayer instance. Holds a [`DepositSource`], a [`ProofGenerator`]
/// and an [`AnSubmitter`].
pub struct Relayer<S: DepositSource, P: ProofGenerator, A: AnSubmitter> {
    config: RelayerConfig,
    source: Arc<S>,
    prover: Arc<P>,
    submitter: Arc<A>,
    state: RelayerState,
    /// Last outstanding id retried in the current round.
    last_outstanding_retry: Option<u64>,
    /// Freeze the end of each retry round so a growing queue cannot keep
    /// extending it and prevent a return to older ids.
    outstanding_round_end: Option<u64>,
    /// Alternate ready retries with forward work while both exist.
    retry_outstanding_next: bool,
}

impl<S: DepositSource, P: ProofGenerator, A: AnSubmitter> Relayer<S, P, A> {
    /// Build a relayer, loading any existing state from disk. If
    /// `state.json` doesn't exist, a fresh [`RelayerState`] is created.
    pub fn new(
        config: RelayerConfig,
        source: Arc<S>,
        prover: Arc<P>,
        submitter: Arc<A>,
    ) -> Result<Self, RelayerError> {
        let mut state = RelayerState::load(&config.state_path)?.unwrap_or_default();
        if let Some(deployment) = &config.deployment {
            if let Some(dropped) = state.ensure_deployment(deployment, config.force_state)? {
                warn!(
                    old_deployment = ?dropped.deployment,
                    old_last_processed_deposit_id = ?dropped.last_processed_deposit_id,
                    old_scan_done_through_block = ?dropped.scan_done_through_block,
                    old_parked_deposit_ids = ?dropped.parked_deposit_ids,
                    old_unprovable_deposit_ids = ?dropped.unprovable_deposit_ids,
                    old_outstanding_deposit_ids = ?dropped
                        .outstanding_deposits
                        .iter()
                        .map(|o| o.deposit_id)
                        .collect::<Vec<_>>(),
                    old_last_finalized_deposit_id = ?dropped.last_finalized_deposit_id,
                    start_deposit_id = config.start_deposit_id,
                    "--force-state: state.json belonged to another deployment; starting over",
                );
                if let Some(cursor) = &config.scan_cursor {
                    *cursor
                        .lock()
                        .map_err(|e| RelayerError::other(e.to_string()))? = None;
                }
            }
        }
        let retry_outstanding_next = !state.outstanding_deposits.is_empty();
        Ok(Self {
            config,
            source,
            prover,
            submitter,
            state,
            last_outstanding_retry: None,
            outstanding_round_end: None,
            retry_outstanding_next,
        })
    }

    /// Read-only access to the persisted state (for tests + CLI status).
    pub fn state(&self) -> &RelayerState {
        &self.state
    }

    /// One step of the loop. Idempotent: a deposit already finalised on AN
    /// is skipped via the nullifier pre-check rather than re-submitted.
    ///
    /// Alternate forward work with ready outstanding retries. When no new
    /// deposit is available, use that same tick for a retry instead of leaving
    /// the slot idle. Both queues get turns even under continuous arrivals.
    pub async fn tick(&mut self) -> Result<TickOutcome, RelayerError> {
        let now = unix_now_secs();
        let forward = self.state.next_target_at(self.config.start_deposit_id, now);
        if self.retry_outstanding_next {
            self.retry_outstanding_next = false;
            if let Some(id) = self.next_ready_outstanding(now) {
                return self.tick_target(id).await;
            }
        }
        // Change turns before awaiting work, including work that returns an
        // error, so a failing target cannot monopolize subsequent ticks.
        self.retry_outstanding_next = true;
        let outcome = self.tick_target(forward).await?;
        if matches!(outcome, TickOutcome::NotYetAvailable { .. }) {
            if let Some(id) = self.next_ready_outstanding(now) {
                self.retry_outstanding_next = false;
                return self.tick_target(id).await;
            }
        }
        Ok(outcome)
    }

    fn next_ready_outstanding(&mut self, now: u64) -> Option<u64> {
        let next = self
            .state
            .ready_outstanding_after(now, self.last_outstanding_retry);
        if let (Some(id), Some(last), Some(end)) = (
            next,
            self.last_outstanding_retry,
            self.outstanding_round_end,
        ) {
            if id > last && id <= end {
                self.last_outstanding_retry = Some(id);
                return Some(id);
            }
        }
        // Start a new finite round. Entries appended during this round join
        // the next one; deferred entries are reconsidered after backoff.
        self.outstanding_round_end = self
            .state
            .outstanding_deposits
            .iter()
            .map(|o| o.deposit_id)
            .max();
        self.last_outstanding_retry = self.state.ready_outstanding_after(now, None);
        self.last_outstanding_retry
    }

    async fn tick_target(&mut self, target: u64) -> Result<TickOutcome, RelayerError> {
        self.publish_state_gauges(target);

        // 1. Fetch the confirmed deposit event. Outstanding ids load from the stored tx
        //    / block so the shared scan cursor can keep moving.
        debug!(deposit_id = target, "fetching deposit event");
        let timer = StageTimer::start("fetch_event");
        let known = self.state.outstanding(target).map(|o| {
            (
                o.tx_hash.as_deref().and_then(|s| s.parse().ok()),
                o.log_index,
                o.block_number,
            )
        });
        let fetched = match known {
            Some((tx_hash, log_index, block_number)) => {
                self.source
                    .fetch_known(target, tx_hash, log_index, block_number)
                    .await
            },
            None => self.source.fetch(target).await,
        };
        timer.finish();
        let event = match fetched? {
            Some(e) => e,
            None => {
                // Nothing to attempt yet. Persist anyway: the source may have
                // moved the scan cursor.
                self.persist_state()?;
                debug!(deposit_id = target, "no confirmed deposit yet");
                return Ok(TickOutcome::NotYetAvailable {
                    deposit_id: target,
                });
            },
        };

        if event.deposit_id != target {
            return Err(RelayerError::DepositIdMismatch {
                requested: target,
                got: event.deposit_id,
            });
        }

        // 2. Cheap pre-check — skip proving on replays. Needs the event: the deposit's
        //    AN identity includes its source contract and chain.
        let timer = StageTimer::start("is_finalized");
        let finalized = self.submitter.is_finalized(&event).await;
        timer.finish();
        if finalized? {
            self.mark_finalized_block(event.block_number);
            self.state.record_progress(target);
            self.persist_state()?;
            info!(
                deposit_id = target,
                "already finalized on AN; advancing without proving"
            );
            return Ok(TickOutcome::AlreadyFinalized {
                deposit_id: target,
            });
        }

        // Ethereum confirmation depth is not AN anchor readiness: the light
        // client still needs finality and proving time. Wait before doing any
        // expensive work. Keep the log for retries, leaving new ids runnable.
        let timer = StageTimer::start("is_block_accepted");
        let accepted = self.submitter.is_block_accepted(&event).await;
        timer.finish();
        if !accepted? {
            let now = unix_now_secs();
            let first_wait = self
                .state
                .outstanding(target)
                .and_then(|o| o.anchor_wait_started_unix)
                .is_none();
            self.state.record_anchor_wait(
                target,
                now,
                self.config.outstanding_backoff_secs,
                deposit_log_ref(&event),
            );
            let warning_age_secs = self.state.record_anchor_wait_warning(
                target,
                now,
                self.config.anchor_wait_warn_after_secs,
                ANCHOR_WAIT_WARN_REPEAT_SECS,
            );
            if first_wait {
                info!(
                    deposit_id = target,
                    chain_id = event.source_chain_id,
                    block_number = event.block_number,
                    block_hash = %event.block_hash,
                    "waiting for AN block anchor; proving and submission deferred"
                );
            }
            if let Some(wait_secs) = warning_age_secs {
                let status = if first_wait {
                    "anchor wait started"
                } else if self.config.anchor_wait_warn_after_secs > 0 {
                    "anchor wait exceeded the configured warning delay"
                } else {
                    "anchor wait continues"
                };
                warn!(
                    deposit_id = target,
                    chain_id = event.source_chain_id,
                    block_number = event.block_number,
                    block_hash = %event.block_hash,
                    wait_secs,
                    status,
                    "AN block anchor missing; expected delivery may still be in progress; \
                     inspect the light-client relayer and block coverage; if manual admission \
                     is needed, compare this block_number and block_hash using eth_getBlockByNumber \
                     on an independent RPC for chain_id before setAcceptedBlockHash(chainId, \
                     blockHash, true) while owner anchors are enabled, otherwise through the light client"
                );
            }
            // Emit the deposit identifiers even if saving fails. A failed
            // save remains a tick error, and no proving/submission follows.
            self.persist_state()?;
            return Ok(TickOutcome::WaitingForAnchor {
                deposit_id: target,
                first_wait,
            });
        }
        if self.state.clear_anchor_wait(target) {
            self.persist_state()?;
        }

        // 3. Generate the proof triple. Treated as recoverable: a transient
        //    witness-fetch failure shouldn't crash the daemon.
        let timer = StageTimer::start("prove");
        let generated = self.prover.generate(&event).await;
        timer.finish();
        let bundle = match generated {
            Ok(b) => b,
            // The same deposit gets the same answer every time: no retry
            // holds the queue behind it.
            Err(RelayerError::Unprovable(reason)) => {
                self.state.record_unprovable(target);
                self.persist_state()?;
                error!(
                    deposit_id = target,
                    reason = %reason,
                    unprovable = ?self.state.unprovable_deposit_ids,
                    "deposit cannot be proven; recorded in unprovable_deposit_ids and skipped at \
                     once. finalize-one cannot help; only the EVM bridge operator can return its \
                     funds",
                );
                return Ok(TickOutcome::Unprovable {
                    deposit_id: target,
                    reason: format!("unprovable: {reason}"),
                });
            },
            Err(e) => {
                if let Some(outcome) =
                    self.record_failure(target, &format!("proof generation: {e}"), Some(&event))?
                {
                    return Ok(outcome);
                }
                let reason = e.to_string();
                warn!(deposit_id = target, reason = %reason, "proof generation failed");
                return Ok(TickOutcome::ProofFailed {
                    deposit_id: target,
                    reason,
                });
            },
        };

        // 4. Submit + finalise on AN.
        let timer = StageTimer::start("submit");
        let submitted = self.submitter.submit(&event, &bundle).await;
        timer.finish();
        match submitted? {
            SubmitOutcome::Finalized {
                tx_hash,
            } => {
                self.mark_finalized_block(event.block_number);
                self.state.record_progress(target);
                self.persist_state()?;
                info!(
                    deposit_id = target,
                    amount = ?event.amount,
                    sender = %event.sender,
                    "finalized on AN",
                );
                Ok(TickOutcome::Finalized {
                    deposit_id: target,
                    tx_hash,
                })
            },
            SubmitOutcome::AlreadyFinalized => {
                self.mark_finalized_block(event.block_number);
                self.state.record_progress(target);
                self.persist_state()?;
                info!(
                    deposit_id = target,
                    "AN reports already finalized; advancing"
                );
                Ok(TickOutcome::AlreadyFinalized {
                    deposit_id: target,
                })
            },
            SubmitOutcome::Rejected {
                reason,
            } => {
                if let Some(outcome) =
                    self.record_failure(target, &format!("AN rejected: {reason}"), Some(&event))?
                {
                    return Ok(outcome);
                }
                warn!(
                    deposit_id = target,
                    attempts = self.state.attempts_since_progress,
                    reason = %reason,
                    "AN rejected finalizeDeposit",
                );
                Ok(TickOutcome::AnRejected {
                    deposit_id: target,
                    reason,
                })
            },
            SubmitOutcome::Pending {
                reason,
            } => {
                self.state.record_pending(
                    target,
                    unix_now_secs(),
                    self.config.outstanding_backoff_secs,
                    Some(deposit_log_ref(&event)),
                );
                self.persist_state()?;
                info!(
                    deposit_id = target,
                    reason = %reason,
                    "finalizeDeposit still pending; will retry",
                );
                Ok(TickOutcome::AnPending {
                    deposit_id: target,
                    reason,
                })
            },
        }
    }

    /// Convenience: tick repeatedly with `poll_interval` between iterations,
    /// until `max_ticks` is reached or `should_stop` returns `true`.
    pub async fn run_loop(
        &mut self,
        max_ticks: usize,
        mut should_stop: impl FnMut(&TickOutcome) -> bool,
    ) -> Result<Vec<TickOutcome>, RelayerError> {
        let mut history = Vec::with_capacity(max_ticks);
        for _ in 0..max_ticks {
            let outcome = self.tick().await?;
            let stop = should_stop(&outcome);
            history.push(outcome);
            if stop {
                break;
            }
            tokio::time::sleep(self.config.poll_interval).await;
        }
        Ok(history)
    }

    fn persist_state(&mut self) -> Result<(), RelayerError> {
        if let Some(cursor) = &self.config.scan_cursor {
            self.state.scan_done_through_block = *cursor
                .lock()
                .map_err(|e| RelayerError::other(e.to_string()))?;
        }
        let saved = self.state.save(&self.config.state_path);
        let target = self.state.next_target(self.config.start_deposit_id);
        self.publish_state_gauges(target);
        saved
    }

    /// Mirror `state.json` and the current target into the Prometheus gauges.
    fn publish_state_gauges(&self, target: u64) {
        let waits: Vec<_> = self
            .state
            .outstanding_deposits
            .iter()
            .filter_map(|o| o.anchor_wait_started_unix)
            .collect();
        metrics::gauge!(prom::WAITING_FOR_ANCHOR).set(waits.len() as f64);
        metrics::gauge!(prom::OLDEST_ANCHOR_WAIT_TIMESTAMP_SECONDS)
            .set(waits.into_iter().min().unwrap_or(0) as f64);
        prom::set_state_gauges(
            target,
            self.state.attempts_since_progress,
            self.state.parked_deposit_ids.len(),
            self.state.unprovable_deposit_ids.len(),
            self.state.outstanding_deposits.len(),
            self.state.scan_done_through_block,
        );
    }

    /// Remember that the deposit at `block_number` is done. The next log
    /// scan still includes that block: a later `depositId` can share it.
    /// Outstanding retries do not use this cursor: they load the stored tx.
    fn mark_finalized_block(&self, block_number: u64) {
        if let (Some(cursor), Some(through)) =
            (&self.config.scan_cursor, block_number.checked_sub(1))
        {
            advance_scan_cursor(cursor, through);
        }
    }

    /// Record a failed attempt and optionally park the deposit when the skip
    /// threshold is reached.
    fn record_failure(
        &mut self,
        deposit_id: u64,
        reason: &str,
        event: Option<&crate::types::DepositEvent>,
    ) -> Result<Option<TickOutcome>, RelayerError> {
        let log = event.map(deposit_log_ref);
        self.state.record_attempt_at(
            deposit_id,
            unix_now_secs(),
            self.config.outstanding_backoff_secs,
            log,
        );
        if let Some(limit) = self.config.skip_after_attempts {
            if limit > 0 && self.state.outstanding_attempts(deposit_id) >= limit {
                self.state.record_skip(deposit_id);
                self.persist_state()?;
                error!(
                    deposit_id,
                    attempts = limit,
                    reason,
                    parked = ?self.state.parked_deposit_ids,
                    "deposit parked after max attempts; run finalize-one manually",
                );
                return Ok(Some(TickOutcome::Skipped {
                    deposit_id,
                    reason: reason.to_string(),
                }));
            }
        }
        self.persist_state()?;
        Ok(None)
    }
}

fn deposit_log_ref(event: &crate::types::DepositEvent) -> crate::state::DepositLogRef {
    crate::state::DepositLogRef {
        block_number: event.block_number,
        tx_hash: event.tx_hash,
        log_index: event.log_index,
    }
}

// ─────────────────────────────────────────────────────────────────────
// Tests — drive the loop end-to-end against the mock stages
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };

    use alloy::primitives::{Address, B256, U256};
    use tempfile::tempdir;

    use super::*;
    use crate::{
        fake_rpc::FakeRpc,
        prover::MockProofGenerator,
        source::{EthLogSource, InMemoryDepositSource},
        submitter::MockAnSubmitter,
        types::{DepositEvent, DepositProofBundle},
    };

    #[derive(Default)]
    struct CountingProver(AtomicUsize);

    #[async_trait::async_trait]
    impl ProofGenerator for CountingProver {
        async fn generate(&self, event: &DepositEvent) -> Result<DepositProofBundle, RelayerError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            MockProofGenerator::new().generate(event).await
        }
    }

    struct AnchorGate {
        accepted: Mutex<std::collections::HashSet<u64>>,
        read_fails: AtomicBool,
        inner: MockAnSubmitter,
    }

    impl Default for AnchorGate {
        fn default() -> Self {
            Self {
                accepted: Mutex::default(),
                read_fails: AtomicBool::new(false),
                inner: MockAnSubmitter::accepting(),
            }
        }
    }

    #[async_trait::async_trait]
    impl AnSubmitter for AnchorGate {
        async fn is_block_accepted(&self, event: &DepositEvent) -> Result<bool, RelayerError> {
            if self.read_fails.load(Ordering::Relaxed) {
                return Err(RelayerError::other("anchor read failed"));
            }
            Ok(self.accepted.lock().unwrap().contains(&event.deposit_id))
        }

        async fn is_finalized(&self, event: &DepositEvent) -> Result<bool, RelayerError> {
            self.inner.is_finalized(event).await
        }

        async fn submit(
            &self,
            event: &DepositEvent,
            bundle: &DepositProofBundle,
        ) -> Result<SubmitOutcome, RelayerError> {
            self.inner.submit(event, bundle).await
        }
    }

    #[tokio::test]
    async fn anchor_wait_survives_restart_without_proving_parking_or_blocking_later_deposits() {
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new());
        source.insert(deposit(0));
        source.insert(deposit(1));
        let prover = Arc::new(CountingProver::default());
        let gate = Arc::new(AnchorGate::default());
        gate.accepted.lock().unwrap().insert(1);
        let cfg = RelayerConfig {
            skip_after_attempts: Some(1),
            outstanding_backoff_secs: 0,
            ..RelayerConfig::new(dir.path().join("state.json"))
        };
        let mut relayer =
            Relayer::new(cfg.clone(), source.clone(), prover.clone(), gate.clone()).unwrap();
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::WaitingForAnchor {
                deposit_id: 0,
                first_wait: true
            }
        ));
        assert_eq!(prover.0.load(Ordering::Relaxed), 0);
        assert_eq!(gate.inner.finalized_count(), 0);
        let started = relayer
            .state()
            .outstanding(0)
            .unwrap()
            .anchor_wait_started_unix;
        assert!(started.is_some());
        assert_eq!(relayer.state().outstanding(0).unwrap().attempts, 0);
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::WaitingForAnchor {
                deposit_id: 0,
                first_wait: false
            }
        ));
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::Finalized {
                deposit_id: 1,
                ..
            }
        ));
        drop(relayer);

        let mut relayer = Relayer::new(cfg, source, prover.clone(), gate.clone()).unwrap();
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::WaitingForAnchor {
                deposit_id: 0,
                first_wait: false
            }
        ));
        assert_eq!(
            relayer
                .state()
                .outstanding(0)
                .unwrap()
                .anchor_wait_started_unix,
            started
        );
        assert_eq!(relayer.state().outstanding(0).unwrap().attempts, 0);
        assert_eq!(relayer.state().attempts_since_progress, 0);
        assert!(relayer.state().parked_deposit_ids.is_empty());
        assert_eq!(prover.0.load(Ordering::Relaxed), 1);
        gate.accepted.lock().unwrap().insert(0);
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::Finalized {
                deposit_id: 0,
                ..
            }
        ));
        assert_eq!(prover.0.load(Ordering::Relaxed), 2);
        assert_eq!(gate.inner.finalized_count(), 2);
        assert!(relayer.state().outstanding_deposits.is_empty());
    }

    #[tokio::test]
    async fn anchor_read_errors_stop_before_proving_and_do_not_become_anchor_waits() {
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new());
        source.insert(deposit(0));
        let prover = Arc::new(CountingProver::default());
        let gate = Arc::new(AnchorGate::default());
        gate.read_fails.store(true, Ordering::Relaxed);
        let mut relayer = Relayer::new(
            RelayerConfig::new(dir.path().join("state.json")),
            source,
            prover.clone(),
            gate.clone(),
        )
        .unwrap();
        assert!(relayer
            .tick()
            .await
            .unwrap_err()
            .to_string()
            .contains("anchor read failed"));
        assert_eq!(prover.0.load(Ordering::Relaxed), 0);
        assert_eq!(gate.inner.finalized_count(), 0);
        assert!(relayer.state().outstanding_deposits.is_empty());
    }

    #[tokio::test]
    async fn ready_anchor_waits_finish_during_continuous_forward_work() {
        // Cover both a stream of fresh, unanchored deposits and an already
        // anchored backlog. Neither stream may displace an older ready id.
        for new_blocks_accepted in [false, true] {
            let dir = tempdir().unwrap();
            let source = Arc::new(InMemoryDepositSource::new());
            source.insert(deposit(0));
            let gate = Arc::new(AnchorGate::default());
            let mut relayer = Relayer::new(
                RelayerConfig {
                    outstanding_backoff_secs: 0,
                    ..RelayerConfig::new(dir.path().join("state.json"))
                },
                source.clone(),
                Arc::new(CountingProver::default()),
                gate.clone(),
            )
            .unwrap();
            assert!(matches!(
                relayer.tick().await.unwrap(),
                TickOutcome::WaitingForAnchor {
                    deposit_id: 0,
                    ..
                }
            ));
            gate.accepted.lock().unwrap().insert(0);
            for id in 1..=8 {
                source.insert(deposit(id));
                if new_blocks_accepted {
                    gate.accepted.lock().unwrap().insert(id);
                }
                relayer.tick().await.unwrap();
            }
            assert!(
                gate.inner.is_finalized(&deposit(0)).await.unwrap(),
                "ready id 0 starved; new_blocks_accepted={new_blocks_accepted}"
            );
            if new_blocks_accepted {
                assert!(
                    gate.inner.finalized_count() > 1,
                    "forward work must also progress"
                );
            } else {
                assert!(
                    relayer
                        .state()
                        .outstanding_deposits
                        .iter()
                        .any(|o| o.deposit_id > 0),
                    "forward work must also progress"
                );
            }
        }
    }

    #[tokio::test]
    async fn growing_retry_queue_cannot_keep_an_older_wait_outside_the_round() {
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new());
        source.insert(deposit(0));
        let gate = Arc::new(AnchorGate::default());
        let mut relayer = Relayer::new(
            RelayerConfig {
                outstanding_backoff_secs: 0,
                ..RelayerConfig::new(dir.path().join("state.json"))
            },
            source.clone(),
            Arc::new(CountingProver::default()),
            gate.clone(),
        )
        .unwrap();
        relayer.tick().await.unwrap();
        for id in 1..=24 {
            source.insert(deposit(id));
            if id == 6 {
                // Let the retry cursor pass 0 while its anchor is absent.
                gate.accepted.lock().unwrap().insert(0);
            }
            relayer.tick().await.unwrap();
        }
        assert!(
            gate.inner.is_finalized(&deposit(0)).await.unwrap(),
            "new arrivals kept extending the retry round indefinitely"
        );
    }

    #[tokio::test]
    async fn anchor_read_error_does_not_keep_the_other_queue_from_its_turn() {
        for error_on_retry in [false, true] {
            let dir = tempdir().unwrap();
            let source = Arc::new(InMemoryDepositSource::new());
            source.insert(deposit(0));
            let gate = Arc::new(AnchorGate::default());
            let mut relayer = Relayer::new(
                RelayerConfig {
                    outstanding_backoff_secs: 0,
                    ..RelayerConfig::new(dir.path().join("state.json"))
                },
                source.clone(),
                Arc::new(CountingProver::default()),
                gate.clone(),
            )
            .unwrap();
            relayer.tick().await.unwrap();
            if !error_on_retry {
                relayer.tick().await.unwrap(); // Give the existing retry its
                                               // turn first.
            }
            source.insert(deposit(1));
            gate.read_fails.store(true, Ordering::Relaxed);
            assert!(relayer.tick().await.is_err());
            gate.read_fails.store(false, Ordering::Relaxed);
            gate.accepted.lock().unwrap().extend([0, 1]);
            let expected = if error_on_retry { 1 } else { 0 };
            assert!(
                matches!(relayer.tick().await.unwrap(), TickOutcome::Finalized { deposit_id, .. } if deposit_id == expected)
            );
        }
    }

    #[tokio::test]
    async fn daemon_counts_anchor_waits_without_rejections_or_failed_attempts() {
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new());
        source.insert(deposit(0));
        let prover = Arc::new(CountingProver::default());
        let gate = Arc::new(AnchorGate::default());
        let mut relayer = Relayer::new(
            RelayerConfig::new(dir.path().join("state.json")),
            source,
            prover.clone(),
            gate.clone(),
        )
        .unwrap();
        let counters = crate::daemon::RelayerMetrics::new();
        // An already-ready shutdown stops at the first tick boundary, after
        // that tick's outcome has been counted by the actual daemon loop.
        let summary = relayer
            .run_until_shutdown(
                crate::daemon::BackoffConfig::default(),
                Some(counters.clone()),
                std::future::ready(()),
            )
            .await
            .unwrap();
        assert_eq!(summary.ticks, 1);
        assert_eq!(summary.an_rejected, 0);
        assert_eq!(summary.not_yet_available, 1);
        assert_eq!(
            summary.last_outcome,
            Some(crate::daemon::LastOutcome::WaitingForAnchor {
                deposit_id: 0
            })
        );
        assert_eq!(counters.snapshot().an_rejected, 0);
        assert_eq!(counters.snapshot().proof_failed, 0);
        assert_eq!(relayer.state().attempts_since_progress, 0);
        assert_eq!(prover.0.load(Ordering::Relaxed), 0);
        assert_eq!(gate.inner.finalized_count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_anchor_waits_preserve_forward_error_backoff_until_progress() {
        struct ForwardErrorSource;

        #[async_trait::async_trait]
        impl DepositSource for ForwardErrorSource {
            async fn fetch(&self, id: u64) -> Result<Option<DepositEvent>, RelayerError> {
                if id == 0 {
                    Ok(Some(deposit(0)))
                } else {
                    Err(RelayerError::eth("forward RPC unavailable"))
                }
            }
        }

        let dir = tempdir().unwrap();
        let gate = Arc::new(AnchorGate::default());
        let prover = Arc::new(CountingProver::default());
        let mut r = Relayer::new(
            RelayerConfig {
                outstanding_backoff_secs: 0,
                ..RelayerConfig::new(dir.path().join("state.json"))
            },
            Arc::new(ForwardErrorSource),
            prover.clone(),
            gate.clone(),
        )
        .unwrap();
        let counters = crate::daemon::RelayerMetrics::new();
        let summary = r
            .run_until_shutdown(
                crate::daemon::BackoffConfig {
                    initial: Duration::from_secs(2),
                    max: Duration::from_secs(16),
                    multiplier: 2,
                },
                Some(counters.clone()),
                async {
                    loop {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        let snap = counters.snapshot();
                        let expected = match snap.ticks {
                            0..=2 | 14.. => 2,
                            3..=4 => 4,
                            5..=6 => 8,
                            _ => 16,
                        };
                        assert_eq!(snap.current_backoff_secs, expected);
                        if snap.ticks == 12 {
                            // An arrived anchor lets actual progress reset
                            // the error backoff on the next retry turn.
                            gate.accepted.lock().unwrap().insert(0);
                        }
                        if snap.ticks >= 14 {
                            return;
                        }
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(summary.tick_errors, 6);
        assert_eq!(summary.finalized, 1);
        assert_eq!(summary.an_rejected, 0);
        assert_eq!(prover.0.load(Ordering::Relaxed), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn anchor_waits_reset_idle_or_failure_backoff_and_keep_polling() {
        for start_with_error in [false, true] {
            let dir = tempdir().unwrap();
            let source = Arc::new(InMemoryDepositSource::new());
            let gate = Arc::new(AnchorGate::default());
            let prover = Arc::new(CountingProver::default());
            if start_with_error {
                source.insert(deposit(0));
                gate.read_fails.store(true, Ordering::Relaxed);
            }
            let mut relayer = Relayer::new(
                RelayerConfig {
                    outstanding_backoff_secs: 0,
                    ..RelayerConfig::new(dir.path().join("state.json"))
                },
                source.clone(),
                prover.clone(),
                gate.clone(),
            )
            .unwrap();
            let counters = crate::daemon::RelayerMetrics::new();
            let backoff = crate::daemon::BackoffConfig {
                initial: Duration::from_secs(2),
                max: Duration::from_secs(16),
                multiplier: 2,
            };
            let summary = relayer
                .run_until_shutdown(backoff, Some(counters.clone()), async {
                    let mut switched_to_wait = false;
                    loop {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        let snap = counters.snapshot();
                        if snap.ticks >= 4 && !switched_to_wait {
                            assert_eq!(snap.current_backoff_secs, 16);
                            source.insert(deposit(0));
                            gate.read_fails.store(false, Ordering::Relaxed);
                            switched_to_wait = true;
                        }
                        if snap.ticks >= 5 {
                            assert_eq!(
                                snap.current_backoff_secs, 2,
                                "normal anchor waits must reset and retain the initial polling \
                                 delay"
                            );
                            if snap.ticks >= 10 {
                                return;
                            }
                        }
                    }
                })
                .await
                .unwrap();
            assert_eq!(summary.tick_errors, if start_with_error { 4 } else { 0 });
            assert_eq!(summary.finalized, 0);
            assert_eq!(summary.an_rejected, 0);
            assert_eq!(prover.0.load(Ordering::Relaxed), 0);
            assert_eq!(gate.inner.finalized_count(), 0);
        }
    }

    fn deposit(id: u64) -> DepositEvent {
        DepositEvent {
            deposit_id: id,
            sender: Address::repeat_byte(0x11),
            amount: U256::from(id * 1000 + 1),
            an_account: B256::repeat_byte(0x33),
            timestamp: U256::from(1_700_000_000u64),
            tx_hash: B256::repeat_byte(0xaa),
            log_index: 0,
            block_number: 100 + id,
            block_hash: B256::repeat_byte(0xcd),
            source_contract: Address::repeat_byte(0x22),
            source_chain_id: 1,
        }
    }

    type R = Relayer<InMemoryDepositSource, MockProofGenerator, MockAnSubmitter>;

    fn make_relayer(
        source: Arc<InMemoryDepositSource>,
        prover: Arc<MockProofGenerator>,
        submitter: Arc<MockAnSubmitter>,
        state_path: PathBuf,
    ) -> R {
        let cfg = RelayerConfig {
            state_path,
            start_deposit_id: 0,
            poll_interval: Duration::from_millis(0),
            deployment: None,
            force_state: false,
            skip_after_attempts: None,
            outstanding_backoff_secs: 0,
            anchor_wait_warn_after_secs: 0,
            scan_cursor: None,
        };
        Relayer::new(cfg, source, prover, submitter).unwrap()
    }

    #[tokio::test]
    async fn force_state_onto_another_bridge_starts_from_its_first_deposit() {
        let dir = tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let old_bridge = DeploymentIdentity::new(1, Address::repeat_byte(0x01), "0x0");
        let mut old = RelayerState {
            deployment: Some(old_bridge),
            scan_done_through_block: Some(900),
            ..RelayerState::default()
        };
        old.record_progress(100);
        old.save(&state_path).unwrap();

        let source = Arc::new(InMemoryDepositSource::new());
        source.insert(deposit(0));
        let cursor = Arc::new(Mutex::new(old.scan_done_through_block));
        let cfg = RelayerConfig {
            deployment: Some(DeploymentIdentity::new(
                1,
                Address::repeat_byte(0x02),
                "0x0",
            )),
            force_state: true,
            scan_cursor: Some(cursor.clone()),
            ..RelayerConfig::new(&state_path)
        };
        let mut relayer = Relayer::new(
            cfg,
            source,
            Arc::new(MockProofGenerator::new()),
            Arc::new(MockAnSubmitter::accepting()),
        )
        .unwrap();
        assert_eq!(*cursor.lock().unwrap(), None);

        match relayer.tick().await.unwrap() {
            TickOutcome::Finalized {
                deposit_id, ..
            } => assert_eq!(deposit_id, 0),
            other => panic!("expected Finalized(0), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn finalizes_five_deposits_in_order() {
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new());
        for id in 0..5 {
            source.insert(deposit(id));
        }
        let prover = Arc::new(MockProofGenerator::new());
        let submitter = Arc::new(MockAnSubmitter::accepting());

        let mut relayer = make_relayer(
            source,
            prover,
            submitter.clone(),
            dir.path().join("state.json"),
        );

        for id in 0..5 {
            match relayer.tick().await.unwrap() {
                TickOutcome::Finalized {
                    deposit_id, ..
                } => assert_eq!(deposit_id, id),
                other => panic!("expected Finalized at {id}, got {other:?}"),
            }
        }
        assert_eq!(submitter.finalized_count(), 5);
        assert_eq!(submitter.finalized_log(), vec![0, 1, 2, 3, 4]);
        assert_eq!(relayer.state().last_processed_deposit_id, Some(4));
        assert_eq!(relayer.state().attempts_since_progress, 0);
    }

    #[tokio::test]
    async fn finalize_moves_the_cursor_to_the_block_before_the_deposit() {
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new());
        let mut a = deposit(0);
        a.block_number = 105;
        let mut b = deposit(1);
        b.block_number = 105;
        let mut c = deposit(2);
        c.block_number = 107;
        source.insert(a);
        source.insert(b);
        source.insert(c);
        let cursor = Arc::new(Mutex::new(None));
        let cfg = RelayerConfig {
            state_path: dir.path().join("state.json"),
            start_deposit_id: 0,
            poll_interval: Duration::from_millis(0),
            deployment: None,
            force_state: false,
            skip_after_attempts: None,
            outstanding_backoff_secs: 0,
            anchor_wait_warn_after_secs: 0,
            scan_cursor: Some(cursor.clone()),
        };
        let mut relayer = Relayer::new(
            cfg,
            source,
            Arc::new(MockProofGenerator::new()),
            Arc::new(MockAnSubmitter::accepting()),
        )
        .unwrap();

        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::Finalized {
                deposit_id: 0,
                ..
            }
        ));
        assert_eq!(*cursor.lock().unwrap(), Some(104));
        assert_eq!(relayer.state().scan_done_through_block, Some(104));

        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::Finalized {
                deposit_id: 1,
                ..
            }
        ));
        assert_eq!(*cursor.lock().unwrap(), Some(104));

        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::Finalized {
                deposit_id: 2,
                ..
            }
        ));
        assert_eq!(*cursor.lock().unwrap(), Some(106));
        assert_eq!(relayer.state().scan_done_through_block, Some(106));
    }

    #[tokio::test]
    async fn log_source_retry_and_same_block_neighbour_end_to_end() {
        // Deposits 0 and 1 share block 105, deposit 2 is in block 107; AN
        // rejects the first attempt at deposit 0. Restart with a fresh source
        // so its retry must fetch the stored tx's receipt, without a located
        // event cache. Forward work must still find the same-block neighbour.
        let dir = tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let bridge = Address::repeat_byte(0x22);
        let rpc = FakeRpc::new(bridge, 130);
        for block in [105, 105, 107] {
            rpc.deposit(block);
        }
        let cursor = Arc::new(Mutex::new(None));
        let source =
            EthLogSource::new(rpc.provider(), bridge, 100, 12).with_scan_cursor(cursor.clone());
        let rejected_once = AtomicBool::new(false);
        let submitter = MockAnSubmitter::with_verifier(Arc::new(move |_| {
            rejected_once.swap(true, Ordering::SeqCst)
        }));
        let submitter = Arc::new(submitter);
        let cfg = RelayerConfig {
            scan_cursor: Some(cursor.clone()),
            outstanding_backoff_secs: 0,
            ..RelayerConfig::new(&state_path)
        };
        let mut relayer = Relayer::new(
            cfg.clone(),
            Arc::new(source),
            Arc::new(MockProofGenerator::new()),
            submitter.clone(),
        )
        .unwrap();

        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::AnRejected {
                deposit_id: 0,
                ..
            }
        ));
        assert_eq!(*cursor.lock().unwrap(), Some(104));
        assert_eq!(
            RelayerState::load(&state_path)
                .unwrap()
                .unwrap()
                .scan_done_through_block,
            Some(104)
        );

        let saved = RelayerState::load(&state_path).unwrap().unwrap();
        let stored_tx: B256 = saved
            .outstanding(0)
            .unwrap()
            .tx_hash
            .as_ref()
            .unwrap()
            .parse()
            .unwrap();
        drop(relayer);
        let source =
            EthLogSource::new(rpc.provider(), bridge, 100, 12).with_scan_cursor(cursor.clone());
        let mut relayer = Relayer::new(
            cfg,
            Arc::new(source),
            Arc::new(MockProofGenerator::new()),
            submitter,
        )
        .unwrap();
        rpc.take_receipt_hashes();
        rpc.take_get_logs_ranges();

        // The ready retry gets its turn before forward work, without moving
        // the scan cursor past the same-block neighbour.
        for (id, cursor_after) in [(0, 104), (1, 104), (2, 106)] {
            match relayer.tick().await.unwrap() {
                TickOutcome::Finalized {
                    deposit_id, ..
                } => assert_eq!(deposit_id, id),
                other => panic!("expected Finalized({id}), got {other:?}"),
            }
            assert_eq!(*cursor.lock().unwrap(), Some(cursor_after));
            if id == 0 {
                assert_eq!(rpc.take_receipt_hashes(), vec![stored_tx]);
                assert!(rpc.take_get_logs_ranges().is_empty());
            }
        }

        // Nothing new at 3: the idle scan advances to the confirmed head.
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::NotYetAvailable {
                deposit_id: 3
            }
        ));
        assert_eq!(*cursor.lock().unwrap(), Some(118));
        let saved = RelayerState::load(&state_path).unwrap().unwrap();
        assert_eq!(saved.scan_done_through_block, Some(118));
        assert_eq!(saved.last_processed_deposit_id, Some(2));
    }

    #[tokio::test]
    async fn a_state_file_from_an_earlier_relayer_finds_the_stuck_deposit() {
        // A previous binary stored attempts on the last failed id with no
        // tx or block. After upgrade that id is outstanding and gets a retry
        // before new work. It must still be found when the saved scan cursor
        // is already past its block.
        let dir = tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        std::fs::write(
            &state_path,
            r#"{"last_processed_deposit_id":null,"last_attempt_deposit_id":0,
                "attempts_since_progress":3,"scan_done_through_block":109}"#,
        )
        .unwrap();
        let bridge = Address::repeat_byte(0x22);
        let rpc = FakeRpc::new(bridge, 130);
        for block in [105, 110] {
            rpc.deposit(block);
        }
        let cursor = Arc::new(Mutex::new(Some(109)));
        let source =
            EthLogSource::new(rpc.provider(), bridge, 100, 12).with_scan_cursor(cursor.clone());
        let cfg = RelayerConfig {
            scan_cursor: Some(cursor.clone()),
            outstanding_backoff_secs: 0,
            skip_after_attempts: Some(5),
            ..RelayerConfig::new(&state_path)
        };
        let mut relayer = Relayer::new(
            cfg,
            Arc::new(source),
            Arc::new(MockProofGenerator::new()),
            Arc::new(MockAnSubmitter::accepting()),
        )
        .unwrap();
        assert_eq!(relayer.state().outstanding_attempts(0), 3);
        assert!(relayer.state().outstanding(0).unwrap().tx_hash.is_none());

        for id in [0, 1] {
            match relayer.tick().await.unwrap() {
                TickOutcome::Finalized {
                    deposit_id, ..
                } => assert_eq!(deposit_id, id),
                other => panic!("expected Finalized({id}), got {other:?}"),
            }
        }
        assert!(relayer.state().outstanding_deposits.is_empty());
        assert!(relayer.state().parked_deposit_ids.is_empty());
        assert_eq!(relayer.state().last_processed_deposit_id, Some(1));
        assert_eq!(relayer.state().last_finalized_deposit_id, Some(1));
    }

    #[tokio::test]
    async fn not_yet_available_then_recovers() {
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new());
        let prover = Arc::new(MockProofGenerator::new());
        let submitter = Arc::new(MockAnSubmitter::accepting());
        let mut relayer = make_relayer(
            source.clone(),
            prover,
            submitter,
            dir.path().join("state.json"),
        );

        match relayer.tick().await.unwrap() {
            TickOutcome::NotYetAvailable {
                deposit_id,
            } => assert_eq!(deposit_id, 0),
            other => panic!("expected NotYetAvailable, got {other:?}"),
        }
        assert_eq!(relayer.state().last_processed_deposit_id, None);
        assert_eq!(relayer.state().attempts_since_progress, 0);

        source.insert(deposit(0));
        match relayer.tick().await.unwrap() {
            TickOutcome::Finalized {
                deposit_id, ..
            } => assert_eq!(deposit_id, 0),
            other => panic!("expected Finalized, got {other:?}"),
        }
        assert_eq!(relayer.state().attempts_since_progress, 0);
    }

    #[tokio::test]
    async fn skips_already_finalized_without_proving() {
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new());
        source.insert(deposit(0));
        source.insert(deposit(1));
        let prover = Arc::new(MockProofGenerator::new());
        let submitter = Arc::new(MockAnSubmitter::accepting());
        submitter.seed_finalized(0);

        let mut relayer = make_relayer(
            source,
            prover,
            submitter.clone(),
            dir.path().join("state.json"),
        );

        match relayer.tick().await.unwrap() {
            TickOutcome::AlreadyFinalized {
                deposit_id,
            } => assert_eq!(deposit_id, 0),
            other => panic!("expected AlreadyFinalized, got {other:?}"),
        }
        // Cursor advanced; next tick finalises deposit 1.
        match relayer.tick().await.unwrap() {
            TickOutcome::Finalized {
                deposit_id, ..
            } => assert_eq!(deposit_id, 1),
            other => panic!("expected Finalized, got {other:?}"),
        }
        // Only deposit 1 was actually proven + submitted.
        assert_eq!(submitter.finalized_log(), vec![1]);
    }

    #[tokio::test]
    async fn proof_failure_is_recoverable() {
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new());
        source.insert(deposit(0));
        let prover = Arc::new(MockProofGenerator::failing_on(0));
        let submitter = Arc::new(MockAnSubmitter::accepting());
        let mut relayer = make_relayer(
            source,
            prover,
            submitter.clone(),
            dir.path().join("state.json"),
        );

        match relayer.tick().await.unwrap() {
            TickOutcome::ProofFailed {
                deposit_id, ..
            } => assert_eq!(deposit_id, 0),
            other => panic!("expected ProofFailed, got {other:?}"),
        }
        assert_eq!(relayer.state().last_processed_deposit_id, None);
        assert_eq!(relayer.state().attempts_since_progress, 1);
        assert_eq!(submitter.finalized_count(), 0);
    }

    #[tokio::test]
    async fn an_rejection_is_recorded() {
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new());
        source.insert(deposit(0));
        let prover = Arc::new(MockProofGenerator::new());
        let submitter = Arc::new(MockAnSubmitter::with_verifier(Arc::new(|_| false)));
        let mut relayer = make_relayer(source, prover, submitter, dir.path().join("state.json"));

        match relayer.tick().await.unwrap() {
            TickOutcome::AnRejected {
                deposit_id, ..
            } => assert_eq!(deposit_id, 0),
            other => panic!("expected AnRejected, got {other:?}"),
        }
        assert_eq!(relayer.state().attempts_since_progress, 1);
    }

    #[tokio::test]
    async fn restart_resumes_from_persisted_state() {
        let dir = tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let source = Arc::new(InMemoryDepositSource::new());
        for id in 0..4 {
            source.insert(deposit(id));
        }
        let prover = Arc::new(MockProofGenerator::new());
        let submitter = Arc::new(MockAnSubmitter::accepting());

        {
            let mut r = make_relayer(
                source.clone(),
                prover.clone(),
                submitter.clone(),
                state_path.clone(),
            );
            assert!(matches!(r.tick().await.unwrap(), TickOutcome::Finalized {
                deposit_id: 0,
                ..
            }));
            assert!(matches!(r.tick().await.unwrap(), TickOutcome::Finalized {
                deposit_id: 1,
                ..
            }));
        }

        let mut r2 = make_relayer(source, prover, submitter.clone(), state_path);
        assert_eq!(r2.state().last_processed_deposit_id, Some(1));
        assert!(matches!(r2.tick().await.unwrap(), TickOutcome::Finalized {
            deposit_id: 2,
            ..
        }));
        assert!(matches!(r2.tick().await.unwrap(), TickOutcome::Finalized {
            deposit_id: 3,
            ..
        }));
        assert_eq!(submitter.finalized_count(), 4);
    }

    fn relayer_skipping_after(
        attempts: u32,
        source: Arc<InMemoryDepositSource>,
        prover: MockProofGenerator,
        state_path: PathBuf,
    ) -> R {
        let cfg = RelayerConfig {
            skip_after_attempts: Some(attempts),
            poll_interval: Duration::from_millis(0),
            outstanding_backoff_secs: 0,
            ..RelayerConfig::new(state_path)
        };
        Relayer::new(
            cfg,
            source,
            Arc::new(prover),
            Arc::new(MockAnSubmitter::accepting()),
        )
        .unwrap()
    }

    /// Answers each submit with the next outcome from its script.
    struct ScriptedSubmitter(Mutex<Vec<SubmitOutcome>>);

    #[async_trait::async_trait]
    impl AnSubmitter for ScriptedSubmitter {
        async fn is_block_accepted(&self, _: &DepositEvent) -> Result<bool, RelayerError> {
            Ok(true)
        }

        async fn is_finalized(&self, _: &DepositEvent) -> Result<bool, RelayerError> {
            Ok(false)
        }

        async fn submit(
            &self,
            _: &DepositEvent,
            _: &DepositProofBundle,
        ) -> Result<SubmitOutcome, RelayerError> {
            Ok(self.0.lock().unwrap().remove(0))
        }
    }

    #[tokio::test]
    async fn a_pending_submit_does_not_count_toward_parking() {
        // A long pause of the lane (exit 231) or slow confirmations must not
        // leave a healthy deposit one failure away from being parked.
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new());
        source.insert(deposit(0));
        let pending = || SubmitOutcome::Pending {
            reason: "paused".into(),
        };
        let rejected = || SubmitOutcome::Rejected {
            reason: "anchor".into(),
        };
        let script = vec![pending(), pending(), pending(), rejected(), rejected()];
        let cfg = RelayerConfig {
            skip_after_attempts: Some(2),
            outstanding_backoff_secs: 0,
            ..RelayerConfig::new(dir.path().join("state.json"))
        };
        let mut relayer = Relayer::new(
            cfg,
            source,
            Arc::new(MockProofGenerator::new()),
            Arc::new(ScriptedSubmitter(Mutex::new(script))),
        )
        .unwrap();

        for _ in 0..3 {
            assert!(matches!(
                relayer.tick().await.unwrap(),
                TickOutcome::AnPending {
                    deposit_id: 0,
                    ..
                }
            ));
        }
        assert_eq!(relayer.state().attempts_since_progress, 0);
        assert_eq!(relayer.state().outstanding_attempts(0), 0);
        assert!(relayer.state().outstanding(0).is_some());
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::AnRejected {
                deposit_id: 0,
                ..
            }
        ));
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::Skipped {
                deposit_id: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn skips_stuck_deposit_after_max_attempts() {
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new());
        source.insert(deposit(0));
        source.insert(deposit(1));
        let mut relayer = relayer_skipping_after(
            3,
            source,
            MockProofGenerator::failing_on(0),
            dir.path().join("state.json"),
        );

        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::ProofFailed {
                deposit_id: 0,
                ..
            }
        ));
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::ProofFailed {
                deposit_id: 0,
                ..
            }
        ));
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::Finalized {
                deposit_id: 1,
                ..
            }
        ));
        match relayer.tick().await.unwrap() {
            TickOutcome::Skipped {
                deposit_id, ..
            } => assert_eq!(deposit_id, 0),
            other => panic!("expected Skipped, got {other:?}"),
        }
        assert_eq!(relayer.state().parked_deposit_ids, vec![0]);
    }

    #[tokio::test]
    async fn a_failed_deposit_does_not_block_later_ids() {
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new());
        source.insert(deposit(0));
        source.insert(deposit(1));
        let cfg = RelayerConfig {
            outstanding_backoff_secs: 60,
            poll_interval: Duration::from_millis(0),
            ..RelayerConfig::new(dir.path().join("state.json"))
        };
        let mut relayer = Relayer::new(
            cfg,
            source,
            Arc::new(MockProofGenerator::failing_on(0)),
            Arc::new(MockAnSubmitter::accepting()),
        )
        .unwrap();

        match relayer.tick().await.unwrap() {
            TickOutcome::ProofFailed {
                deposit_id, ..
            } => assert_eq!(deposit_id, 0),
            other => panic!("expected ProofFailed, got {other:?}"),
        }
        match relayer.tick().await.unwrap() {
            TickOutcome::Finalized {
                deposit_id, ..
            } => assert_eq!(deposit_id, 1),
            other => panic!("expected Finalized(1), got {other:?}"),
        }
        assert_eq!(
            relayer
                .state()
                .outstanding_deposits
                .iter()
                .map(|o| o.deposit_id)
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert_eq!(relayer.state().last_processed_deposit_id, Some(1));
    }

    #[tokio::test]
    async fn idle_retries_share_the_slot_across_outstanding_ids() {
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new());
        source.insert(deposit(0));
        source.insert(deposit(1));
        let cfg = RelayerConfig {
            outstanding_backoff_secs: 0,
            poll_interval: Duration::from_millis(0),
            ..RelayerConfig::new(dir.path().join("state.json"))
        };
        let mut relayer = Relayer::new(
            cfg,
            source,
            Arc::new(MockProofGenerator::new()),
            Arc::new(MockAnSubmitter::with_verifier(Arc::new(|_| false))),
        )
        .unwrap();

        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::AnRejected {
                deposit_id: 0,
                ..
            }
        ));
        // The first ready retry gets a turn before discovering deposit 1.
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::AnRejected {
                deposit_id: 0,
                ..
            }
        ));
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::AnRejected {
                deposit_id: 1,
                ..
            }
        ));
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::AnRejected {
                deposit_id: 0,
                ..
            }
        ));
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::AnRejected {
                deposit_id: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn an_unprovable_deposit_is_skipped_at_once_even_without_a_skip_limit() {
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new());
        source.insert(deposit(0));
        source.insert(deposit(1));
        let mut relayer = Relayer::new(
            RelayerConfig {
                poll_interval: Duration::from_millis(0),
                ..RelayerConfig::new(dir.path().join("state.json"))
            },
            source,
            Arc::new(MockProofGenerator::unprovable_on(0)),
            Arc::new(MockAnSubmitter::accepting()),
        )
        .unwrap();
        match relayer.tick().await.unwrap() {
            TickOutcome::Unprovable {
                deposit_id,
                reason,
            } => {
                assert_eq!(deposit_id, 0);
                assert!(reason.starts_with("unprovable: "), "{reason}");
            },
            other => panic!("expected Unprovable, got {other:?}"),
        }
        assert_eq!(relayer.state().unprovable_deposit_ids, vec![0]);
        assert!(relayer.state().parked_deposit_ids.is_empty());
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::Finalized {
                deposit_id: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn idle_polls_an_older_daemon_counted_do_not_park_after_upgrade() {
        // An older daemon counted every empty poll as a failed attempt and
        // saved the count. After the upgrade one real failure must not reach
        // the skip threshold on top of it.
        let dir = tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        std::fs::write(
            &state_path,
            r#"{"last_processed_deposit_id":4,"last_attempt_deposit_id":5,
                "attempts_since_progress":500,"scanned_through_block":11800417}"#,
        )
        .unwrap();
        let source = Arc::new(InMemoryDepositSource::new());
        source.insert(deposit(5));
        let mut relayer =
            relayer_skipping_after(64, source, MockProofGenerator::failing_on(5), state_path);

        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::ProofFailed {
                deposit_id: 5,
                ..
            }
        ));
        assert_eq!(relayer.state().attempts_since_progress, 1);
        assert!(relayer.state().parked_deposit_ids.is_empty());
    }

    #[tokio::test]
    async fn waiting_for_a_deposit_never_parks_it() {
        // An idle bridge: the next id is not made yet, however long we poll.
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new());
        let mut relayer = relayer_skipping_after(
            3,
            source.clone(),
            MockProofGenerator::new(),
            dir.path().join("state.json"),
        );

        for _ in 0..10 {
            assert!(matches!(
                relayer.tick().await.unwrap(),
                TickOutcome::NotYetAvailable {
                    deposit_id: 0
                }
            ));
        }
        assert_eq!(relayer.state().attempts_since_progress, 0);
        assert!(relayer.state().parked_deposit_ids.is_empty());

        source.insert(deposit(0));
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::Finalized {
                deposit_id: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn run_loop_stops_when_should_stop_true() {
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new());
        for id in 0..5 {
            source.insert(deposit(id));
        }
        let prover = Arc::new(MockProofGenerator::new());
        let submitter = Arc::new(MockAnSubmitter::accepting());
        let mut r = make_relayer(source, prover, submitter, dir.path().join("state.json"));

        let history = r
            .run_loop(10, |o| {
                matches!(o, TickOutcome::Finalized {
                    deposit_id: 2,
                    ..
                })
            })
            .await
            .unwrap();
        assert_eq!(history.len(), 3);
    }
}
