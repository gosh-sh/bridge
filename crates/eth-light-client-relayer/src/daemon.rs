//! Long-running loop with exponential backoff and SIGINT-aware shutdown.

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
            | TickOutcome::Rotated { .. }
    )
}

impl<S: BeaconSource, P: ProofGenerator, A: AnSubmitter> Relayer<S, P, A> {
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
        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    info!("shutdown signal; exiting after last tick");
                    return Ok(());
                }
                outcome = self.tick() => {
                    metrics.ticks_total.fetch_add(1, Ordering::Relaxed);
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
                _ = tokio::time::sleep(delay) => {}
            }
        }
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
