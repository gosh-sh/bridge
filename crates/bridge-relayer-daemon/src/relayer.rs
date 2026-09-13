//! Relayer main loop.
//!
//! [`Relayer::tick`] is the testable single-step entry point. It:
//!
//! 1. reads the on-chain anchors (+ Check B vs last observed);
//! 2. **Phase 1** — drains any pending BK-set update (`applyBkSetUpdate`);
//! 3. **Phase 2** — fetches the next block and submits `verifyBlock`;
//! 4. on success — acks the live driver (if any), runs Check A, persists state.
//!
//! [`Relayer::run_loop`] just calls `tick` in a `loop` with a configurable
//! delay between iterations and a "max ticks" budget for tests.

use std::{path::PathBuf, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use crate::{
    bridge::{BkSetUpdateSubmitOutcome, BridgeClient, BridgeOnChainState, SubmitOutcome},
    error::RelayerError,
    history_consistency::{check_chain_monotonicity, check_history_consistency},
    source::{BkUpdateSource, BlockSource},
    state::RelayerState,
};

/// Max seq_no gap another actor may advance between ticks before we halt
/// (Check B). Thinned bundles are W·P = 512 apart; allow a few.
const MAX_FORWARD_GAP: u64 = 2048;

/// Static configuration for one relayer instance.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelayerConfig {
    /// On-disk path for `state.json`. Recovery on startup reads this.
    pub state_path: PathBuf,
    /// Interval between attempts in [`Relayer::run_loop`].
    #[serde(default = "default_poll_interval")]
    pub poll_interval: Duration,
    /// After this many consecutive failures on the same target seqNo,
    /// emit a warning. Doesn't stop the relayer; operator-visible only.
    #[serde(default = "default_max_attempts_warn")]
    pub max_attempts_warn: u32,
    /// After this many consecutive rejections on the same target seqNo,
    /// return [`RelayerError::Stuck`] from `tick()`. The daemon loop
    /// treats `Stuck` as terminal and exits (unlike a normal `Reverted`
    /// which is soft-retried forever under exponential backoff). Applies
    /// to both the verifyBlock lane (`attempts_since_progress`) and the
    /// applyBkSetUpdate lane (`bk_update_attempts_since_progress`).
    #[serde(default = "default_max_attempts_abort")]
    pub max_attempts_abort: u32,
}

fn default_poll_interval() -> Duration {
    Duration::from_secs(2)
}
fn default_max_attempts_warn() -> u32 {
    16
}
fn default_max_attempts_abort() -> u32 {
    3
}

impl RelayerConfig {
    /// Sensible defaults: 2-second polling, warn after 16 attempts on
    /// the same block, hard-abort after 3.
    pub fn new(state_path: impl Into<PathBuf>) -> Self {
        Self {
            state_path: state_path.into(),
            poll_interval: default_poll_interval(),
            max_attempts_warn: default_max_attempts_warn(),
            max_attempts_abort: default_max_attempts_abort(),
        }
    }
}

/// What happened during a single [`Relayer::tick`] call.
#[derive(Clone, Debug)]
pub enum TickOutcome {
    /// A block was successfully submitted and the on-chain anchor moved.
    Verified {
        seq_no: u64,
        new_state: BridgeOnChainState,
        tx_hash: Option<alloy::primitives::B256>,
    },
    /// A BK-set update was applied on-chain.
    BkUpdateApplied {
        seq_no: u64,
        new_state: BridgeOnChainState,
        tx_hash: Option<alloy::primitives::B256>,
    },
    /// `BlockSource` says the next block isn't available yet (and no
    /// BK-update was pending).
    NotYetAvailable { target_seq_no: u64 },
    /// The bridge reverted `verifyBlock`.
    BridgeReverted { target_seq_no: u64, reason: String },
    /// The bridge reverted `applyBkSetUpdate`.
    BkUpdateReverted { seq_no: u64, reason: String },
}

/// Single relayer instance. Holds a block source, a BK-update source, and
/// a [`BridgeClient`]. For the live path both sources are the same
/// [`crate::live_source::LiveBlockSource`]; file-driven tests pair
/// [`crate::source::InMemoryBlockSource`] with
/// [`crate::source::EmptyBkUpdateSource`].
pub struct Relayer<S: BlockSource, U: BkUpdateSource, B: BridgeClient> {
    config: RelayerConfig,
    source: Arc<S>,
    bk_update_source: Arc<U>,
    bridge: Arc<B>,
    state: RelayerState,
}

impl<S: BlockSource, U: BkUpdateSource, B: BridgeClient> Relayer<S, U, B> {
    /// Build a relayer, loading any existing state from disk. If
    /// `state.json` doesn't exist, a fresh [`RelayerState`] is created.
    pub fn new(
        config: RelayerConfig,
        source: Arc<S>,
        bk_update_source: Arc<U>,
        bridge: Arc<B>,
    ) -> Result<Self, RelayerError> {
        let state = RelayerState::load(&config.state_path)?.unwrap_or_default();
        Ok(Self {
            config,
            source,
            bk_update_source,
            bridge,
            state,
        })
    }

    /// Read-only access to the persisted state (useful for tests + the
    /// CLI binary's status output).
    pub fn state(&self) -> &RelayerState {
        &self.state
    }

    /// Mutable access for startup drift audit (daemon-live).
    pub fn state_mut(&mut self) -> &mut RelayerState {
        &mut self.state
    }

    /// Access the underlying bridge (lets tests call `read_state` without
    /// going through a full `tick`).
    pub fn bridge(&self) -> &B {
        &self.bridge
    }

    pub fn persist_state(&self) -> Result<(), RelayerError> {
        self.state.save(&self.config.state_path)
    }

    /// One step of the loop. Idempotent in the sense that calling
    /// `tick()` twice in a row when the second call's data isn't
    /// available returns `NotYetAvailable` rather than re-submitting.
    pub async fn tick(&mut self) -> Result<TickOutcome, RelayerError> {
        let on_chain = self.bridge.read_state().await?;

        // Check B — chain monotonicity vs last observed snapshot.
        if let Some(remembered) = &self.state.last_observed_on_chain {
            if let Err(drift) = check_chain_monotonicity(remembered, &on_chain, MAX_FORWARD_GAP) {
                error!(?drift, "chain monotonicity drift — halting");
                return Err(RelayerError::other(format!("chain drift: {drift}")));
            }
        }

        if let Some(local_last) = self.state.last_processed_seqno {
            if local_last > on_chain.last_seen_block_seq_no {
                warn!(
                    local = local_last,
                    on_chain = on_chain.last_seen_block_seq_no,
                    "local state is ahead of bridge — assuming chain rolled back / wrong endpoint",
                );
            }
        }

        // ── Phase 1: drain BK-set updates ────────────────────────────
        let bk_target = on_chain.last_bk_set_update_seq_no.saturating_add(1);
        if let Some(upd) = self.bk_update_source.fetch_bk_update(bk_target).await? {
            if upd.old_commitment_l2 != on_chain.bk_set_commitment {
                self.state.record_bk_update_attempt(upd.block_seq_no);
                self.persist_state()?;
                return Ok(TickOutcome::BkUpdateReverted {
                    seq_no: upd.block_seq_no,
                    reason: format!(
                        "off-chain old_commitment {:#x} != on-chain {:#x}",
                        upd.old_commitment_l2, on_chain.bk_set_commitment
                    ),
                });
            }
            match self.bridge.submit_bk_set_update(&upd).await? {
                BkSetUpdateSubmitOutcome::Applied {
                    new_state,
                    tx_hash,
                } => {
                    self.bk_update_source
                        .ack_last_bk_update(upd.block_seq_no)
                        .await?;
                    self.after_ack_consistency(upd.block_seq_no, &new_state)
                        .await?;
                    self.state.record_bk_update_progress(upd.block_seq_no);
                    self.state.last_observed_on_chain = Some(new_state.clone());
                    self.persist_state()?;
                    info!(
                        seq_no = upd.block_seq_no,
                        tx = ?tx_hash,
                        "bk-set update applied",
                    );
                    return Ok(TickOutcome::BkUpdateApplied {
                        seq_no: upd.block_seq_no,
                        new_state,
                        tx_hash,
                    });
                },
                BkSetUpdateSubmitOutcome::Reverted {
                    reason,
                } => {
                    // Keep pending so next tick retries the same update.
                    self.state.record_bk_update_attempt(upd.block_seq_no);
                    self.persist_state()?;
                    warn!(
                        seq_no = upd.block_seq_no,
                        reason = %reason,
                        "bk-set update reverted",
                    );
                    if self.state.bk_update_attempts_since_progress
                        >= self.config.max_attempts_abort
                    {
                        return Err(RelayerError::Stuck {
                            seq_no: upd.block_seq_no,
                            attempts: self.state.bk_update_attempts_since_progress,
                            reason: format!("applyBkSetUpdate: {reason}"),
                        });
                    }
                    return Ok(TickOutcome::BkUpdateReverted {
                        seq_no: upd.block_seq_no,
                        reason,
                    });
                },
            }
        }

        // ── Phase 2: verifyBlock lane ────────────────────────────────
        let target = on_chain.last_seen_block_seq_no.saturating_add(1);

        debug!(target_seq_no = target, "fetching block");
        let block = match self.source.fetch(target).await? {
            Some(b) => b,
            None => {
                self.state.record_attempt(target);
                self.persist_state()?;
                if self.state.attempts_since_progress >= self.config.max_attempts_warn {
                    warn!(
                        seqno = target,
                        attempts = self.state.attempts_since_progress,
                        "block source still has no data; relayer is stalled",
                    );
                }
                return Ok(TickOutcome::NotYetAvailable {
                    target_seq_no: target,
                });
            },
        };

        // The source may fall forward to the next available key-block proof
        // (`>= target`), since AN emits proofs only for 512-spaced key
        // blocks. A block *behind* the cursor is still a real error.
        if block.block_seq_no < target {
            return Err(RelayerError::SeqNoMismatch {
                requested: target,
                got: block.block_seq_no,
            });
        }
        let seq_no = block.block_seq_no;
        block.validate_shape()?;

        // Storage v2.0 (2026-08-04): compare against the per-layer anchor
        // pick, not the immutable genesis seed. The prover derives
        // `block.prev_max_level_layer_hash` from
        // `min(num_layers, highest_active_layer)`; the contract's
        // `expectedPrevAnchor(num_layers)` mirrors that exactly.
        let chain_anchor = self.bridge.expected_prev_anchor(block.num_layers).await?;
        if block.prev_max_level_layer_hash != chain_anchor {
            self.state.record_attempt(target);
            self.persist_state()?;
            return Ok(TickOutcome::BridgeReverted {
                target_seq_no: target,
                reason: format!(
                    "off-chain prev_max_level_layer_hash {:#x} != on-chain \
                     expectedPrevAnchor({})={:#x}",
                    block.prev_max_level_layer_hash, block.num_layers, chain_anchor
                ),
            });
        }
        if block.bk_set_commitment != on_chain.bk_set_commitment {
            self.state.record_attempt(target);
            self.persist_state()?;
            return Ok(TickOutcome::BridgeReverted {
                target_seq_no: target,
                reason: format!(
                    "off-chain bk_set_commitment {:#x} != on-chain {:#x}",
                    block.bk_set_commitment, on_chain.bk_set_commitment
                ),
            });
        }

        match self.bridge.submit_block(&block).await? {
            SubmitOutcome::Verified {
                new_state,
                tx_hash,
            } => {
                self.source.ack_last_bundle(seq_no).await?;
                self.after_ack_consistency(seq_no, &new_state).await?;
                self.state.record_progress(seq_no);
                self.state.last_observed_on_chain = Some(new_state.clone());
                self.persist_state()?;
                info!(
                    seq_no,
                    target,
                    fin_type = ?block.fin_type,
                    num_layers = block.num_layers,
                    tx = ?tx_hash,
                    "verified",
                );
                Ok(TickOutcome::Verified {
                    seq_no,
                    new_state,
                    tx_hash,
                })
            },
            SubmitOutcome::Reverted {
                reason,
            } => {
                self.state.record_attempt(target);
                self.persist_state()?;
                warn!(
                    seq_no = target,
                    attempts = self.state.attempts_since_progress,
                    reason = %reason,
                    "bridge reverted",
                );
                if self.state.attempts_since_progress >= self.config.max_attempts_abort {
                    return Err(RelayerError::Stuck {
                        seq_no: target,
                        attempts: self.state.attempts_since_progress,
                        reason: format!("verifyBlock: {reason}"),
                    });
                }
                Ok(TickOutcome::BridgeReverted {
                    target_seq_no: target,
                    reason,
                })
            },
        }
    }

    async fn after_ack_consistency(
        &self,
        seq_no: u64,
        actual: &BridgeOnChainState,
    ) -> Result<(), RelayerError> {
        let Some(expected) = self.source.driver_snapshot().await else {
            return Ok(());
        };
        if let Err(drift) = check_history_consistency(&expected, actual) {
            error!(
                seq_no,
                ?drift,
                "contract/driver global-history-data drift — halting"
            );
            return Err(RelayerError::other(format!("drift: {drift}")));
        }
        Ok(())
    }

    /// Convenience: tick repeatedly with `poll_interval` between
    /// iterations, until `max_ticks` is reached or `should_stop` returns
    /// `true`. Used by the CLI binary; tests prefer to call `tick()`
    /// directly.
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
}

// ─────────────────────────────────────────────────────────────────────
// Tests — drive the loop end-to-end against MockBridgeClient
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use alloy::primitives::{Bytes, U256};
    use tempfile::tempdir;

    use super::*;
    use crate::{
        bridge::MockBridgeClient,
        source::{EmptyBkUpdateSource, InMemoryBlockSource},
        types::{AnBlockData, FinalizationType, MAX_LAYER_HASHES},
    };

    const BK: u64 = 0xBE5E7;

    fn block(seq: u64, prev_anchor: U256) -> AnBlockData {
        let mut layer_hashes = [U256::ZERO; MAX_LAYER_HASHES];
        layer_hashes[0] = U256::from(seq * 100 + 1);
        AnBlockData {
            fin_type: if seq.is_multiple_of(3) {
                FinalizationType::Fallback
            } else {
                FinalizationType::Primary
            },
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
    ) -> Relayer<InMemoryBlockSource, EmptyBkUpdateSource, MockBridgeClient> {
        let cfg = RelayerConfig {
            state_path,
            poll_interval: Duration::from_millis(0),
            max_attempts_warn: 16,
            // Tests want soft-retry: assertions inspect state after long
            // revert streaks and would trip the abort gate.
            max_attempts_abort: u32::MAX,
        };
        Relayer::new(cfg, source, Arc::new(EmptyBkUpdateSource), bridge).unwrap()
    }

    #[tokio::test]
    async fn tick_advances_through_five_blocks() {
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

        let mut relayer = make_relayer(
            source.clone(),
            bridge.clone(),
            dir.path().join("state.json"),
        );
        for seq in 1..=5 {
            match relayer.tick().await.unwrap() {
                TickOutcome::Verified {
                    seq_no,
                    new_state,
                    ..
                } => {
                    assert_eq!(seq_no, seq);
                    assert_eq!(new_state.last_seen_block_seq_no, seq);
                },
                other => panic!("expected Verified at seq {seq}, got {other:?}"),
            }
        }
        assert_eq!(bridge.accepted_count(), 5);
        assert_eq!(relayer.state().last_processed_seqno, Some(5));
        assert_eq!(relayer.state().attempts_since_progress, 0);
    }

    #[tokio::test]
    async fn tick_returns_not_yet_available_when_source_empty() {
        let dir = tempdir().unwrap();
        let bridge = Arc::new(MockBridgeClient::with_genesis(
            U256::from(BK),
            U256::ZERO,
            Arc::new(|_| true),
        ));
        let source = Arc::new(InMemoryBlockSource::new());
        let mut relayer = make_relayer(
            source.clone(),
            bridge.clone(),
            dir.path().join("state.json"),
        );

        match relayer.tick().await.unwrap() {
            TickOutcome::NotYetAvailable {
                target_seq_no,
            } => assert_eq!(target_seq_no, 1),
            other => panic!("expected NotYetAvailable, got {other:?}"),
        }
        assert_eq!(bridge.accepted_count(), 0);
        assert_eq!(relayer.state().last_processed_seqno, None);
        assert_eq!(relayer.state().last_attempt_seqno, Some(1));
        assert_eq!(relayer.state().attempts_since_progress, 1);

        source.insert(block(1, U256::ZERO));
        match relayer.tick().await.unwrap() {
            TickOutcome::Verified {
                seq_no, ..
            } => assert_eq!(seq_no, 1),
            other => panic!("expected Verified, got {other:?}"),
        }
        assert_eq!(relayer.state().attempts_since_progress, 0);
    }

    #[tokio::test]
    async fn tick_handles_verifier_rejection_then_recovers() {
        let dir = tempdir().unwrap();
        let verifier = Arc::new(|b: &AnBlockData| b.block_seq_no != 2);
        let bridge = Arc::new(MockBridgeClient::with_genesis(
            U256::from(BK),
            U256::ZERO,
            verifier,
        ));
        let source = Arc::new(InMemoryBlockSource::new());

        let mut anchor = U256::ZERO;
        for seq in 1..=3 {
            let b = block(seq, anchor);
            anchor = b.next_anchor();
            source.insert(b);
        }

        let mut relayer = make_relayer(
            source.clone(),
            bridge.clone(),
            dir.path().join("state.json"),
        );

        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::Verified {
                seq_no: 1,
                ..
            }
        ));
        match relayer.tick().await.unwrap() {
            TickOutcome::BridgeReverted {
                target_seq_no,
                reason,
            } => {
                assert_eq!(target_seq_no, 2);
                assert!(
                    reason.contains("AttestationProofRejected")
                        || reason.contains("LayerHashesProofRejected")
                );
            },
            other => panic!("expected revert, got {other:?}"),
        }
        assert_eq!(relayer.state().last_processed_seqno, Some(1));
        assert_eq!(relayer.state().last_attempt_seqno, Some(2));
        assert!(relayer.state().attempts_since_progress >= 1);
    }

    #[tokio::test]
    async fn restart_resumes_from_persisted_state_and_on_chain_anchor() {
        let dir = tempdir().unwrap();
        let state_path = dir.path().join("state.json");

        let bridge = Arc::new(MockBridgeClient::with_genesis(
            U256::from(BK),
            U256::ZERO,
            Arc::new(|_| true),
        ));
        let source = Arc::new(InMemoryBlockSource::new());

        let mut anchor = U256::ZERO;
        for seq in 1..=4 {
            let b = block(seq, anchor);
            anchor = b.next_anchor();
            source.insert(b);
        }

        {
            let mut r = make_relayer(source.clone(), bridge.clone(), state_path.clone());
            assert!(matches!(r.tick().await.unwrap(), TickOutcome::Verified {
                seq_no: 1,
                ..
            }));
            assert!(matches!(r.tick().await.unwrap(), TickOutcome::Verified {
                seq_no: 2,
                ..
            }));
        }

        let mut r2 = make_relayer(source, bridge.clone(), state_path);
        assert_eq!(r2.state().last_processed_seqno, Some(2));

        assert!(matches!(r2.tick().await.unwrap(), TickOutcome::Verified {
            seq_no: 3,
            ..
        }));
        assert!(matches!(r2.tick().await.unwrap(), TickOutcome::Verified {
            seq_no: 4,
            ..
        }));

        assert_eq!(bridge.accepted_count(), 4);
        assert_eq!(r2.state().last_processed_seqno, Some(4));
    }

    #[tokio::test]
    async fn run_loop_stops_when_should_stop_returns_true() {
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
        let mut r = make_relayer(source, bridge.clone(), dir.path().join("state.json"));

        let history = r
            .run_loop(10, |outcome| {
                matches!(outcome, TickOutcome::Verified {
                    seq_no: 3,
                    ..
                })
            })
            .await
            .unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(bridge.accepted_count(), 3);
    }
}
