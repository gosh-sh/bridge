//! Rotation-aware composition of [`Relayer`] + [`BkSetSentry`].
//!
//! `SentryGuardedRelayer::tick()` is "sentry first, relayer second":
//!
//! 1. Ask the sentry for its latest read of the AN node's BK set.
//! 2. If the sentry reports a `RotationDetected`, mark the guard `paused` and
//!    refuse to invoke the inner relayer — submitting `verifyBlock` against the
//!    new committee would just be rejected on-chain (`BkSetCommitmentMismatch`
//!    until `storedBkSetCommitment` is advanced via a Circuit 3 rotation
//!    proof).
//! 3. Otherwise (Bootstrapped or Quiet) let the inner relayer perform its
//!    normal tick.
//!
//! Why a wrapper (composition) rather than a hook on `Relayer`?
//! Strictly additive: `Relayer<S, B>` stays unchanged, existing tests
//! and CLI consumers don't need to know about the sentry. Operators
//! who want rotation-aware behaviour wrap their relayer; those who
//! don't, don't pay any cost.
//!
//! ## Resume semantics
//!
//! Once paused, the guard stays paused until the caller explicitly
//! invokes [`SentryGuardedRelayer::resume`]. That call should happen
//! **after** the Circuit 3 rotation proof has been submitted and the
//! bridge's `storedBkSetCommitment` has been updated. We deliberately
//! do not auto-resume on the next "Quiet" event from the sentry —
//! the sentry's internal cache has already advanced to the new
//! snapshot, so subsequent polls would silently report `Unchanged`
//! and the relayer would resume submitting `verifyBlock` proofs even
//! though the bridge hasn't been reconciled.
//!
//! Phase 5.2 will wire the rotation pipeline (Circuit 3 prover →
//! gnark → on-chain `rotateBkSet`) and call `resume()` from there.
//! Phase 5.1 lands the guard alone, with the pause-and-wait-for-
//! resume invariant covered by unit tests.

use tracing::{info, warn};

use crate::{
    bk_set_sentry::{BkSetPoller, BkSetSentry, SentryStatus},
    bridge::BridgeClient,
    error::RelayerError,
    relayer::{Relayer, TickOutcome},
    source::{BkUpdateSource, BlockSource},
};

/// What happened during a single [`SentryGuardedRelayer::tick`] call.
///
/// `Passthrough` variants carry the inner relayer's [`TickOutcome`]
/// so callers can react to verifier rejections etc. exactly as they
/// would without the guard.
#[derive(Debug)]
pub enum GuardedOutcome {
    /// Sentry just made its first observation (cold start). Inner
    /// relayer was invoked normally.
    SentryBootstrapped {
        observed_seq_no: u64,
        bk_count: usize,
        inner: TickOutcome,
    },
    /// Sentry reports the committee is unchanged. Inner relayer was
    /// invoked normally. The `future_changed` flag is forwarded for
    /// observability; the relayer takes no action on it.
    SentryQuiet {
        observed_seq_no: u64,
        future_changed: bool,
        inner: TickOutcome,
    },
    /// Sentry detected a committee rotation. The inner relayer was
    /// **NOT** invoked, and the guard is now paused. Phase 5.2 must
    /// react: generate a Circuit 3 rotation proof, submit
    /// `rotateBkSet`, then call [`SentryGuardedRelayer::resume`].
    RotationDetected {
        old_seq_no: u64,
        new_seq_no: u64,
        added: usize,
        removed: usize,
        pubkey_mutations: usize,
    },
    /// Guard is paused awaiting a `resume()` call. Inner relayer was
    /// NOT invoked. Cheap to call repeatedly (no HTTP, no on-chain
    /// reads).
    PausedAwaitingRotationReconcile,
}

impl GuardedOutcome {
    /// `true` for variants that pause the relayer (RotationDetected
    /// or PausedAwaitingRotationReconcile).
    pub fn is_paused(&self) -> bool {
        matches!(
            self,
            Self::RotationDetected { .. } | Self::PausedAwaitingRotationReconcile
        )
    }
}

/// Composition of an inner [`Relayer`] and a [`BkSetSentry`].
pub struct SentryGuardedRelayer<S: BlockSource, U: BkUpdateSource, B: BridgeClient, P: BkSetPoller>
{
    inner: Relayer<S, U, B>,
    sentry: BkSetSentry<P>,
    paused: bool,
}

impl<S: BlockSource, U: BkUpdateSource, B: BridgeClient, P: BkSetPoller>
    SentryGuardedRelayer<S, U, B, P>
{
    pub fn new(inner: Relayer<S, U, B>, sentry: BkSetSentry<P>) -> Self {
        Self {
            inner,
            sentry,
            paused: false,
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn inner(&self) -> &Relayer<S, U, B> {
        &self.inner
    }

    pub fn sentry(&self) -> &BkSetSentry<P> {
        &self.sentry
    }

    /// Clear the rotation-detected pause flag. Caller must invoke
    /// this only AFTER the on-chain `storedBkSetCommitment` has been
    /// reconciled with the new BK-set (i.e. a Circuit 3 rotation
    /// proof was submitted successfully). No internal cross-check
    /// here — the on-chain reconciliation lives in the rotation
    /// pipeline, not in the guard.
    pub fn resume(&mut self) {
        if self.paused {
            info!("SentryGuardedRelayer: resume() called — clearing rotation pause");
            self.paused = false;
        }
    }

    pub async fn tick(&mut self) -> Result<GuardedOutcome, RelayerError> {
        if self.paused {
            return Ok(GuardedOutcome::PausedAwaitingRotationReconcile);
        }
        let sentry_status = self.sentry.tick().await?;
        match sentry_status {
            SentryStatus::RotationDetected {
                old_seq_no,
                new_seq_no,
                delta,
            } => {
                self.paused = true;
                warn!(
                    old_seq_no,
                    new_seq_no,
                    added = delta.added.len(),
                    removed = delta.removed.len(),
                    mutations = delta.pubkey_mutations.len(),
                    "SentryGuardedRelayer: BK rotation detected — pausing verifyBlock until \
                     resume() is called",
                );
                Ok(GuardedOutcome::RotationDetected {
                    old_seq_no,
                    new_seq_no,
                    added: delta.added.len(),
                    removed: delta.removed.len(),
                    pubkey_mutations: delta.pubkey_mutations.len(),
                })
            },
            SentryStatus::Bootstrapped {
                observed_seq_no,
                bk_count,
            } => {
                let inner = self.inner.tick().await?;
                Ok(GuardedOutcome::SentryBootstrapped {
                    observed_seq_no,
                    bk_count,
                    inner,
                })
            },
            SentryStatus::Quiet {
                observed_seq_no,
                future_changed,
            } => {
                let inner = self.inner.tick().await?;
                Ok(GuardedOutcome::SentryQuiet {
                    observed_seq_no,
                    future_changed,
                    inner,
                })
            },
        }
    }
}

/// Convenience: split `SentryGuardedRelayer` back into its parts
/// (e.g. for shutdown handling that needs to flush each side).
impl<S: BlockSource, U: BkUpdateSource, B: BridgeClient, P: BkSetPoller>
    SentryGuardedRelayer<S, U, B, P>
{
    pub fn into_parts(self) -> (Relayer<S, U, B>, BkSetSentry<P>) {
        (self.inner, self.sentry)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet, VecDeque},
        path::PathBuf,
        sync::Arc,
        time::Duration,
    };

    use acki_nacki_interface::{
        AckiNackiError, BkSetChange, BkSetSnapshot, MembershipDelta, BLS_PUBKEY_LEN,
    };
    use alloy::primitives::{Bytes, U256};
    use async_trait::async_trait;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        bk_set_sentry::BkSetPoller,
        bridge::MockBridgeClient,
        relayer::{Relayer, RelayerConfig, TickOutcome},
        source::{EmptyBkUpdateSource, InMemoryBlockSource},
        types::{AnBlockData, FinalizationType, MAX_LAYER_HASHES},
    };

    const BK: u64 = 0xBE5E7;

    // ──── Stub poller (mirrors sentry's tests) ───────────────────

    struct StubPoller {
        events: VecDeque<Result<BkSetChange, AckiNackiError>>,
        last: Option<BkSetSnapshot>,
    }

    impl StubPoller {
        fn new(events: Vec<Result<BkSetChange, AckiNackiError>>) -> Self {
            Self {
                events: events.into(),
                last: None,
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
                self.last = Some(snapshot.clone());
            }
            next
        }
        fn latest(&self) -> Option<&BkSetSnapshot> {
            self.last.as_ref()
        }
        fn reset(&mut self) {
            self.last = None;
        }
    }

    fn make_snapshot(seq_no: u64) -> BkSetSnapshot {
        let mut current = BTreeMap::new();
        current.insert(1, [0x01u8; BLS_PUBKEY_LEN]);
        current.insert(2, [0x02u8; BLS_PUBKEY_LEN]);
        BkSetSnapshot {
            observed_seq_no: seq_no,
            current,
            future: BTreeMap::new(),
        }
    }

    fn rotation_delta() -> MembershipDelta {
        MembershipDelta {
            added: BTreeSet::from([5]),
            removed: BTreeSet::from([1]),
            pubkey_mutations: BTreeSet::new(),
        }
    }

    // ──── Helpers to build an inner relayer ──────────────────────

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

    fn inner_relayer(
        source: Arc<InMemoryBlockSource>,
        bridge: Arc<MockBridgeClient>,
        state_path: PathBuf,
    ) -> Relayer<InMemoryBlockSource, EmptyBkUpdateSource, MockBridgeClient> {
        Relayer::new(
            RelayerConfig {
                state_path,
                poll_interval: Duration::from_millis(0),
                max_attempts_warn: 16,
            },
            source,
            Arc::new(EmptyBkUpdateSource),
            bridge,
        )
        .unwrap()
    }

    fn fresh_bridge() -> Arc<MockBridgeClient> {
        Arc::new(MockBridgeClient::with_genesis(
            U256::from(BK),
            U256::ZERO,
            Arc::new(|_| true),
        ))
    }

    // ──── Tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn bootstrapped_then_quiet_passes_through_to_inner() {
        let dir = tempdir().unwrap();
        let bridge = fresh_bridge();
        let source = Arc::new(InMemoryBlockSource::new());
        source.insert(block(1, U256::ZERO));
        let mut anchor = block(1, U256::ZERO).next_anchor();
        source.insert(block(2, anchor));
        anchor = block(2, anchor).next_anchor();
        source.insert(block(3, anchor));

        let inner = inner_relayer(source, bridge.clone(), dir.path().join("state.json"));
        let stub = StubPoller::new(vec![
            Ok(BkSetChange::FirstObservation {
                snapshot: make_snapshot(100),
            }),
            Ok(BkSetChange::Unchanged {
                observed_seq_no: 101,
                future_changed: false,
            }),
            Ok(BkSetChange::Unchanged {
                observed_seq_no: 102,
                future_changed: true,
            }),
        ]);
        let mut guarded = SentryGuardedRelayer::new(inner, BkSetSentry::from_poller(stub));

        match guarded.tick().await.unwrap() {
            GuardedOutcome::SentryBootstrapped {
                observed_seq_no,
                bk_count,
                inner,
            } => {
                assert_eq!(observed_seq_no, 100);
                assert_eq!(bk_count, 2);
                assert!(matches!(inner, TickOutcome::Verified {
                    seq_no: 1,
                    ..
                }));
            },
            other => panic!("expected SentryBootstrapped, got {other:?}"),
        }

        match guarded.tick().await.unwrap() {
            GuardedOutcome::SentryQuiet {
                observed_seq_no,
                future_changed,
                inner,
            } => {
                assert_eq!(observed_seq_no, 101);
                assert!(!future_changed);
                assert!(matches!(inner, TickOutcome::Verified {
                    seq_no: 2,
                    ..
                }));
            },
            other => panic!("expected SentryQuiet, got {other:?}"),
        }

        match guarded.tick().await.unwrap() {
            GuardedOutcome::SentryQuiet {
                future_changed: true,
                inner,
                ..
            } => assert!(matches!(inner, TickOutcome::Verified {
                seq_no: 3,
                ..
            })),
            other => panic!("expected SentryQuiet future_changed, got {other:?}"),
        }

        assert_eq!(bridge.accepted_count(), 3);
        assert!(!guarded.is_paused());
    }

    #[tokio::test]
    async fn rotation_pauses_and_does_not_invoke_inner() {
        let dir = tempdir().unwrap();
        let bridge = fresh_bridge();
        let source = Arc::new(InMemoryBlockSource::new());
        source.insert(block(1, U256::ZERO));

        let inner = inner_relayer(source, bridge.clone(), dir.path().join("state.json"));
        let stub = StubPoller::new(vec![
            Ok(BkSetChange::FirstObservation {
                snapshot: make_snapshot(100),
            }),
            Ok(BkSetChange::MembershipChanged {
                old_seq_no: 100,
                new_seq_no: 200,
                delta: rotation_delta(),
            }),
        ]);
        let mut guarded = SentryGuardedRelayer::new(inner, BkSetSentry::from_poller(stub));

        // First tick: bootstrap → inner runs and accepts block 1.
        let first = guarded.tick().await.unwrap();
        assert!(matches!(first, GuardedOutcome::SentryBootstrapped { .. }));
        assert_eq!(bridge.accepted_count(), 1);

        // Second tick: rotation detected → inner NOT invoked, guard paused.
        let second = guarded.tick().await.unwrap();
        match second {
            GuardedOutcome::RotationDetected {
                old_seq_no,
                new_seq_no,
                added,
                removed,
                pubkey_mutations,
            } => {
                assert_eq!(old_seq_no, 100);
                assert_eq!(new_seq_no, 200);
                assert_eq!(added, 1);
                assert_eq!(removed, 1);
                assert_eq!(pubkey_mutations, 0);
            },
            other => panic!("expected RotationDetected, got {other:?}"),
        }
        assert!(guarded.is_paused());
        // Inner relayer state unchanged after the paused tick.
        assert_eq!(bridge.accepted_count(), 1);
        assert_eq!(guarded.inner().state().last_processed_seqno, Some(1));
    }

    #[tokio::test]
    async fn paused_state_is_cheap_and_independent_of_inner() {
        let dir = tempdir().unwrap();
        let bridge = fresh_bridge();
        let source = Arc::new(InMemoryBlockSource::new());
        // No blocks staged → inner would normally return NotYetAvailable.

        let inner = inner_relayer(source, bridge.clone(), dir.path().join("state.json"));
        let stub = StubPoller::new(vec![Ok(BkSetChange::MembershipChanged {
            old_seq_no: 0,
            new_seq_no: 10,
            delta: rotation_delta(),
        })]);
        let mut guarded = SentryGuardedRelayer::new(inner, BkSetSentry::from_poller(stub));

        assert!(matches!(
            guarded.tick().await.unwrap(),
            GuardedOutcome::RotationDetected { .. }
        ));
        // Now paused — subsequent ticks short-circuit BEFORE touching
        // the inner relayer. The stub has zero events left, so any
        // call into the sentry would panic. The fact that this passes
        // proves the pause short-circuits before the sentry too.
        for _ in 0..5 {
            let outcome = guarded.tick().await.unwrap();
            assert!(matches!(
                outcome,
                GuardedOutcome::PausedAwaitingRotationReconcile
            ));
        }
        assert!(guarded.is_paused());
    }

    #[tokio::test]
    async fn resume_after_rotation_unblocks_subsequent_ticks() {
        let dir = tempdir().unwrap();
        let bridge = fresh_bridge();
        let source = Arc::new(InMemoryBlockSource::new());
        source.insert(block(1, U256::ZERO));

        let inner = inner_relayer(source, bridge.clone(), dir.path().join("state.json"));
        // After Bootstrap+Rotation, give the stub a Quiet so a post-
        // resume tick has something to consume from the sentry.
        let stub = StubPoller::new(vec![
            Ok(BkSetChange::FirstObservation {
                snapshot: make_snapshot(100),
            }),
            Ok(BkSetChange::MembershipChanged {
                old_seq_no: 100,
                new_seq_no: 200,
                delta: rotation_delta(),
            }),
            Ok(BkSetChange::Unchanged {
                observed_seq_no: 250,
                future_changed: false,
            }),
        ]);
        let mut guarded = SentryGuardedRelayer::new(inner, BkSetSentry::from_poller(stub));

        let _ = guarded.tick().await.unwrap(); // Bootstrap → block 1 submitted
        let _ = guarded.tick().await.unwrap(); // Rotation → paused
        assert!(guarded.is_paused());

        guarded.resume();
        assert!(!guarded.is_paused());

        // Now we're unpaused; sentry's next event is Quiet, and the
        // source has no block 2 staged → inner returns NotYetAvailable.
        match guarded.tick().await.unwrap() {
            GuardedOutcome::SentryQuiet {
                observed_seq_no,
                inner:
                    TickOutcome::NotYetAvailable {
                        target_seq_no,
                    },
                ..
            } => {
                assert_eq!(observed_seq_no, 250);
                assert_eq!(target_seq_no, 2);
            },
            other => panic!("expected SentryQuiet + NotYetAvailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sentry_poll_error_propagates_and_does_not_pause() {
        let dir = tempdir().unwrap();
        let bridge = fresh_bridge();
        let source = Arc::new(InMemoryBlockSource::new());

        let inner = inner_relayer(source, bridge.clone(), dir.path().join("state.json"));
        let stub = StubPoller::new(vec![Err(AckiNackiError::Http("boom".into()))]);
        let mut guarded = SentryGuardedRelayer::new(inner, BkSetSentry::from_poller(stub));

        let err = guarded.tick().await.unwrap_err();
        assert!(matches!(err, RelayerError::AckiNacki(_)));
        assert!(
            !guarded.is_paused(),
            "transient sentry error must not pause"
        );
    }
}
