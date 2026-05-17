//! Relayer-side wrapper around `acki-nacki-interface::BkSetTracker`.
//!
//! The tracker (lower layer) knows *how* to fetch and diff snapshots
//! from `/v2/bk_set_update`. The sentry (this module) knows *what the
//! relayer should do* with that information: classify each poll into a
//! [`SentryStatus`], keep aggregated counters for tracing / metrics,
//! and convert the underlying `AckiNackiError` into [`RelayerError`].
//!
//! ## Phase 5.2 wiring
//!
//! The relayer's main loop polls the bridge anchor every ~12 s. BK
//! rotations on AN happen orders of magnitude less frequently
//! (`epoch_finish_seq_no − seq_no` is in the hundreds of thousands of
//! blocks). The sentry is therefore designed to be **cheap and
//! independent** — a separate cadence (e.g. one tick per minute) is
//! enough, and the sentry's events are consumed alongside the main
//! relayer tick.
//!
//! ```text
//! ┌────────────────────────┐    ┌───────────────────────────────┐
//! │ BkSetTracker (lib)     │◀── │ BkSetSentry::tick()           │
//! │  HTTP poll + diff      │    │  1. tracker.poll()            │
//! └────────────────────────┘    │  2. classify → SentryStatus   │
//!                               │  3. bump counters / log       │
//!                               └────────────────┬──────────────┘
//!                                                │
//!                                                ▼
//!                              ┌────────────────────────────────┐
//!                              │ Relayer (Phase 5.2):            │
//!                              │  • Quiet → keep verifyBlock     │
//!                              │  • Rotation → trigger Circuit 3 │
//!                              └────────────────────────────────┘
//! ```
//!
//! ## Testability
//!
//! `BkSetSentry` is generic over the [`BkSetPoller`] trait so unit
//! tests can drive it with a `StubBkSetPoller` that returns canned
//! `BkSetChange`s without spinning up HTTP. The production constructor
//! [`BkSetSentry::from_node_url`] returns a sentry parameterised over
//! `acki_nacki_interface::BkSetTracker`, which talks to a real node.

use std::time::Duration;

use acki_nacki_interface::{
    BkSetChange, BkSetClient, BkSetSnapshot, BkSetTracker, MembershipDelta,
};
use async_trait::async_trait;
use tracing::{info, warn};

use crate::error::RelayerError;

/// Single observation outcome surfaced by [`BkSetSentry::tick`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SentryStatus {
    /// First poll since the sentry was constructed. Caller should warm
    /// up any cached state (e.g. record the observed commitment offline)
    /// but does not need to trigger any on-chain action.
    Bootstrapped {
        observed_seq_no: u64,
        bk_count: usize,
    },
    /// Membership is unchanged versus the previous poll. The relayer's
    /// happy path continues — submit `verifyBlock` proofs as usual.
    Quiet {
        observed_seq_no: u64,
        /// Set when the look-ahead `future` window changed even though
        /// the active committee didn't. Surfaced for observability;
        /// the relayer does not need to act.
        future_changed: bool,
    },
    /// Active committee rotated. The relayer must:
    ///
    /// 1. Stop submitting `verifyBlock` proofs that were generated against the
    ///    previous committee (the bridge will revert).
    /// 2. Trigger Circuit 3 rotation proof generation against the new snapshot.
    /// 3. Submit the rotation proof to the bridge.
    ///
    /// Phase 5.2 will wire the consumer side of this event; until then
    /// the relayer logs it and refuses to submit further blocks (a
    /// follow-up commit will add the corresponding guard).
    RotationDetected {
        old_seq_no: u64,
        new_seq_no: u64,
        delta: MembershipDelta,
    },
}

impl SentryStatus {
    /// Convenience predicate: "did this tick reveal a rotation the
    /// relayer must handle?"
    pub fn is_rotation(&self) -> bool {
        matches!(self, Self::RotationDetected { .. })
    }
}

/// Trait that lets the sentry be unit-tested without HTTP.
///
/// `BkSetTracker` already exposes this surface; the trait is a thin
/// adapter so we can substitute a stub in tests. `Send + Sync` to
/// keep the sentry trivially compatible with `tokio::spawn`.
#[async_trait]
pub trait BkSetPoller: Send + Sync {
    async fn poll(&mut self) -> Result<BkSetChange, acki_nacki_interface::AckiNackiError>;
    fn latest(&self) -> Option<&BkSetSnapshot>;
    #[allow(dead_code)]
    fn reset(&mut self);
}

#[async_trait]
impl BkSetPoller for BkSetTracker {
    async fn poll(&mut self) -> Result<BkSetChange, acki_nacki_interface::AckiNackiError> {
        BkSetTracker::poll(self).await
    }
    fn latest(&self) -> Option<&BkSetSnapshot> {
        BkSetTracker::latest(self)
    }
    fn reset(&mut self) {
        BkSetTracker::reset(self)
    }
}

/// Aggregated counters carried alongside the poller. Cheap to read
/// from any thread (it lives behind `&self` once the sentry is owned).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SentryMetrics {
    pub total_ticks: u64,
    pub successful_ticks: u64,
    pub rotations_observed: u64,
    pub last_observed_seq_no: u64,
}

/// Relayer-side BK-set sentry.
///
/// Cheap to construct; holds a poller (typically a `BkSetTracker`) and
/// rolling counters. Not internally `Clone` because the embedded
/// poller carries mutable cache state — share via `Arc<Mutex<_>>` if
/// multiple tasks need to consume `tick`.
pub struct BkSetSentry<P: BkSetPoller> {
    poller: P,
    metrics: SentryMetrics,
}

impl BkSetSentry<BkSetTracker> {
    /// Build a sentry backed by a real `BkSetTracker` against the given
    /// AN node base URL (e.g. `http://94.156.178.19:8600`). Returns
    /// `RelayerError::AckiNacki` if the underlying reqwest client
    /// can't be constructed.
    pub fn from_node_url(node_url: impl Into<String>) -> Result<Self, RelayerError> {
        let client = BkSetClient::new(node_url)?;
        Ok(Self {
            poller: BkSetTracker::new(client),
            metrics: SentryMetrics::default(),
        })
    }
}

impl<P: BkSetPoller> BkSetSentry<P> {
    /// Build a sentry around an arbitrary poller. Intended for tests;
    /// production code should call [`BkSetSentry::from_node_url`].
    pub fn from_poller(poller: P) -> Self {
        Self {
            poller,
            metrics: SentryMetrics::default(),
        }
    }

    /// Latest cached snapshot, if any.
    pub fn latest(&self) -> Option<&BkSetSnapshot> {
        self.poller.latest()
    }

    /// A copy of the running metrics (cheap — counters are scalar).
    pub fn metrics(&self) -> SentryMetrics {
        self.metrics
    }

    /// Take a single observation. Failure leaves the metrics unchanged
    /// for `successful_ticks` / `rotations_observed` but still bumps
    /// `total_ticks` so the operator can spot a stuck sentry from the
    /// outside.
    pub async fn tick(&mut self) -> Result<SentryStatus, RelayerError> {
        self.metrics.total_ticks = self.metrics.total_ticks.saturating_add(1);
        let change = self.poller.poll().await.map_err(RelayerError::from)?;
        let status = classify(change);
        self.metrics.successful_ticks = self.metrics.successful_ticks.saturating_add(1);
        match &status {
            SentryStatus::Bootstrapped {
                observed_seq_no, ..
            } => {
                self.metrics.last_observed_seq_no = *observed_seq_no;
            },
            SentryStatus::Quiet {
                observed_seq_no, ..
            } => {
                self.metrics.last_observed_seq_no = *observed_seq_no;
            },
            SentryStatus::RotationDetected {
                new_seq_no,
                delta,
                old_seq_no,
            } => {
                self.metrics.last_observed_seq_no = *new_seq_no;
                self.metrics.rotations_observed = self.metrics.rotations_observed.saturating_add(1);
                if !delta.pubkey_mutations.is_empty() {
                    warn!(
                        old_seq_no,
                        new_seq_no,
                        mutated = delta.pubkey_mutations.len(),
                        "BkSetSentry: pubkey mutation on existing signer_index — investigate"
                    );
                }
            },
        }
        Ok(status)
    }

    /// Convenience: run `tick()` in a loop with `interval` between
    /// observations, until `should_stop` returns true. Returns the
    /// final metrics on graceful exit, or the first hard error.
    ///
    /// Intentionally simple — no per-tick callback, no internal
    /// channel. Phase 5.2 will own the orchestration policy and call
    /// `tick()` directly from the main relayer loop's branch.
    pub async fn run_until_stop<F>(
        &mut self,
        interval: Duration,
        mut should_stop: F,
    ) -> Result<SentryMetrics, RelayerError>
    where
        F: FnMut() -> bool + Send,
    {
        loop {
            if should_stop() {
                return Ok(self.metrics);
            }
            match self.tick().await {
                Ok(SentryStatus::Bootstrapped {
                    observed_seq_no,
                    bk_count,
                }) => {
                    info!(observed_seq_no, bk_count, "BkSetSentry bootstrapped");
                },
                Ok(SentryStatus::Quiet {
                    observed_seq_no,
                    future_changed,
                }) => {
                    if future_changed {
                        info!(observed_seq_no, "BkSetSentry: future window changed");
                    }
                },
                Ok(SentryStatus::RotationDetected {
                    old_seq_no,
                    new_seq_no,
                    delta,
                }) => {
                    info!(
                        old_seq_no,
                        new_seq_no,
                        added = delta.added.len(),
                        removed = delta.removed.len(),
                        mutations = delta.pubkey_mutations.len(),
                        "BkSetSentry rotation observed"
                    );
                },
                Err(e) => return Err(e),
            }
            tokio::time::sleep(interval).await;
        }
    }
}

fn classify(change: BkSetChange) -> SentryStatus {
    match change {
        BkSetChange::FirstObservation {
            snapshot,
        } => SentryStatus::Bootstrapped {
            observed_seq_no: snapshot.observed_seq_no,
            bk_count: snapshot.current_size(),
        },
        BkSetChange::Unchanged {
            observed_seq_no,
            future_changed,
        } => SentryStatus::Quiet {
            observed_seq_no,
            future_changed,
        },
        BkSetChange::MembershipChanged {
            old_seq_no,
            new_seq_no,
            delta,
        } => SentryStatus::RotationDetected {
            old_seq_no,
            new_seq_no,
            delta,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use acki_nacki_interface::{AckiNackiError, BLS_PUBKEY_LEN};

    use super::*;

    /// Returns canned BkSetChange events on each `poll()`; once
    /// exhausted, returns the `then` error. Mirrors the same trait
    /// surface as a real tracker.
    struct StubPoller {
        events: VecDeque<Result<BkSetChange, AckiNackiError>>,
        last_snapshot: Option<BkSetSnapshot>,
    }

    impl StubPoller {
        fn new(events: Vec<Result<BkSetChange, AckiNackiError>>) -> Self {
            Self {
                events: events.into(),
                last_snapshot: None,
            }
        }
    }

    #[async_trait]
    impl BkSetPoller for StubPoller {
        async fn poll(&mut self) -> Result<BkSetChange, AckiNackiError> {
            let next = self.events.pop_front().expect("stub exhausted");
            if let Ok(BkSetChange::FirstObservation {
                snapshot,
            }) = &next
            {
                self.last_snapshot = Some(snapshot.clone());
            }
            next
        }

        fn latest(&self) -> Option<&BkSetSnapshot> {
            self.last_snapshot.as_ref()
        }

        fn reset(&mut self) {
            self.last_snapshot = None;
        }
    }

    fn make_snapshot(seq_no: u64) -> BkSetSnapshot {
        use std::collections::BTreeMap;
        let mut current = BTreeMap::new();
        current.insert(1, [0x01u8; BLS_PUBKEY_LEN]);
        current.insert(2, [0x02u8; BLS_PUBKEY_LEN]);
        BkSetSnapshot {
            observed_seq_no: seq_no,
            current,
            future: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn first_tick_emits_bootstrapped() {
        let snap = make_snapshot(100);
        let stub = StubPoller::new(vec![Ok(BkSetChange::FirstObservation {
            snapshot: snap.clone(),
        })]);
        let mut sentry = BkSetSentry::from_poller(stub);
        let status = sentry.tick().await.unwrap();
        match status {
            SentryStatus::Bootstrapped {
                observed_seq_no,
                bk_count,
            } => {
                assert_eq!(observed_seq_no, 100);
                assert_eq!(bk_count, 2);
            },
            other => panic!("expected Bootstrapped, got {other:?}"),
        }
        let m = sentry.metrics();
        assert_eq!(m.total_ticks, 1);
        assert_eq!(m.successful_ticks, 1);
        assert_eq!(m.last_observed_seq_no, 100);
        assert_eq!(m.rotations_observed, 0);
    }

    #[tokio::test]
    async fn quiet_tick_does_not_bump_rotation_counter() {
        let stub = StubPoller::new(vec![
            Ok(BkSetChange::FirstObservation {
                snapshot: make_snapshot(100),
            }),
            Ok(BkSetChange::Unchanged {
                observed_seq_no: 105,
                future_changed: false,
            }),
            Ok(BkSetChange::Unchanged {
                observed_seq_no: 110,
                future_changed: true,
            }),
        ]);
        let mut sentry = BkSetSentry::from_poller(stub);
        assert!(matches!(
            sentry.tick().await.unwrap(),
            SentryStatus::Bootstrapped { .. }
        ));
        let s1 = sentry.tick().await.unwrap();
        assert!(matches!(s1, SentryStatus::Quiet {
            observed_seq_no: 105,
            future_changed: false,
        }));
        let s2 = sentry.tick().await.unwrap();
        assert!(matches!(s2, SentryStatus::Quiet {
            observed_seq_no: 110,
            future_changed: true,
        }));
        let m = sentry.metrics();
        assert_eq!(m.rotations_observed, 0);
        assert_eq!(m.last_observed_seq_no, 110);
        assert_eq!(m.total_ticks, 3);
        assert_eq!(m.successful_ticks, 3);
    }

    #[tokio::test]
    async fn rotation_tick_bumps_rotation_counter() {
        use std::collections::BTreeSet;
        let delta = MembershipDelta {
            added: BTreeSet::from([5]),
            removed: BTreeSet::from([2]),
            pubkey_mutations: BTreeSet::new(),
        };
        let stub = StubPoller::new(vec![
            Ok(BkSetChange::FirstObservation {
                snapshot: make_snapshot(100),
            }),
            Ok(BkSetChange::MembershipChanged {
                old_seq_no: 100,
                new_seq_no: 200,
                delta: delta.clone(),
            }),
        ]);
        let mut sentry = BkSetSentry::from_poller(stub);
        let _ = sentry.tick().await.unwrap();
        let s = sentry.tick().await.unwrap();
        assert!(s.is_rotation());
        match &s {
            SentryStatus::RotationDetected {
                old_seq_no,
                new_seq_no,
                delta: d,
            } => {
                assert_eq!(*old_seq_no, 100);
                assert_eq!(*new_seq_no, 200);
                assert_eq!(d, &delta);
            },
            other => panic!("expected RotationDetected, got {other:?}"),
        }
        let m = sentry.metrics();
        assert_eq!(m.rotations_observed, 1);
        assert_eq!(m.last_observed_seq_no, 200);
    }

    #[tokio::test]
    async fn poll_error_bumps_total_but_not_successful() {
        let stub = StubPoller::new(vec![Err(AckiNackiError::Http(
            "node unreachable".to_string(),
        ))]);
        let mut sentry = BkSetSentry::from_poller(stub);
        let err = sentry.tick().await.unwrap_err();
        match err {
            RelayerError::AckiNacki(s) => assert!(s.contains("node unreachable")),
            other => panic!("expected RelayerError::AckiNacki, got {other:?}"),
        }
        let m = sentry.metrics();
        assert_eq!(m.total_ticks, 1);
        assert_eq!(m.successful_ticks, 0);
        assert_eq!(m.last_observed_seq_no, 0);
    }

    #[tokio::test]
    async fn run_until_stop_drives_until_predicate_flips() {
        // Stub holds exactly the number of events we expect to consume,
        // and `should_stop` returns true after the second invocation
        // (i.e. after two `tick()` calls). The third invocation would
        // crash the stub if it happened — proving the loop respects
        // the predicate.
        let stub = StubPoller::new(vec![
            Ok(BkSetChange::FirstObservation {
                snapshot: make_snapshot(10),
            }),
            Ok(BkSetChange::Unchanged {
                observed_seq_no: 11,
                future_changed: false,
            }),
        ]);
        let mut sentry = BkSetSentry::from_poller(stub);
        let mut invocations = 0u64;
        let metrics = sentry
            .run_until_stop(Duration::from_millis(0), move || {
                let stop = invocations >= 2;
                invocations += 1;
                stop
            })
            .await
            .unwrap();
        assert_eq!(metrics.total_ticks, 2);
        assert_eq!(metrics.successful_ticks, 2);
        assert_eq!(metrics.last_observed_seq_no, 11);
    }

    #[tokio::test]
    async fn run_until_stop_propagates_first_hard_error() {
        let stub = StubPoller::new(vec![
            Ok(BkSetChange::FirstObservation {
                snapshot: make_snapshot(10),
            }),
            Err(AckiNackiError::Http("transient".into())),
        ]);
        let mut sentry = BkSetSentry::from_poller(stub);
        let err = sentry
            .run_until_stop(Duration::from_millis(0), || false)
            .await
            .unwrap_err();
        assert!(matches!(err, RelayerError::AckiNacki(_)));
        let m = sentry.metrics();
        assert_eq!(m.total_ticks, 2);
        assert_eq!(m.successful_ticks, 1);
    }
}
