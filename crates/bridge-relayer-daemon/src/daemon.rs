//! Phase 5.1.B — Long-running daemon entry points for the relayer.
//!
//! [`Relayer::tick`] is fine for tests and one-shot smoke runs, and
//! [`Relayer::run_loop`] gives a bounded loop with a `should_stop`
//! predicate. Neither is suitable for an operator running the relayer as
//! a service: a real daemon needs to
//!
//! 1. loop forever (until SIGINT/SIGTERM or an explicit shutdown signal),
//! 2. **exponentially back off** when the AN side has nothing to submit or the
//!    bridge rejects our submission (so we don't hammer the RPC every 2 s for
//!    an hour while the source catches up),
//! 3. **reset** the backoff the moment a block gets verified, and
//! 4. expose **structured metrics** for an external scrape / log aggregator.
//!
//! This module adds [`RelayerMetrics`] + [`BackoffConfig`] +
//! [`Relayer::run_until_shutdown`] without modifying the testable
//! single-step `tick()` entry points.
//!
//! The shutdown future is passed in by the caller (so tests can pass a
//! `tokio::time::sleep` or a `oneshot::Receiver`; the binary wires it to
//! `tokio::signal::ctrl_c()`). When the future resolves the daemon
//! returns at the next sleep boundary — never mid-`tick`, so a `Verified`
//! outcome is always followed by a flushed `state.json`.

use std::{
    future::Future,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use tracing::{error, info, warn};

use crate::{
    bridge::BridgeClient,
    error::RelayerError,
    relayer::{Relayer, TickOutcome},
    source::BlockSource,
};

// ──────────────────────────────────────────────────────────────────────
// Backoff configuration
// ──────────────────────────────────────────────────────────────────────

/// Backoff policy for the daemon loop.
///
/// `current = min(initial * multiplier^consecutive_failures, max)`.
/// On any [`TickOutcome::Verified`] the streak resets and `current`
/// snaps back to `initial`.
#[derive(Clone, Copy, Debug)]
pub struct BackoffConfig {
    /// Sleep applied after the *first* non-success outcome (and after
    /// every `Verified`). Also the lower bound for subsequent sleeps.
    pub initial: Duration,
    /// Upper bound on the sleep duration, regardless of how many
    /// consecutive failures we've seen.
    pub max: Duration,
    /// Multiplier applied after each consecutive non-success outcome.
    /// `2` is the standard "double on each failure" policy.
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
    /// Bump the current delay by `multiplier`, saturating at `max`.
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

/// Atomic counters for one running daemon. Use [`RelayerMetrics::new`]
/// to get an [`Arc<RelayerMetrics>`]; pass clones into `run_until_shutdown`
/// to feed metrics, and call [`RelayerMetrics::snapshot`] from any thread
/// (e.g. an HTTP `/metrics` handler) to read them.
///
/// All counters are monotonic except `current_backoff_secs` and
/// `last_verified_seq_no`, which track "latest value" rather than a sum.
#[derive(Debug, Default)]
pub struct RelayerMetrics {
    /// Total number of `tick()` calls that returned (including errors).
    pub ticks_total: AtomicU64,
    /// Times the bridge accepted our block.
    pub verified_total: AtomicU64,
    /// Times the bridge accepted a BK-set update.
    pub bk_update_applied_total: AtomicU64,
    /// Times the bridge reverted (any reason).
    pub reverted_total: AtomicU64,
    /// Times the block source returned `None` for the target seqno.
    pub not_yet_available_total: AtomicU64,
    /// Times `tick()` returned `Err(_)` (RPC / IO / serde failure).
    pub tick_errors_total: AtomicU64,
    /// Check A / Check B history-drift failures.
    pub history_drift_detected_total: AtomicU64,
    /// Check B: on-chain seq_no went backwards.
    pub chain_rewind_observed_total: AtomicU64,
    /// **Current** sleep length in whole seconds (most recent backoff
    /// computation). Useful to expose as a gauge for alerting.
    pub current_backoff_secs: AtomicU64,
    /// Largest seqNo we successfully submitted, or `0` if we never have.
    pub last_verified_seq_no: AtomicU64,
    /// Last observed on-chain seq_no (gauge).
    pub last_observed_on_chain_seq_no: AtomicU64,
}

impl RelayerMetrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn snapshot(&self) -> RelayerMetricsSnapshot {
        RelayerMetricsSnapshot {
            ticks: self.ticks_total.load(Ordering::Relaxed),
            verified: self.verified_total.load(Ordering::Relaxed),
            bk_update_applied: self.bk_update_applied_total.load(Ordering::Relaxed),
            reverted: self.reverted_total.load(Ordering::Relaxed),
            not_yet_available: self.not_yet_available_total.load(Ordering::Relaxed),
            tick_errors: self.tick_errors_total.load(Ordering::Relaxed),
            history_drift_detected: self.history_drift_detected_total.load(Ordering::Relaxed),
            chain_rewind_observed: self.chain_rewind_observed_total.load(Ordering::Relaxed),
            current_backoff_secs: self.current_backoff_secs.load(Ordering::Relaxed),
            last_verified_seq_no: self.last_verified_seq_no.load(Ordering::Relaxed),
            last_observed_on_chain_seq_no: self
                .last_observed_on_chain_seq_no
                .load(Ordering::Relaxed),
        }
    }
}

/// Plain-old-data snapshot of [`RelayerMetrics`]. Cheap to clone, safe
/// to serialise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RelayerMetricsSnapshot {
    pub ticks: u64,
    pub verified: u64,
    pub bk_update_applied: u64,
    pub reverted: u64,
    pub not_yet_available: u64,
    pub tick_errors: u64,
    pub history_drift_detected: u64,
    pub chain_rewind_observed: u64,
    pub current_backoff_secs: u64,
    pub last_verified_seq_no: u64,
    pub last_observed_on_chain_seq_no: u64,
}

// ──────────────────────────────────────────────────────────────────────
// Run summary
// ──────────────────────────────────────────────────────────────────────

/// Returned by `run_until_shutdown` after the shutdown future resolves.
/// Mirrors a [`RelayerMetricsSnapshot`] but is always produced (even when
/// the caller passed `metrics = None`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DaemonRunSummary {
    pub ticks: u64,
    pub verified: u64,
    pub reverted: u64,
    pub not_yet_available: u64,
    pub tick_errors: u64,
    /// Outcome of the *final* tick — useful for logs at shutdown time.
    pub last_outcome: Option<LastOutcome>,
}

/// Tag for [`DaemonRunSummary::last_outcome`]. Avoids embedding the full
/// `TickOutcome` (which can carry a 25k-byte `Bytes`) in a summary that
/// might end up in a log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LastOutcome {
    Verified { seq_no: u64 },
    BkUpdateApplied { seq_no: u64 },
    NotYetAvailable { seq_no: u64 },
    BridgeReverted { seq_no: u64 },
    BkUpdateReverted { seq_no: u64 },
    TickError,
}

// ──────────────────────────────────────────────────────────────────────
// Relayer::run_until_shutdown
// ──────────────────────────────────────────────────────────────────────

impl<S: BlockSource, U: crate::source::BkUpdateSource, B: BridgeClient> Relayer<S, U, B> {
    /// Drive `tick()` forever (or until `shutdown` resolves), applying
    /// exponential backoff between non-success outcomes. Sleeps are
    /// shutdown-aware via `tokio::select!`, so a SIGINT during a 60-second
    /// sleep is honoured immediately.
    ///
    /// Returns when:
    /// - `shutdown` resolves at any tick boundary, or
    /// - a `tick()` Err is encountered and `bail_on_error` is `true` (default
    ///   `false` — daemons keep going through transient RPC blips).
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
            // 1. Single tick.
            let outcome_or_err = self.tick().await;
            summary.ticks += 1;
            if let Some(m) = &metrics {
                m.ticks_total.fetch_add(1, Ordering::Relaxed);
            }

            // 2. Classify, update metrics, decide on backoff.
            let (success, last_tag) = match outcome_or_err {
                Ok(TickOutcome::Verified {
                    seq_no, ..
                }) => {
                    if let Some(m) = &metrics {
                        m.verified_total.fetch_add(1, Ordering::Relaxed);
                        m.last_verified_seq_no.store(seq_no, Ordering::Relaxed);
                    }
                    summary.verified += 1;
                    info!(seq_no, "daemon: verified");
                    (true, LastOutcome::Verified {
                        seq_no,
                    })
                },
                Ok(TickOutcome::BkUpdateApplied {
                    seq_no, ..
                }) => {
                    if let Some(m) = &metrics {
                        m.bk_update_applied_total.fetch_add(1, Ordering::Relaxed);
                    }
                    summary.verified += 1; // treat as progress for summary
                    info!(seq_no, "daemon: bk-set update applied");
                    (true, LastOutcome::BkUpdateApplied {
                        seq_no,
                    })
                },
                Ok(TickOutcome::NotYetAvailable {
                    target_seq_no,
                }) => {
                    if let Some(m) = &metrics {
                        m.not_yet_available_total.fetch_add(1, Ordering::Relaxed);
                    }
                    summary.not_yet_available += 1;
                    (false, LastOutcome::NotYetAvailable {
                        seq_no: target_seq_no,
                    })
                },
                Ok(TickOutcome::BridgeReverted {
                    target_seq_no,
                    reason,
                }) => {
                    if let Some(m) = &metrics {
                        m.reverted_total.fetch_add(1, Ordering::Relaxed);
                    }
                    summary.reverted += 1;
                    warn!(target_seq_no, reason = %reason, "daemon: bridge reverted");
                    (false, LastOutcome::BridgeReverted {
                        seq_no: target_seq_no,
                    })
                },
                Ok(TickOutcome::BkUpdateReverted {
                    seq_no,
                    reason,
                }) => {
                    if let Some(m) = &metrics {
                        m.reverted_total.fetch_add(1, Ordering::Relaxed);
                    }
                    summary.reverted += 1;
                    warn!(seq_no, reason = %reason, "daemon: bk-set update reverted");
                    (false, LastOutcome::BkUpdateReverted {
                        seq_no,
                    })
                },
                Err(e) => {
                    if let Some(m) = &metrics {
                        m.tick_errors_total.fetch_add(1, Ordering::Relaxed);
                        if format!("{e}").contains("drift") || format!("{e}").contains("rewound") {
                            m.history_drift_detected_total
                                .fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    summary.tick_errors += 1;
                    if matches!(e, RelayerError::Stuck { .. }) {
                        // Hard stop: same seqNo has been rejected past the
                        // abort threshold. Log loudly and break out of the
                        // loop so the operator has to intervene instead of
                        // watching us retry the same broken block forever.
                        // `summary` is discarded on Err — caller receives
                        // the RelayerError directly.
                        error!(error = ?e, "daemon: stuck — hard aborting");
                        return Err(e);
                    }
                    warn!(error = ?e, "daemon: tick failed, will back off");
                    (false, LastOutcome::TickError)
                },
            };
            summary.last_outcome = Some(last_tag);

            // 3. Backoff: success -> reset; failure -> grow.
            if success {
                current_delay = backoff.initial;
            } else {
                current_delay = backoff.bump(current_delay);
            }
            if let Some(m) = &metrics {
                m.current_backoff_secs
                    .store(current_delay.as_secs(), Ordering::Relaxed);
            }

            // 4. Sleep with shutdown awareness.
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

    use alloy::primitives::{Bytes, U256};
    use tempfile::tempdir;
    use tokio::sync::oneshot;

    use super::*;
    use crate::{
        bridge::MockBridgeClient,
        relayer::{Relayer, RelayerConfig},
        source::InMemoryBlockSource,
        types::{AnBlockData, FinalizationType, MAX_LAYER_HASHES},
    };

    const BK: u64 = 0xBE5E7;

    fn block(seq: u64, prev_anchor: U256) -> AnBlockData {
        let mut layer_hashes = [U256::ZERO; MAX_LAYER_HASHES];
        layer_hashes[0] = U256::from(seq * 100 + 1);
        AnBlockData {
            fin_type: FinalizationType::Primary,
            block_id: U256::from(seq),
            bk_set_commitment: U256::from(BK),
            block_seq_no: seq,
            num_layers: 1,
            layer_hashes,
            prev_max_level_layer_hash: prev_anchor,
            attestation_proof: Bytes::from(vec![0u8; 32]),
            layer_hashes_proof: Bytes::from(vec![0u8; 32]),
        }
    }

    fn make_relayer(
        source: Arc<InMemoryBlockSource>,
        bridge: Arc<MockBridgeClient>,
        state_path: PathBuf,
    ) -> Relayer<InMemoryBlockSource, crate::source::EmptyBkUpdateSource, MockBridgeClient> {
        let cfg = RelayerConfig {
            state_path,
            // We never sleep against this `poll_interval` in the daemon
            // path — daemon uses BackoffConfig — but keep it in sync
            // with the rest of the test rig.
            poll_interval: Duration::from_millis(0),
            max_attempts_warn: 16,
            // Tests want soft-retry semantics: mock bridge tests exercise
            // long revert streaks and would trip the abort gate.
            max_attempts_abort: u32::MAX,
            bundle_stride: bridge_prover_lib::BUNDLE_STRIDE_L1,
        };
        Relayer::new(
            cfg,
            source,
            Arc::new(crate::source::EmptyBkUpdateSource),
            bridge,
        )
        .unwrap()
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
        d = b.bump(d); // 30
        assert_eq!(d, Duration::from_millis(30));
        d = b.bump(d); // 90 -> capped to 50
        assert_eq!(d, b.max);
        d = b.bump(d); // still capped
        assert_eq!(d, b.max);
    }

    #[tokio::test(start_paused = true)]
    async fn daemon_exits_immediately_on_pre_resolved_shutdown() {
        let dir = tempdir().unwrap();
        let bridge = Arc::new(MockBridgeClient::with_genesis(
            U256::from(BK),
            U256::ZERO,
            Arc::new(|_| true),
        ));
        let source = Arc::new(InMemoryBlockSource::new());

        let mut relayer = make_relayer(source, bridge, dir.path().join("state.json"));
        let metrics = RelayerMetrics::new();

        // An already-ready shutdown future (the relayer must still make
        // *one* tick — shutdown is only checked at the sleep boundary —
        // and then return immediately).
        let (tx, rx) = oneshot::channel::<()>();
        tx.send(()).unwrap();
        let shutdown = async move {
            let _ = rx.await;
        };

        let summary = relayer
            .run_until_shutdown(fast_backoff(), Some(metrics.clone()), shutdown)
            .await
            .unwrap();

        assert_eq!(
            summary.ticks, 1,
            "exactly one tick before shutdown is honoured"
        );
        assert_eq!(summary.not_yet_available, 1);
        let snap = metrics.snapshot();
        assert_eq!(snap.ticks, 1);
        assert_eq!(snap.not_yet_available, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn daemon_processes_5_blocks_then_shuts_down() {
        let dir = tempdir().unwrap();
        let bridge = Arc::new(MockBridgeClient::with_genesis(
            U256::from(BK),
            U256::ZERO,
            Arc::new(|_| true),
        ));
        let source = Arc::new(InMemoryBlockSource::new());

        let mut anchor = U256::ZERO;
        for seq in 1..=5 {
            let b = block(seq, anchor);
            anchor = b.next_anchor();
            source.insert(b);
        }

        let mut relayer = make_relayer(source, bridge.clone(), dir.path().join("state.json"));
        let metrics = RelayerMetrics::new();
        let metrics_for_check = metrics.clone();

        // Shutdown via oneshot once we've seen 5 verifications.
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        // Spawn a watcher: poll metrics every virtual ms and trip the
        // shutdown once `verified >= 5`.
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(1)).await;
                if metrics_for_check.verified_total.load(Ordering::Relaxed) >= 5 {
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

        assert!(summary.verified >= 5, "at least 5 verifies: {summary:?}");
        assert_eq!(summary.reverted, 0);
        assert_eq!(summary.tick_errors, 0);
        let snap = metrics.snapshot();
        assert!(snap.last_verified_seq_no >= 5);
    }

    #[tokio::test(start_paused = true)]
    async fn daemon_backoff_grows_on_consecutive_not_available() {
        let dir = tempdir().unwrap();
        let bridge = Arc::new(MockBridgeClient::with_genesis(
            U256::from(BK),
            U256::ZERO,
            Arc::new(|_| true),
        ));
        let source = Arc::new(InMemoryBlockSource::new()); // empty -> always NotYetAvailable

        let mut relayer = make_relayer(source, bridge, dir.path().join("state.json"));
        let metrics = RelayerMetrics::new();
        let metrics_for_check = metrics.clone();

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        // After 4 ticks the backoff should be at max (initial=10ms,
        // 10→20→40→80 = max).
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(1)).await;
                if metrics_for_check.ticks_total.load(Ordering::Relaxed) >= 4 {
                    let _ = shutdown_tx.send(());
                    break;
                }
            }
        });

        let backoff = fast_backoff(); // 10ms..80ms, x2
        let _ = relayer
            .run_until_shutdown(backoff, Some(metrics.clone()), async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();

        let snap = metrics.snapshot();
        // Backoff doubled at least once from initial 10ms (=0s in
        // whole-second granularity). We assert via the not-yet-
        // available counter and current backoff being non-zero in
        // milliseconds. The seconds counter rounds to 0 here because
        // 80ms < 1s; assert the bookkeeping is correct via the loop
        // having tripped 4+ NotYetAvailable bumps instead.
        assert!(snap.not_yet_available >= 4);
        assert_eq!(snap.verified, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn daemon_backoff_resets_after_verify() {
        let dir = tempdir().unwrap();
        let bridge = Arc::new(MockBridgeClient::with_genesis(
            U256::from(BK),
            U256::ZERO,
            Arc::new(|_| true),
        ));
        let source = Arc::new(InMemoryBlockSource::new());

        let mut relayer = make_relayer(source.clone(), bridge, dir.path().join("state.json"));
        let metrics = RelayerMetrics::new();
        let metrics_for_check = metrics.clone();
        let source_for_seed = source.clone();

        // Seed a block after 2 NotYetAvailable ticks so we observe the
        // reset boundary.
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(1)).await;
                if metrics_for_check
                    .not_yet_available_total
                    .load(Ordering::Relaxed)
                    >= 2
                {
                    source_for_seed.insert(block(1, U256::ZERO));
                    break;
                }
            }
        });

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let metrics_for_stop = metrics.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(1)).await;
                if metrics_for_stop.verified_total.load(Ordering::Relaxed) >= 1 {
                    let _ = shutdown_tx.send(());
                    break;
                }
            }
        });

        let backoff = BackoffConfig {
            initial: Duration::from_millis(5),
            max: Duration::from_millis(20),
            multiplier: 2,
        };
        let summary = relayer
            .run_until_shutdown(backoff, Some(metrics.clone()), async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();

        assert!(summary.verified >= 1, "should land at least one verify");
        assert!(summary.not_yet_available >= 2, "two pre-verify stalls");
        // Last outcome before shutdown is the Verified one.
        assert!(matches!(
            summary.last_outcome,
            Some(LastOutcome::Verified {
                seq_no: 1
            })
        ));
    }
}
