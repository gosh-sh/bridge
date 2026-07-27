//! Relayer main loop (EVM→AN deposit direction).
//!
//! [`Relayer::tick`] is the testable single-step entry point. For the next
//! target `deposit_id` it:
//!
//! 1. cheaply checks the AN nullifier ([`AnSubmitter::is_finalized`]) so a
//!    replay skips the expensive proving step;
//! 2. asks the [`DepositSource`] for the confirmed `Deposit` event;
//! 3. generates the AN-consumable proof triple via the [`ProofGenerator`];
//! 4. submits it to AN via the [`AnSubmitter`] (`finalizeDeposit`);
//! 5. on success — advances [`RelayerState`] and persists it; otherwise records
//!    the attempt.
//!
//! [`Relayer::run_loop`] calls `tick` in a loop with a configurable delay and
//! a "max ticks" budget for tests; [`crate::daemon`] adds the long-running
//! backoff + shutdown wrapper.

use std::{path::PathBuf, sync::{Arc, Mutex}, time::Duration};

use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use crate::{
    error::RelayerError,
    prover::ProofGenerator,
    source::DepositSource,
    state::{DeploymentIdentity, RelayerState},
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
    /// After this many consecutive failures on the same deposit, emit a
    /// warning. Doesn't stop the relayer; operator-visible only.
    #[serde(default = "default_max_attempts_warn")]
    pub max_attempts_warn: u32,
    /// Deployment binding stamped into `state.json` (daemon only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment: Option<DeploymentIdentity>,
    /// Override a mismatched deployment binding in an existing state file.
    #[serde(default)]
    pub force_state: bool,
    /// After this many consecutive failures on the same deposit, park it and
    /// advance the cursor. `None` or `0` disables skipping (default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_after_attempts: Option<u32>,
    /// Shared with [`EthLogSource`] — highest safe head scanned so far.
    #[serde(skip)]
    pub scan_cursor: Option<Arc<Mutex<u64>>>,
}

fn default_poll_interval() -> Duration {
    Duration::from_secs(2)
}
fn default_max_attempts_warn() -> u32 {
    16
}

impl RelayerConfig {
    /// Sensible defaults: start at deposit 0, 2-second polling, warn after
    /// 16 attempts on the same deposit.
    pub fn new(state_path: impl Into<PathBuf>) -> Self {
        Self {
            state_path: state_path.into(),
            start_deposit_id: 0,
            poll_interval: default_poll_interval(),
            max_attempts_warn: default_max_attempts_warn(),
            deployment: None,
            force_state: false,
            skip_after_attempts: None,
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
    /// The deposit isn't visible / confirmed on Ethereum yet.
    NotYetAvailable { deposit_id: u64 },
    /// Proof generation failed (witness fetch blip, circuit error). The
    /// relayer records the attempt and retries later; persistent failures
    /// surface via the attempt counter / warning.
    ProofFailed { deposit_id: u64, reason: String },
    /// AN rejected the finalize submission (verifier rejection, malformed
    /// call). Recorded as an attempt; a corrected re-prove may succeed.
    AnRejected { deposit_id: u64, reason: String },
    /// AN accepted the tx but confirmation timed out while still pending.
    /// Retried later without advancing the cursor.
    AnPending { deposit_id: u64, reason: String },
    /// The deposit was parked after `--skip-after-attempts`; cursor advanced
    /// for liveness. Operator must run `finalize-one` manually.
    Skipped { deposit_id: u64, reason: String },
}

/// Single relayer instance. Holds a [`DepositSource`], a [`ProofGenerator`]
/// and an [`AnSubmitter`].
pub struct Relayer<S: DepositSource, P: ProofGenerator, A: AnSubmitter> {
    config: RelayerConfig,
    source: Arc<S>,
    prover: Arc<P>,
    submitter: Arc<A>,
    state: RelayerState,
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
            state.ensure_deployment(deployment, config.force_state)?;
        }
        Ok(Self {
            config,
            source,
            prover,
            submitter,
            state,
        })
    }

    /// Read-only access to the persisted state (for tests + CLI status).
    pub fn state(&self) -> &RelayerState {
        &self.state
    }

    /// One step of the loop. Idempotent: a deposit already finalised on AN
    /// is skipped via the nullifier pre-check rather than re-submitted.
    pub async fn tick(&mut self) -> Result<TickOutcome, RelayerError> {
        let target = self.state.next_target(self.config.start_deposit_id);

        // 1. Cheap nullifier pre-check — skip proving on replays.
        if self.submitter.is_finalized(target).await? {
            debug!(
                deposit_id = target,
                "already finalized on AN; advancing cursor"
            );
            self.state.record_progress(target);
            self.persist_state()?;
            return Ok(TickOutcome::AlreadyFinalized {
                deposit_id: target,
            });
        }

        // 2. Fetch the confirmed deposit event.
        debug!(deposit_id = target, "fetching deposit event");
        let event = match self.source.fetch(target).await? {
            Some(e) => e,
            None => {
                if let Some(outcome) = self.record_failure(target, "deposit not yet confirmed on Ethereum")? {
                    return Ok(outcome);
                }
                if self.state.attempts_since_progress >= self.config.max_attempts_warn {
                    warn!(
                        deposit_id = target,
                        attempts = self.state.attempts_since_progress,
                        "no confirmed deposit yet; relayer is idle",
                    );
                }
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

        // 3. Generate the proof triple. Treated as recoverable: a transient
        //    witness-fetch failure shouldn't crash the daemon.
        let bundle = match self.prover.generate(&event).await {
            Ok(b) => b,
            Err(e) => {
                if let Some(outcome) =
                    self.record_failure(target, &format!("proof generation: {e}"))?
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
        match self.submitter.submit(&event, &bundle).await? {
            SubmitOutcome::Finalized {
                tx_hash,
            } => {
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
                    self.record_failure(target, &format!("AN rejected: {reason}"))?
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
                self.state.record_attempt(target);
                self.persist_state()?;
                debug!(
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
            self.state.scanned_through_block =
                Some(*cursor.lock().map_err(|e| RelayerError::other(e.to_string()))?);
        }
        self.state.save(&self.config.state_path)
    }

    /// Record a failed attempt and optionally park the deposit when the skip
    /// threshold is reached.
    fn record_failure(
        &mut self,
        deposit_id: u64,
        reason: &str,
    ) -> Result<Option<TickOutcome>, RelayerError> {
        self.state.record_attempt(deposit_id);
        if let Some(limit) = self.config.skip_after_attempts {
            if limit > 0 && self.state.attempts_since_progress >= limit {
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

// ─────────────────────────────────────────────────────────────────────
// Tests — drive the loop end-to-end against the mock stages
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use alloy::primitives::{Address, B256, U256};
    use tempfile::tempdir;

    use super::*;
    use crate::{
        prover::MockProofGenerator, source::InMemoryDepositSource, submitter::MockAnSubmitter,
        types::DepositEvent,
    };

    fn deposit(id: u64) -> DepositEvent {
        DepositEvent {
            deposit_id: id,
            sender: Address::repeat_byte(0x11),
            amount: U256::from(id * 1000 + 1),
            an_workchain: 0,
            an_account: B256::repeat_byte(0x33),
            timestamp: U256::from(1_700_000_000u64),
            tx_hash: B256::repeat_byte(0xaa),
            log_index: 0,
            block_number: 100 + id,
            block_hash: B256::repeat_byte(0xcd),
            source_contract: Address::repeat_byte(0x22),
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
            max_attempts_warn: 16,
            deployment: None,
            force_state: false,
            skip_after_attempts: None,
            scan_cursor: None,
        };
        Relayer::new(cfg, source, prover, submitter).unwrap()
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
        assert_eq!(relayer.state().attempts_since_progress, 1);

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
        // Note: deposit 0 is NOT staged in the source — the nullifier
        // pre-check must let us advance past it without a source hit.
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

    #[tokio::test]
    async fn skips_stuck_deposit_after_max_attempts() {
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new());
        source.insert(deposit(1));
        let prover = Arc::new(MockProofGenerator::new());
        let submitter = Arc::new(MockAnSubmitter::accepting());
        let cfg = RelayerConfig {
            state_path: dir.path().join("state.json"),
            start_deposit_id: 0,
            poll_interval: Duration::from_millis(0),
            max_attempts_warn: 16,
            deployment: None,
            force_state: false,
            skip_after_attempts: Some(3),
            scan_cursor: None,
        };
        let mut relayer = Relayer::new(cfg, source, prover, submitter).unwrap();

        for _ in 0..2 {
            assert!(matches!(
                relayer.tick().await.unwrap(),
                TickOutcome::NotYetAvailable { deposit_id: 0 }
            ));
        }
        match relayer.tick().await.unwrap() {
            TickOutcome::Skipped { deposit_id, .. } => assert_eq!(deposit_id, 0),
            other => panic!("expected Skipped, got {other:?}"),
        }
        assert_eq!(relayer.state().parked_deposit_ids, vec![0]);
        assert_eq!(relayer.state().next_target(0), 1);
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
