//! Long-running daemon entry point for the deposit relayer.
//!
//! [`Relayer::run_until_shutdown`] drives `tick()` forever (until a passed-in
//! shutdown future resolves), applying exponential backoff between
//! non-success outcomes and resetting the backoff the moment a deposit is
//! finalised. It exposes structured [`RelayerMetrics`] for an external scrape
//! and is shutdown-aware via `tokio::select!`, so a SIGINT during a long
//! backoff sleep is honoured at the next tick boundary (never mid-`tick`, so a
//! `Finalized` outcome is always followed by a flushed `state.json`).

use std::{
    future::Future,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use tracing::{info, warn};

use crate::{
    error::RelayerError,
    prover::ProofGenerator,
    relayer::{Relayer, TickOutcome},
    source::DepositSource,
    submitter::AnSubmitter,
};

// ──────────────────────────────────────────────────────────────────────
// Backoff configuration
// ──────────────────────────────────────────────────────────────────────

/// Backoff policy for the daemon loop.
///
/// `current = min(initial * multiplier^consecutive_failures, max)`. On any
/// [`TickOutcome::Finalized`] / [`TickOutcome::AlreadyFinalized`] the streak
/// resets and `current` snaps back to `initial`.
#[derive(Clone, Copy, Debug)]
pub struct BackoffConfig {
    pub initial: Duration,
    pub max: Duration,
    pub multiplier: u32,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(2),
            max: Duration::from_secs(60),
            multiplier: 2,
        }
    }
}

impl BackoffConfig {
    fn bump(&self, current: Duration) -> Duration {
        let next = current.saturating_mul(self.multiplier);
        if next > self.max {
            self.max
        } else {
            next
        }
    }
}

// ──────────────────────────────────────────────────────────────────────
// Metrics
// ──────────────────────────────────────────────────────────────────────

/// Atomic counters for one running daemon. All counters are monotonic except
/// `current_backoff_secs` and `last_finalized_deposit_id`, which track the
/// latest value.
#[derive(Debug, Default)]
pub struct RelayerMetrics {
    pub ticks_total: AtomicU64,
    pub finalized_total: AtomicU64,
    pub already_finalized_total: AtomicU64,
    pub not_yet_available_total: AtomicU64,
    pub proof_failed_total: AtomicU64,
    pub an_rejected_total: AtomicU64,
    pub tick_errors_total: AtomicU64,
    pub current_backoff_secs: AtomicU64,
    pub last_finalized_deposit_id: AtomicU64,
}

impl RelayerMetrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn snapshot(&self) -> RelayerMetricsSnapshot {
        RelayerMetricsSnapshot {
            ticks: self.ticks_total.load(Ordering::Relaxed),
            finalized: self.finalized_total.load(Ordering::Relaxed),
            already_finalized: self.already_finalized_total.load(Ordering::Relaxed),
            not_yet_available: self.not_yet_available_total.load(Ordering::Relaxed),
            proof_failed: self.proof_failed_total.load(Ordering::Relaxed),
            an_rejected: self.an_rejected_total.load(Ordering::Relaxed),
            tick_errors: self.tick_errors_total.load(Ordering::Relaxed),
            current_backoff_secs: self.current_backoff_secs.load(Ordering::Relaxed),
            last_finalized_deposit_id: self.last_finalized_deposit_id.load(Ordering::Relaxed),
        }
    }
}

/// Plain-old-data snapshot of [`RelayerMetrics`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RelayerMetricsSnapshot {
    pub ticks: u64,
    pub finalized: u64,
    pub already_finalized: u64,
    pub not_yet_available: u64,
    pub proof_failed: u64,
    pub an_rejected: u64,
    pub tick_errors: u64,
    pub current_backoff_secs: u64,
    pub last_finalized_deposit_id: u64,
}

/// Returned by `run_until_shutdown` after the shutdown future resolves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DaemonRunSummary {
    pub ticks: u64,
    pub finalized: u64,
    pub already_finalized: u64,
    pub not_yet_available: u64,
    pub proof_failed: u64,
    pub an_rejected: u64,
    pub tick_errors: u64,
    pub last_outcome: Option<LastOutcome>,
}

/// Tag for [`DaemonRunSummary::last_outcome`]. Avoids embedding the full
/// `TickOutcome` (which can carry large proof bytes) in a log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LastOutcome {
    Finalized { deposit_id: u64 },
    AlreadyFinalized { deposit_id: u64 },
    NotYetAvailable { deposit_id: u64 },
    ProofFailed { deposit_id: u64 },
    AnRejected { deposit_id: u64 },
    TickError,
}

// ──────────────────────────────────────────────────────────────────────
// Relayer::run_until_shutdown
// ──────────────────────────────────────────────────────────────────────

impl<S: DepositSource, P: ProofGenerator, A: AnSubmitter> Relayer<S, P, A> {
    /// Drive `tick()` forever (or until `shutdown` resolves), applying
    /// exponential backoff between non-success outcomes.
    pub async fn run_until_shutdown<F>(
        &mut self,
        backoff: BackoffConfig,
        metrics: Option<Arc<RelayerMetrics>>,
        shutdown: F,
    ) -> Result<DaemonRunSummary, RelayerError>
    where
        F: Future<Output = ()>,
    {
        let mut summary = DaemonRunSummary::default();
        let mut current_delay = backoff.initial;
        if let Some(m) = &metrics {
            m.current_backoff_secs
                .store(current_delay.as_secs(), Ordering::Relaxed);
        }
        tokio::pin!(shutdown);

        loop {
            let outcome_or_err = self.tick().await;
            summary.ticks += 1;
            if let Some(m) = &metrics {
                m.ticks_total.fetch_add(1, Ordering::Relaxed);
            }

            let (success, last_tag) = match outcome_or_err {
                Ok(TickOutcome::Finalized {
                    deposit_id, ..
                }) => {
                    if let Some(m) = &metrics {
                        m.finalized_total.fetch_add(1, Ordering::Relaxed);
                        m.last_finalized_deposit_id
                            .store(deposit_id, Ordering::Relaxed);
                    }
                    summary.finalized += 1;
                    info!(deposit_id, "daemon: finalized");
                    (true, LastOutcome::Finalized {
                        deposit_id,
                    })
                },
                Ok(TickOutcome::AlreadyFinalized {
                    deposit_id,
                }) => {
                    if let Some(m) = &metrics {
                        m.already_finalized_total.fetch_add(1, Ordering::Relaxed);
                        m.last_finalized_deposit_id
                            .store(deposit_id, Ordering::Relaxed);
                    }
                    summary.already_finalized += 1;
                    // A nullifier skip is "progress" — reset backoff.
                    (true, LastOutcome::AlreadyFinalized {
                        deposit_id,
                    })
                },
                Ok(TickOutcome::NotYetAvailable {
                    deposit_id,
                }) => {
                    if let Some(m) = &metrics {
                        m.not_yet_available_total.fetch_add(1, Ordering::Relaxed);
                    }
                    summary.not_yet_available += 1;
                    (false, LastOutcome::NotYetAvailable {
                        deposit_id,
                    })
                },
                Ok(TickOutcome::ProofFailed {
                    deposit_id,
                    reason,
                }) => {
                    if let Some(m) = &metrics {
                        m.proof_failed_total.fetch_add(1, Ordering::Relaxed);
                    }
                    summary.proof_failed += 1;
                    warn!(deposit_id, reason = %reason, "daemon: proof generation failed");
                    (false, LastOutcome::ProofFailed {
                        deposit_id,
                    })
                },
                Ok(TickOutcome::AnRejected {
                    deposit_id,
                    reason,
                }) => {
                    if let Some(m) = &metrics {
                        m.an_rejected_total.fetch_add(1, Ordering::Relaxed);
                    }
                    summary.an_rejected += 1;
                    warn!(deposit_id, reason = %reason, "daemon: AN rejected finalizeDeposit");
                    (false, LastOutcome::AnRejected {
                        deposit_id,
                    })
                },
                Err(e) => {
                    if let Some(m) = &metrics {
                        m.tick_errors_total.fetch_add(1, Ordering::Relaxed);
                    }
                    summary.tick_errors += 1;
                    warn!(error = ?e, "daemon: tick failed, will back off");
                    (false, LastOutcome::TickError)
                },
            };
            summary.last_outcome = Some(last_tag);

            if success {
                current_delay = backoff.initial;
            } else {
                current_delay = backoff.bump(current_delay);
            }
            if let Some(m) = &metrics {
                m.current_backoff_secs
                    .store(current_delay.as_secs(), Ordering::Relaxed);
            }

            tokio::select! {
                biased;
                _ = &mut shutdown => {
                    info!(?summary, "daemon: shutdown signalled, exiting");
                    return Ok(summary);
                }
                _ = tokio::time::sleep(current_delay) => {}
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc, time::Duration};

    use alloy::primitives::{Address, B256, U256};
    use tempfile::tempdir;
    use tokio::sync::oneshot;

    use super::*;
    use crate::{
        prover::MockProofGenerator,
        relayer::{Relayer, RelayerConfig},
        source::InMemoryDepositSource,
        submitter::MockAnSubmitter,
        types::DepositEvent,
    };

    type R = Relayer<InMemoryDepositSource, MockProofGenerator, MockAnSubmitter>;

    fn deposit(id: u64) -> DepositEvent {
        DepositEvent {
            deposit_id: id,
            sender: Address::repeat_byte(0x11),
            amount: U256::from(id * 100 + 1),
            an_workchain: 0,
            an_account: B256::repeat_byte(0x33),
            timestamp: U256::ZERO,
            tx_hash: B256::repeat_byte(0xaa),
            log_index: 0,
            block_number: 100 + id,
            block_hash: B256::repeat_byte(0xcd),
            source_contract: Address::repeat_byte(0x22),
        }
    }

    fn make_relayer(
        source: Arc<InMemoryDepositSource>,
        submitter: Arc<MockAnSubmitter>,
        state_path: PathBuf,
    ) -> R {
        let cfg = RelayerConfig {
            state_path,
            start_deposit_id: 0,
            poll_interval: Duration::from_millis(0),
            max_attempts_warn: 16,
        };
        Relayer::new(cfg, source, Arc::new(MockProofGenerator::new()), submitter).unwrap()
    }

    fn fast_backoff() -> BackoffConfig {
        BackoffConfig {
            initial: Duration::from_millis(10),
            max: Duration::from_millis(80),
            multiplier: 2,
        }
    }

    #[test]
    fn backoff_bump_caps_at_max() {
        let b = BackoffConfig {
            initial: Duration::from_millis(10),
            max: Duration::from_millis(50),
            multiplier: 3,
        };
        let mut d = b.initial;
        d = b.bump(d);
        assert_eq!(d, Duration::from_millis(30));
        d = b.bump(d);
        assert_eq!(d, b.max);
        d = b.bump(d);
        assert_eq!(d, b.max);
    }

    #[tokio::test(start_paused = true)]
    async fn daemon_exits_on_pre_resolved_shutdown_after_one_tick() {
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new()); // empty
        let submitter = Arc::new(MockAnSubmitter::accepting());
        let mut relayer = make_relayer(source, submitter, dir.path().join("state.json"));
        let metrics = RelayerMetrics::new();

        let (tx, rx) = oneshot::channel::<()>();
        tx.send(()).unwrap();
        let shutdown = async move {
            let _ = rx.await;
        };

        let summary = relayer
            .run_until_shutdown(fast_backoff(), Some(metrics.clone()), shutdown)
            .await
            .unwrap();

        assert_eq!(summary.ticks, 1);
        assert_eq!(summary.not_yet_available, 1);
        assert_eq!(metrics.snapshot().not_yet_available, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn daemon_finalizes_5_then_shuts_down() {
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new());
        for id in 0..5 {
            source.insert(deposit(id));
        }
        let submitter = Arc::new(MockAnSubmitter::accepting());
        let mut relayer = make_relayer(source, submitter, dir.path().join("state.json"));
        let metrics = RelayerMetrics::new();
        let metrics_for_check = metrics.clone();

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(1)).await;
                if metrics_for_check.finalized_total.load(Ordering::Relaxed) >= 5 {
                    let _ = shutdown_tx.send(());
                    break;
                }
            }
        });

        let summary = relayer
            .run_until_shutdown(fast_backoff(), Some(metrics.clone()), async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();

        assert!(summary.finalized >= 5, "{summary:?}");
        assert_eq!(summary.an_rejected, 0);
        assert_eq!(summary.tick_errors, 0);
        assert!(metrics.snapshot().last_finalized_deposit_id >= 4);
    }

    #[tokio::test(start_paused = true)]
    async fn daemon_backoff_grows_on_consecutive_not_available() {
        let dir = tempdir().unwrap();
        let source = Arc::new(InMemoryDepositSource::new()); // empty
        let submitter = Arc::new(MockAnSubmitter::accepting());
        let mut relayer = make_relayer(source, submitter, dir.path().join("state.json"));
        let metrics = RelayerMetrics::new();
        let metrics_for_check = metrics.clone();

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(1)).await;
                if metrics_for_check.ticks_total.load(Ordering::Relaxed) >= 4 {
                    let _ = shutdown_tx.send(());
                    break;
                }
            }
        });

        let _ = relayer
            .run_until_shutdown(fast_backoff(), Some(metrics.clone()), async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();

        let snap = metrics.snapshot();
        assert!(snap.not_yet_available >= 4);
        assert_eq!(snap.finalized, 0);
    }
}
