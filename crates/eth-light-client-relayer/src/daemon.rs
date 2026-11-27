//! Long-running loop with exponential backoff and SIGINT-aware shutdown.
//!
//! The beacon watcher runs in a task of its own beside the loop, at its own
//! pace: the backoff, proving and sending never delay taking a checkpoint.

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
    error::RelayerError,
    metrics as prom,
    prover::ProofGenerator,
    relayer::{Relayer, TickOutcome},
    source::BeaconSource,
    submitter::AnSubmitter,
};

#[derive(Clone, Copy, Debug)]
pub struct BackoffConfig {
    pub initial: Duration,
    pub max: Duration,
    pub multiplier: u32,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(8),
            max: Duration::from_secs(300),
            multiplier: 2,
        }
    }
}

impl BackoffConfig {
    pub fn validate(&self) -> Result<(), RelayerError> {
        if self.initial.is_zero() {
            return Err(RelayerError::other("backoff initial must be > 0"));
        }
        if self.multiplier == 0 {
            return Err(RelayerError::other("backoff multiplier must be >= 1"));
        }
        Ok(())
    }

    fn bump(&self, current: Duration) -> Duration {
        let next = current.saturating_mul(self.multiplier);
        if next > self.max {
            self.max
        } else {
            next
        }
    }
}

/// The daemon's own tally of its rounds, logged when it stops. `/metrics`
/// carries the same counts as `eth_lc_relayer_ticks_total` by outcome
/// ([`crate::metrics`]).
#[derive(Debug, Default)]
pub struct RelayerMetrics {
    pub ticks_total: AtomicU64,
    pub submitted_total: AtomicU64,
    pub unchanged_total: AtomicU64,
    pub rotate_required_total: AtomicU64,
    pub proof_failed_total: AtomicU64,
    pub an_rejected_total: AtomicU64,
}

impl RelayerMetrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

fn is_success(o: &TickOutcome) -> bool {
    matches!(
        o,
        TickOutcome::SubmittedUpdate { .. }
            | TickOutcome::AlreadyOnHead { .. }
            | TickOutcome::Unchanged { .. }
            | TickOutcome::Proving { .. }
            | TickOutcome::Rotated { .. }
    )
}

impl<S: BeaconSource + 'static, P: ProofGenerator + 'static, A: AnSubmitter> Relayer<S, P, A> {
    pub async fn run_until_shutdown<F>(
        &mut self,
        backoff: BackoffConfig,
        metrics: Arc<RelayerMetrics>,
        shutdown: F,
    ) -> Result<(), RelayerError>
    where
        F: Future<Output = ()> + Send,
    {
        backoff.validate()?;
        tokio::pin!(shutdown);
        let mut delay = backoff.initial;
        // Without the watcher no checkpoint is taken while every round still
        // looks fine; the daemon stops instead, and its supervisor restarts it.
        let mut watcher = self.spawn_watcher();
        // What the state file and the queue on disk hold, before the first
        // round ends.
        self.publish_gauges();
        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    info!("shutdown signal; exiting after last tick");
                    return Ok(());
                }
                why = watcher.stopped() => return Err(watcher_stopped(why)),
                outcome = self.work() => {
                    metrics.ticks_total.fetch_add(1, Ordering::Relaxed);
                    prom::record_tick(
                        outcome_label(&outcome),
                        outcome.as_ref().is_ok_and(is_success),
                    );
                    match outcome {
                        Ok(o) => {
                            record(&metrics, &o);
                            if is_success(&o) {
                                delay = backoff.initial;
                            } else {
                                delay = backoff.bump(delay);
                                warn!(?o, secs = delay.as_secs(), "tick failed; backing off");
                            }
                        }
                        Err(e) => {
                            delay = backoff.bump(delay);
                            warn!(error = %e, secs = delay.as_secs(), "tick error; backing off");
                        }
                    }
                }
            }
            tokio::select! {
                _ = &mut shutdown => return Ok(()),
                why = watcher.stopped() => return Err(watcher_stopped(why)),
                _ = tokio::time::sleep(delay) => {}
            }
        }
    }
}

fn watcher_stopped(why: String) -> RelayerError {
    error!(why, "checkpoint watcher stopped; the daemon exits");
    RelayerError::other(format!("checkpoint watcher stopped: {why}"))
}

/// The `outcome` label of `eth_lc_relayer_ticks_total` for one round.
fn outcome_label(outcome: &Result<TickOutcome, RelayerError>) -> &'static str {
    match outcome {
        Ok(TickOutcome::SubmittedUpdate {
            ..
        }) => "submitted",
        Ok(TickOutcome::Rotated {
            ..
        }) => "rotated",
        Ok(TickOutcome::AlreadyOnHead {
            ..
        }) => "already_on_head",
        Ok(TickOutcome::Unchanged {
            ..
        }) => "unchanged",
        Ok(TickOutcome::Proving {
            ..
        }) => "proving",
        Ok(TickOutcome::RotateRequired {
            ..
        }) => "rotate_required",
        Ok(TickOutcome::ProofFailed {
            ..
        }) => "proof_failed",
        Ok(TickOutcome::AnRejected {
            ..
        }) => "an_rejected",
        Ok(TickOutcome::AnPending {
            ..
        }) => "an_pending",
        Ok(TickOutcome::WeakSubjectivityLag {
            ..
        }) => "weak_subjectivity_lag",
        Err(_) => "error",
    }
}

fn record(m: &RelayerMetrics, o: &TickOutcome) {
    match o {
        TickOutcome::SubmittedUpdate {
            ..
        }
        | TickOutcome::Rotated {
            ..
        } => {
            m.submitted_total.fetch_add(1, Ordering::Relaxed);
        },
        TickOutcome::Unchanged {
            ..
        }
        | TickOutcome::AlreadyOnHead {
            ..
        } => {
            m.unchanged_total.fetch_add(1, Ordering::Relaxed);
        },
        TickOutcome::RotateRequired {
            ..
        } => {
            m.rotate_required_total.fetch_add(1, Ordering::Relaxed);
        },
        TickOutcome::ProofFailed {
            ..
        } => {
            m.proof_failed_total.fetch_add(1, Ordering::Relaxed);
        },
        TickOutcome::AnRejected {
            ..
        }
        | TickOutcome::AnPending {
            ..
        } => {
            m.an_rejected_total.fetch_add(1, Ordering::Relaxed);
        },
        TickOutcome::WeakSubjectivityLag {
            ..
        }
        | TickOutcome::Proving {
            ..
        } => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_rejects_zero() {
        let mut b = BackoffConfig {
            multiplier: 0,
            ..Default::default()
        };
        assert!(b.validate().is_err());
        b.multiplier = 2;
        b.initial = Duration::ZERO;
        assert!(b.validate().is_err());
    }

    /// `/metrics` counts the rounds by outcome; the tally the daemon keeps in
    /// memory groups the same outcomes, as the table in `metrics.rs` says.
    #[test]
    fn the_tally_groups_the_outcomes_the_metrics_count() {
        let slot = 96;
        let reason = || "why".to_string();
        let outcomes = [
            (
                TickOutcome::SubmittedUpdate {
                    finalized_slot: slot,
                    tx_hash: None,
                },
                "submitted",
                "submitted_total",
                true,
            ),
            (
                TickOutcome::Rotated {
                    period: 2,
                    tx_hash: None,
                },
                "rotated",
                "submitted_total",
                true,
            ),
            (
                TickOutcome::AlreadyOnHead {
                    finalized_slot: slot,
                },
                "already_on_head",
                "unchanged_total",
                true,
            ),
            (
                TickOutcome::Unchanged {
                    finalized_slot: slot,
                },
                "unchanged",
                "unchanged_total",
                true,
            ),
            (
                TickOutcome::Proving {
                    finalized_slot: slot,
                    queued: 1,
                },
                "proving",
                "",
                true,
            ),
            (
                TickOutcome::RotateRequired {
                    from_period: 1,
                    to_period: 2,
                },
                "rotate_required",
                "rotate_required_total",
                false,
            ),
            (
                TickOutcome::ProofFailed {
                    reason: reason(),
                },
                "proof_failed",
                "proof_failed_total",
                false,
            ),
            (
                TickOutcome::AnRejected {
                    reason: reason(),
                },
                "an_rejected",
                "an_rejected_total",
                false,
            ),
            (
                TickOutcome::AnPending {
                    reason: reason(),
                },
                "an_pending",
                "an_rejected_total",
                false,
            ),
            (
                TickOutcome::WeakSubjectivityLag {
                    lag_slots: 9000,
                    finalized_slot: slot,
                },
                "weak_subjectivity_lag",
                "",
                false,
            ),
        ];
        // Every outcome there is, in the order `/metrics` declares them.
        let labels: Vec<&str> = outcomes
            .iter()
            .map(|(_, label, ..)| *label)
            .chain(["error"])
            .collect();
        assert_eq!(labels, prom::TICK_OUTCOMES);
        for (outcome, label, tallied, success) in outcomes {
            let m = RelayerMetrics::default();
            record(&m, &outcome);
            let tally = [
                ("submitted_total", &m.submitted_total),
                ("unchanged_total", &m.unchanged_total),
                ("rotate_required_total", &m.rotate_required_total),
                ("proof_failed_total", &m.proof_failed_total),
                ("an_rejected_total", &m.an_rejected_total),
            ];
            for (name, counter) in tally {
                assert_eq!(
                    counter.load(Ordering::Relaxed),
                    u64::from(name == tallied),
                    "{label} in {name}"
                );
            }
            assert_eq!(is_success(&outcome), success, "{label}");
            assert_eq!(outcome_label(&Ok(outcome)), label);
        }
        assert_eq!(outcome_label(&Err(RelayerError::other("io"))), "error");
    }

    #[test]
    fn bump_caps() {
        let b = BackoffConfig {
            initial: Duration::from_secs(2),
            max: Duration::from_secs(10),
            multiplier: 2,
        };
        assert_eq!(b.bump(Duration::from_secs(8)), Duration::from_secs(10));
    }
}
