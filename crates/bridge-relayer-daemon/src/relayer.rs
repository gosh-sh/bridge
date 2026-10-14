//! Relayer main loop.
//!
//! [`Relayer::tick`] is the testable single-step entry point. It:
//!
//! 1. reads the on-chain anchors (+ Check B vs last observed);
//! 2. **Phase 1** — applies a pending BK-set update (`applyBkSetUpdate`) unless
//!    the previous one still owes an unverified bundle boundary (the contract's
//!    bundle-aware `PrevSlotStillOwedForBundle` gate): rotations before the
//!    next boundary, and the first one past it, may land ahead of the layer
//!    cursor. The live prover is acked only once the next bundle target is
//!    above N, so it keeps the outgoing set for blocks `<= N`.
//! 3. **Phase 2** — fetches the next block and submits `verifyBlock` against
//!    `_expectedBkSetFor` (outgoing set for `seqNo <= N`);
//! 4. on success — acks the live driver (if any), runs Check A, persists state.
//!
//! [`Relayer::run_loop`] just calls `tick` in a `loop` with a configurable
//! delay between iterations and a "max ticks" budget for tests.

use std::{path::PathBuf, sync::Arc, time::Duration};

use alloy::primitives::U256;
use bridge_prover_lib::BUNDLE_STRIDE_L1;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use crate::{
    bridge::{
        is_unconfirmed_submit_failure, BkSetUpdateSubmitOutcome, BridgeClient, BridgeOnChainState,
        SubmitOutcome,
    },
    error::RelayerError,
    history_consistency::{check_chain_monotonicity_with_stride, check_history_consistency},
    source::{BkUpdateSource, BlockSource},
    state::RelayerState,
};

/// Max seq_no gap another actor may advance between ticks before we halt
/// (Check B). Expressed in bundles rather than seq_nos, and derived from the
/// stride rather than restating it: the literal 2048 was written when `P` was 4
/// and the stride 512, so it meant "four bundles". `P` is 8 now
/// (`THINNING_FACTOR_P`), the stride 1024, and the same literal had quietly
/// become two — a sibling relayer that advanced three bundles between our ticks
/// would halt this one with `HistoryDrift` for no reason.
const MAX_FORWARD_GAP_BUNDLES: u64 = 4;
const MAX_FORWARD_GAP: u64 = MAX_FORWARD_GAP_BUNDLES * BUNDLE_STRIDE_L1;

// The invariant behind the number, checked at build time rather than in a test:
// tolerate at least the three-bundle advance from the finding. A future `P`
// bump cannot silently narrow this, and re-hardcoding the literal breaks the
// build.
const _: () = assert!(MAX_FORWARD_GAP >= 3 * BUNDLE_STRIDE_L1);

/// How long a `_rewound` Check B signal may last before it is reported as
/// drift. Read-after-write lag on a load-balanced RPC lasts seconds; a chain
/// still below our last confirmed write after ten minutes is a reorg that
/// dropped the transaction, or a different chain behind the URL.
const MAX_REWIND_WAIT: Duration = Duration::from_secs(600);

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
    /// Bundle stride the live prover uses (`AnchorMode::stride()`).
    /// The deferred-ack rule must use this, not a hardcoded L1 1024:
    /// in L2 the next target is 16384.
    #[serde(default = "default_bundle_stride")]
    pub bundle_stride: u64,
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
fn default_bundle_stride() -> u64 {
    BUNDLE_STRIDE_L1
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
            bundle_stride: default_bundle_stride(),
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
    /// When the current run of `_rewound` Check B signals began; `None`
    /// while Check B passes.
    rewound_since: Option<std::time::Instant>,
    /// See [`MAX_REWIND_WAIT`]; [`Relayer::with_max_rewind_wait`] overrides.
    max_rewind_wait: Duration,
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
            rewound_since: None,
            max_rewind_wait: MAX_REWIND_WAIT,
        })
    }

    /// How long a `_rewound` Check B signal is retried as read-after-write
    /// lag before it is reported as drift. Defaults to [`MAX_REWIND_WAIT`].
    pub fn with_max_rewind_wait(mut self, wait: Duration) -> Self {
        self.max_rewind_wait = wait;
        self
    }

    /// Check B's forward-gap bound: [`MAX_FORWARD_GAP_BUNDLES`] bundles of the
    /// configured stride (4096 seq_nos at L1, 65536 at L2). A sibling relayer,
    /// or our own transaction whose receipt we never read, may advance the
    /// chain that far between two snapshots.
    pub fn max_forward_gap(&self) -> u64 {
        MAX_FORWARD_GAP_BUNDLES * self.config.bundle_stride.max(1)
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
        let on_chain = self.bridge.read_state_pinned().await?;

        // Check B — chain monotonicity vs last observed snapshot, with the
        // contract's own rotation gate and a forward-gap bound sized to the
        // bundle stride. A `_rewound` drift against our remembered snapshot
        // (populated by a receipt-pinned `read_state_at(receipt.block_number)`
        // after our own last successful submit) is almost always
        // read-after-write staleness on the load-balanced public RPC, so it is
        // retried as `NotYetAvailable`. The reads are at `latest`, not at a
        // finalized block, so a rewind can also be a reorg that dropped our
        // transaction: one that lasts `max_rewind_wait` is reported as drift.
        // Jumps / orphan commitments / contract-impossible rotations are
        // reported at once — a stale backend serves an older snapshot, not a
        // different timeline.
        if let Some(remembered) = &self.state.last_observed_on_chain {
            if let Err(drift) = check_chain_monotonicity_with_stride(
                remembered,
                &on_chain,
                self.max_forward_gap(),
                self.config.bundle_stride,
            ) {
                if drift.is_transient_rpc_lag() {
                    let since = *self
                        .rewound_since
                        .get_or_insert_with(std::time::Instant::now);
                    if since.elapsed() < self.max_rewind_wait {
                        warn!(
                            ?drift,
                            "transient RPC read-after-write lag — will retry next tick"
                        );
                        return Ok(TickOutcome::NotYetAvailable {
                            target_seq_no: remembered.last_seen_block_seq_no.saturating_add(1),
                        });
                    }
                    error!(
                        ?drift,
                        for_secs = since.elapsed().as_secs(),
                        "chain still below our last confirmed write — not RPC lag (a reorg that \
                         dropped our transaction?)"
                    );
                    return Err(RelayerError::other(format!(
                        "chain drift: {drift} (for {} s)",
                        since.elapsed().as_secs()
                    )));
                }
                error!(?drift, "chain monotonicity drift — halting");
                return Err(RelayerError::other(format!("chain drift: {drift}")));
            }
        }
        self.rewound_since = None;

        if let Some(local_last) = self.state.last_processed_seqno {
            if local_last > on_chain.last_seen_block_seq_no {
                warn!(
                    local = local_last,
                    on_chain = on_chain.last_seen_block_seq_no,
                    "local state is ahead of bridge — assuming chain rolled back / wrong endpoint",
                );
            }
        }

        // ── Phase 1: apply a rotation when overwriting
        // `storedPrevBkSetCommitment` will not strand an unverified bundle
        // boundary. The contract's `PrevSlotStillOwedForBundle` gate looks
        // only at the previous N: it reverts while the next bundle boundary
        // after the cursor is at or below that N. Rotations therefore apply
        // back-to-back (AN emits bursts of rotations between bundles on
        // shellnet) until one lands at or past that boundary; the next one
        // waits until `verifyBlock` reaches the last boundary at or below
        // it. No permanent stall: `verifyBlock` keeps advancing the cursor,
        // so a waiting rotation is applied on a later tick.
        let bk_target = on_chain.last_bk_set_update_seq_no.saturating_add(1);
        if let Some(upd) = self.bk_update_source.fetch_bk_update(bk_target).await? {
            let prev_slot_still_owed = on_chain.last_bk_set_update_seq_no != 0
                && !Self::prover_rotation_may_ack(&on_chain, self.config.bundle_stride);
            if prev_slot_still_owed {
                let next = Self::next_bundle_boundary(
                    on_chain.last_seen_block_seq_no,
                    self.config.bundle_stride,
                );
                info!(
                    rotation = upd.block_seq_no,
                    last_bk = on_chain.last_bk_set_update_seq_no,
                    last_seen = on_chain.last_seen_block_seq_no,
                    next_owed_boundary = next,
                    "deferring applyBkSetUpdate until verifyBlock covers the next owed bundle \
                     boundary",
                );
            } else if upd.old_commitment_l2 != on_chain.bk_set_commitment {
                self.state.record_bk_update_attempt(upd.block_seq_no);
                self.persist_state()?;
                return Ok(TickOutcome::BkUpdateReverted {
                    seq_no: upd.block_seq_no,
                    reason: format!(
                        "off-chain old_commitment {:#x} != on-chain {:#x}",
                        upd.old_commitment_l2, on_chain.bk_set_commitment
                    ),
                });
            } else {
                match self.bridge.submit_bk_set_update(&upd).await? {
                    BkSetUpdateSubmitOutcome::Applied {
                        new_state,
                        tx_hash,
                    } => {
                        // Keep the prover on the outgoing set until the
                        // next bundle target is above N. Ack the just-
                        // applied seq, not an unpinned `last_bk` read.
                        if Self::prover_rotation_may_ack(&new_state, self.config.bundle_stride) {
                            self.bk_update_source
                                .ack_last_bk_update(upd.block_seq_no)
                                .await?;
                            self.after_ack_consistency(upd.block_seq_no, &new_state)
                                .await?;
                        }
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
                    } if is_unconfirmed_submit_failure(&reason) => {
                        // The RPC refused or lost the request, or the receipt
                        // never came: the rotation may have landed. The next
                        // tick re-reads the chain (Check B accepts a landed
                        // rotation, and Phase 2 acks the prover for it).
                        warn!(
                            seq_no = upd.block_seq_no,
                            reason = %reason,
                            "bk-set update not confirmed — RPC failure, not counted toward the \
                             abort threshold",
                        );
                        return Ok(TickOutcome::BkUpdateReverted {
                            seq_no: upd.block_seq_no,
                            reason,
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
        }

        // ── Phase 2: verifyBlock lane ────────────────────────────────
        self.maybe_ack_prover_rotation(&on_chain).await?;
        let target = on_chain.last_seen_block_seq_no.saturating_add(1);

        debug!(target_seq_no = target, "fetching block");
        let block = match self.source.fetch(target).await? {
            Some(b) => b,
            None => {
                // Waiting for the next bundle is the normal state between
                // bundles: a poll, not an attempt.
                self.state.record_poll(target);
                self.persist_state()?;
                if self.state.polls_since_progress >= self.config.max_attempts_warn {
                    warn!(
                        seqno = target,
                        polls = self.state.polls_since_progress,
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
        let expected_bk = on_chain.expected_bk_set_for(block.block_seq_no);
        if block.bk_set_commitment != expected_bk {
            self.state.record_attempt(target);
            self.persist_state()?;
            return Ok(TickOutcome::BridgeReverted {
                target_seq_no: target,
                reason: format!(
                    "off-chain bk_set_commitment {:#x} != expected {:#x}",
                    block.bk_set_commitment, expected_bk
                ),
            });
        }

        match self.bridge.submit_block(&block).await? {
            SubmitOutcome::Verified {
                new_state,
                tx_hash,
            } => {
                self.source.ack_last_bundle(seq_no).await?;
                self.maybe_ack_prover_rotation(&new_state).await?;
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
            } if is_unconfirmed_submit_failure(&reason) => {
                // The RPC refused or lost the request, or the receipt never
                // came: the transaction may have landed. The next tick
                // re-reads the chain.
                warn!(
                    seq_no = target,
                    reason = %reason,
                    "verifyBlock not confirmed — RPC failure, not counted toward the abort \
                     threshold",
                );
                Ok(TickOutcome::BridgeReverted {
                    target_seq_no: target,
                    reason,
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

    /// Next thinned-bundle seq_no strictly after `last_seen`.
    fn next_bundle_boundary(last_seen: u64, stride: u64) -> u64 {
        let stride = stride.max(1);
        ((last_seen / stride) + 1) * stride
    }

    /// The live prover may rotate once the next bundle it would prove
    /// is signed by the new set (`seq > N`).
    fn prover_rotation_may_ack(on_chain: &BridgeOnChainState, stride: u64) -> bool {
        let n = on_chain.last_bk_set_update_seq_no;
        n != 0 && Self::next_bundle_boundary(on_chain.last_seen_block_seq_no, stride) > n
    }

    /// BK-set the prover must still hold: current set before any
    /// rotation and after the ack; outgoing set during the hold window.
    fn expected_prover_bk(on_chain: &BridgeOnChainState, stride: u64) -> (U256, Option<u64>) {
        let n = on_chain.last_bk_set_update_seq_no;
        if n == 0 {
            (on_chain.bk_set_commitment, Some(0))
        } else if !Self::prover_rotation_may_ack(on_chain, stride) {
            (on_chain.prev_bk_set_commitment, None)
        } else {
            (on_chain.bk_set_commitment, Some(n))
        }
    }

    async fn maybe_ack_prover_rotation(
        &self,
        on_chain: &BridgeOnChainState,
    ) -> Result<(), RelayerError> {
        if !Self::prover_rotation_may_ack(on_chain, self.config.bundle_stride) {
            return Ok(());
        }
        self.bk_update_source
            .ack_last_bk_update(on_chain.last_bk_set_update_seq_no)
            .await
    }

    async fn after_ack_consistency(
        &self,
        seq_no: u64,
        actual: &BridgeOnChainState,
    ) -> Result<(), RelayerError> {
        let Some(expected) = self.source.driver_snapshot().await else {
            return Ok(());
        };
        let (want_bk, want_last_bk) = Self::expected_prover_bk(actual, self.config.bundle_stride);
        if let Err(drift) =
            check_history_consistency(&expected, actual, Some((want_bk, want_last_bk)))
        {
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
        bridge::{BridgeClient, MockBridgeClient},
        source::{BkUpdateSource, BlockSource, EmptyBkUpdateSource, InMemoryBlockSource},
        types::{AnBlockData, BkSetUpdateData, FinalizationType, MAX_LAYER_HASHES},
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
            bundle_stride: BUNDLE_STRIDE_L1,
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
        assert_eq!(relayer.state().polls_since_progress, 1);
        assert_eq!(relayer.state().attempts_since_progress, 0);

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

    struct OneShotBkUpdate {
        upd: std::sync::Mutex<Option<BkSetUpdateData>>,
        acks: std::sync::atomic::AtomicU64,
    }

    #[async_trait::async_trait]
    impl BkUpdateSource for OneShotBkUpdate {
        async fn fetch_bk_update(
            &self,
            _target: u64,
        ) -> Result<Option<BkSetUpdateData>, RelayerError> {
            Ok(self.upd.lock().unwrap().clone())
        }

        async fn ack_last_bk_update(&self, seq_no: u64) -> Result<(), RelayerError> {
            let mut g = self.upd.lock().unwrap();
            if g.as_ref().is_some_and(|u| u.block_seq_no == seq_no) {
                *g = None;
                self.acks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(())
        }
    }

    fn dummy_update(seq: u64, last_seen: u64) -> BkSetUpdateData {
        BkSetUpdateData {
            fin_type: FinalizationType::Primary,
            block_id: U256::from(seq),
            block_seq_no: seq,
            attestation_last_seen: last_seen,
            old_commitment_l2: U256::from(BK),
            new_commitment_l3: U256::from(0xC0FFEEu64),
            sibling_h01: [0u8; 32],
            sibling_h4_7: [0u8; 32],
            sibling_h8_15: [0u8; 32],
            attestation_proof: Bytes::from(vec![0u8; 32]),
        }
    }

    #[tokio::test]
    async fn tick_applies_rotation_ahead_of_cursor_then_verifies() {
        let dir = tempdir().unwrap();
        let bridge = Arc::new(MockBridgeClient::with_genesis(
            U256::from(BK),
            U256::ZERO,
            Arc::new(|_| true),
        ));
        let source = Arc::new(InMemoryBlockSource::new());
        let mut anchor = U256::ZERO;
        for seq in 1..=3 {
            let b = block(seq, anchor);
            anchor = b.next_anchor();
            source.insert(b);
        }
        let bk = Arc::new(OneShotBkUpdate {
            upd: std::sync::Mutex::new(Some(dummy_update(3, 0))),
            acks: std::sync::atomic::AtomicU64::new(0),
        });
        let cfg = RelayerConfig {
            state_path: dir.path().join("state.json"),
            poll_interval: Duration::from_millis(0),
            max_attempts_warn: 16,
            max_attempts_abort: u32::MAX,
            bundle_stride: BUNDLE_STRIDE_L1,
        };
        let mut r = Relayer::new(cfg, source, bk.clone(), bridge.clone()).unwrap();

        // First rotation may apply before any verifyBlock. Blocks 1..=3
        // are then accepted under the previous commitment.
        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::BkUpdateApplied {
                seq_no: 3,
                ..
            }
        ));
        assert_eq!(
            bridge.read_state().await.unwrap().last_bk_set_update_seq_no,
            3
        );
        assert!(matches!(r.tick().await.unwrap(), TickOutcome::Verified {
            seq_no: 1,
            ..
        }));
        assert!(matches!(r.tick().await.unwrap(), TickOutcome::Verified {
            seq_no: 2,
            ..
        }));
        assert!(matches!(r.tick().await.unwrap(), TickOutcome::Verified {
            seq_no: 3,
            ..
        }));
        assert_eq!(r.state().bk_update_attempts_since_progress, 0);
        // N=3, next L1 bundle is 1024 > 3, so the prover may rotate now.
        assert_eq!(bk.acks.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn tick_defers_prover_ack_while_next_bundle_at_or_below_n() {
        let dir = tempdir().unwrap();
        let bridge = Arc::new(MockBridgeClient::with_genesis(
            U256::from(BK),
            U256::ZERO,
            Arc::new(|_| true),
        ));
        let source = Arc::new(InMemoryBlockSource::new());
        let mut anchor = U256::ZERO;
        for seq in 1..=3 {
            let b = block(seq, anchor);
            anchor = b.next_anchor();
            source.insert(b);
        }
        // Rotation past the next L1 bundle (1024). The prover must keep
        // the outgoing set until that bundle is proven.
        let bk = Arc::new(OneShotBkUpdate {
            upd: std::sync::Mutex::new(Some(dummy_update(2000, 0))),
            acks: std::sync::atomic::AtomicU64::new(0),
        });
        let cfg = RelayerConfig {
            state_path: dir.path().join("state.json"),
            poll_interval: Duration::from_millis(0),
            max_attempts_warn: 16,
            max_attempts_abort: u32::MAX,
            bundle_stride: BUNDLE_STRIDE_L1,
        };
        let mut r = Relayer::new(cfg, source, bk.clone(), bridge.clone()).unwrap();

        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::BkUpdateApplied {
                seq_no: 2000,
                ..
            }
        ));
        assert_eq!(bk.acks.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(matches!(r.tick().await.unwrap(), TickOutcome::Verified {
            seq_no: 1,
            ..
        }));
        assert!(matches!(r.tick().await.unwrap(), TickOutcome::Verified {
            seq_no: 2,
            ..
        }));
        assert!(matches!(r.tick().await.unwrap(), TickOutcome::Verified {
            seq_no: 3,
            ..
        }));
        assert_eq!(
            bk.acks.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "next bundle 1024 is still <= 2000"
        );
    }

    /// Shared fake prover: one signer set, a stride, queued rotations.
    /// Serves a boundary only when the block was signed by the set it holds.
    struct FakeProver {
        stride: u64,
        current_bk: std::sync::Mutex<U256>,
        last_bk: std::sync::atomic::AtomicU64,
        pending: std::sync::Mutex<Option<BkSetUpdateData>>,
        queued: std::sync::Mutex<Vec<BkSetUpdateData>>,
        blocks: std::sync::Mutex<std::collections::BTreeMap<u64, AnBlockData>>,
        acks: std::sync::atomic::AtomicU64,
    }

    impl FakeProver {
        fn new(stride: u64, genesis: U256) -> Self {
            Self {
                stride,
                current_bk: std::sync::Mutex::new(genesis),
                last_bk: std::sync::atomic::AtomicU64::new(0),
                pending: std::sync::Mutex::new(None),
                queued: std::sync::Mutex::new(Vec::new()),
                blocks: std::sync::Mutex::new(std::collections::BTreeMap::new()),
                acks: std::sync::atomic::AtomicU64::new(0),
            }
        }

        fn insert_block(&self, b: AnBlockData) {
            self.blocks.lock().unwrap().insert(b.block_seq_no, b);
        }

        fn queue_update(&self, upd: BkSetUpdateData) {
            self.queued.lock().unwrap().push(upd);
        }
    }

    #[async_trait::async_trait]
    impl BlockSource for FakeProver {
        async fn fetch(&self, target: u64) -> Result<Option<AnBlockData>, RelayerError> {
            let hold = *self.current_bk.lock().unwrap();
            let blocks = self.blocks.lock().unwrap();
            Ok(blocks
                .values()
                .find(|b| {
                    b.block_seq_no >= target
                        && b.block_seq_no.is_multiple_of(self.stride)
                        && b.bk_set_commitment == hold
                })
                .cloned())
        }
    }

    #[async_trait::async_trait]
    impl BkUpdateSource for FakeProver {
        async fn fetch_bk_update(
            &self,
            target: u64,
        ) -> Result<Option<BkSetUpdateData>, RelayerError> {
            {
                let pending = self.pending.lock().unwrap();
                if let Some(p) = pending.as_ref() {
                    if p.block_seq_no >= target {
                        return Ok(Some(p.clone()));
                    }
                    return Ok(None);
                }
            }
            let mut queued = self.queued.lock().unwrap();
            if let Some(i) = queued.iter().position(|u| u.block_seq_no >= target) {
                let u = queued.remove(i);
                *self.pending.lock().unwrap() = Some(u.clone());
                return Ok(Some(u));
            }
            Ok(None)
        }

        async fn ack_last_bk_update(&self, seq_no: u64) -> Result<(), RelayerError> {
            if self.last_bk.load(std::sync::atomic::Ordering::SeqCst) >= seq_no {
                return Ok(());
            }
            let mut pending = self.pending.lock().unwrap();
            let matches = pending.as_ref().is_some_and(|u| u.block_seq_no == seq_no);
            if !matches {
                return Ok(());
            }
            let u = pending.take().expect("matched");
            *self.current_bk.lock().unwrap() = u.new_commitment_l3;
            self.last_bk
                .store(u.block_seq_no, std::sync::atomic::Ordering::SeqCst);
            self.acks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn tick_acks_immediately_when_l2_next_target_is_past_n() {
        let dir = tempdir().unwrap();
        let bridge = Arc::new(MockBridgeClient::with_genesis(
            U256::from(BK),
            U256::ZERO,
            Arc::new(|_| true),
        ));
        let prover = Arc::new(FakeProver::new(16_384, U256::from(BK)));
        prover.queue_update(dummy_update(2000, 0));
        let cfg = RelayerConfig {
            state_path: dir.path().join("state.json"),
            poll_interval: Duration::from_millis(0),
            max_attempts_warn: 16,
            max_attempts_abort: u32::MAX,
            bundle_stride: 16_384,
        };
        let mut r = Relayer::new(cfg, prover.clone(), prover.clone(), bridge).unwrap();
        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::BkUpdateApplied {
                seq_no: 2000,
                ..
            }
        ));
        assert_eq!(prover.acks.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn tick_keeps_pending_n2_when_acking_already_applied_n1() {
        // N1 lives in the second bundle window and the layer cursor has
        // not covered the first boundary yet, so the relayer refuses to ack
        // the prover for it. The test then acks by hand — standing in for a
        // source that has already released N1, which the live source never
        // does before that boundary is proven — so that N2 reaches the
        // relayer's own deferral guard. The guard must log the deferral,
        // fall through to the verifyBlock lane (no blocks available →
        // NotYetAvailable) and leave N2 in the prover's pending slot.
        let dir = tempdir().unwrap();
        let bridge = Arc::new(MockBridgeClient::with_genesis_stride(
            U256::from(BK),
            U256::ZERO,
            1024,
            Arc::new(|_| true),
        ));
        let prover = Arc::new(FakeProver::new(1024, U256::from(BK)));
        prover.queue_update(dummy_update(1500, 0));
        let mut n2 = dummy_update(3000, 0);
        n2.old_commitment_l2 = U256::from(0xC0FFEEu64);
        n2.new_commitment_l3 = U256::from(0xD00Du64);
        prover.queue_update(n2);
        let cfg = RelayerConfig {
            state_path: dir.path().join("state.json"),
            poll_interval: Duration::from_millis(0),
            max_attempts_warn: 16,
            max_attempts_abort: u32::MAX,
            bundle_stride: 1024,
        };
        let mut r = Relayer::new(cfg, prover.clone(), prover.clone(), bridge).unwrap();
        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::BkUpdateApplied {
                seq_no: 1500,
                ..
            }
        ));
        // N1 sits in the second bundle window and `last_seen` is still
        // 0, so the submit-site may_ack guard refuses the ack — the live
        // prover must keep signing against the outgoing set until
        // `verifyBlock` crosses the next owed boundary. Simulate the
        // asynchronous ack that would eventually land once `verifyBlock`
        // catches up (`maybe_ack_prover_rotation` on a later tick) so
        // that `pending` is clear for N2 on the next tick.
        assert_eq!(prover.acks.load(std::sync::atomic::Ordering::SeqCst), 0);
        prover.ack_last_bk_update(1500).await.unwrap();
        assert_eq!(prover.acks.load(std::sync::atomic::Ordering::SeqCst), 1);
        // N2 is discovered and deferred (bundle 1024 is owed by N1).
        let out = r.tick().await.unwrap();
        assert!(
            matches!(out, TickOutcome::NotYetAvailable { .. }),
            "got {out:?}"
        );
        assert!(
            prover
                .pending
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|u| u.block_seq_no == 3000),
            "N2 must stay pending"
        );
    }

    #[tokio::test]
    async fn tick_defers_second_rotation_when_prev_slot_still_owes_a_bundle() {
        // Prev-slot-owed path (bundle-aware `PrevSlotStillOwedForBundle`):
        // N1 lands past the first bundle boundary, so overwriting
        // `storedPrevBkSetCommitment` with N2 would strand the
        // seqno-≤-1024 boundary still signed by the outgoing set. The
        // daemon must defer N2 (log, keep pending) rather than hard-abort
        // — `verifyBlock` eventually crosses `nextOwedBoundary`, and the
        // next tick retries the apply.
        let dir = tempdir().unwrap();
        // Mock must agree with the daemon on `bundle_stride` or the two
        // sides disagree about whether the gate is tripped.
        let bridge = Arc::new(MockBridgeClient::with_genesis_stride(
            U256::from(BK),
            U256::ZERO,
            1024,
            Arc::new(|_| true),
        ));
        let prover = Arc::new(FakeProver::new(1024, U256::from(BK)));
        // N1 = 2000 lies in bundle window [1024, 2048); next owed
        // boundary from `last_seen = 0` is 1024, which is `<= 2000`
        // and trips the gate.
        prover.queue_update(dummy_update(2000, 0));
        let mut n2 = dummy_update(2500, 0);
        n2.old_commitment_l2 = U256::from(0xC0FFEEu64);
        n2.new_commitment_l3 = U256::from(0xD00Du64);
        prover.queue_update(n2);
        let cfg = RelayerConfig {
            state_path: dir.path().join("state.json"),
            poll_interval: Duration::from_millis(0),
            max_attempts_warn: 16,
            max_attempts_abort: u32::MAX,
            bundle_stride: 1024,
        };
        let mut r = Relayer::new(cfg, prover.clone(), prover.clone(), bridge).unwrap();
        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::BkUpdateApplied {
                seq_no: 2000,
                ..
            }
        ));
        // Second tick: the fake prover, like the live source, withholds N2
        // while N1 is still unacked, so the relayer does not even see N2 and
        // goes straight to the verifyBlock lane — no block is queued, so the
        // tick finishes idle without error. The relayer's own deferral guard
        // is reached in `tick_defers_a_rotation_the_contract_would_refuse`.
        let outcome = r.tick().await.expect("deferral path must not error");
        assert!(
            !matches!(outcome, TickOutcome::BkUpdateApplied { .. }),
            "N2 must not apply while prev slot is owed, got {outcome:?}"
        );
    }

    /// Hands out the next queued rotation at or above the target and keeps
    /// no pending slot — unlike the live source, which withholds a newer
    /// rotation while one is still unacked. With it the relayer's own
    /// deferral guard is reachable.
    struct QueueBkUpdates {
        queued: std::sync::Mutex<Vec<BkSetUpdateData>>,
    }

    #[async_trait::async_trait]
    impl BkUpdateSource for QueueBkUpdates {
        async fn fetch_bk_update(
            &self,
            target: u64,
        ) -> Result<Option<BkSetUpdateData>, RelayerError> {
            Ok(self
                .queued
                .lock()
                .unwrap()
                .iter()
                .find(|u| u.block_seq_no >= target)
                .cloned())
        }

        async fn ack_last_bk_update(&self, _seq_no: u64) -> Result<(), RelayerError> {
            Ok(())
        }
    }

    /// [`MockBridgeClient`] with scripted failures. Each `verify_script` /
    /// `bk_script` entry answers one submit, in order: `None` passes it
    /// through, `Some((reason, landed))` returns `Reverted { reason }` —
    /// after applying it on the mock first when `landed`, as when a
    /// transaction lands but its receipt read fails. `rewound`, when set, is
    /// what every read returns.
    struct FlakyBridge {
        inner: MockBridgeClient,
        verify_script: std::sync::Mutex<std::collections::VecDeque<Option<String>>>,
        bk_script: std::sync::Mutex<std::collections::VecDeque<Option<(String, bool)>>>,
        rewound: std::sync::Mutex<Option<BridgeOnChainState>>,
    }

    impl FlakyBridge {
        fn new(inner: MockBridgeClient) -> Self {
            Self {
                inner,
                verify_script: std::sync::Mutex::new(Default::default()),
                bk_script: std::sync::Mutex::new(Default::default()),
                rewound: std::sync::Mutex::new(None),
            }
        }
    }

    #[async_trait::async_trait]
    impl BridgeClient for FlakyBridge {
        async fn read_state(&self) -> Result<BridgeOnChainState, RelayerError> {
            let rewound = self.rewound.lock().unwrap().clone();
            match rewound {
                Some(s) => Ok(s),
                None => self.inner.read_state().await,
            }
        }

        async fn submit_block(&self, block: &AnBlockData) -> Result<SubmitOutcome, RelayerError> {
            let next = self.verify_script.lock().unwrap().pop_front().flatten();
            match next {
                Some(reason) => Ok(SubmitOutcome::Reverted {
                    reason,
                }),
                None => self.inner.submit_block(block).await,
            }
        }

        async fn submit_bk_set_update(
            &self,
            update: &BkSetUpdateData,
        ) -> Result<BkSetUpdateSubmitOutcome, RelayerError> {
            let next = self.bk_script.lock().unwrap().pop_front().flatten();
            match next {
                Some((reason, landed)) => {
                    if landed {
                        self.inner.submit_bk_set_update(update).await?;
                    }
                    Ok(BkSetUpdateSubmitOutcome::Reverted {
                        reason,
                    })
                },
                None => self.inner.submit_bk_set_update(update).await,
            }
        }

        async fn expected_prev_anchor(&self, num_layers: u8) -> Result<U256, RelayerError> {
            self.inner.expected_prev_anchor(num_layers).await
        }
    }

    fn rotation(seq: u64, old: u64, new: u64) -> BkSetUpdateData {
        let mut u = dummy_update(seq, 0);
        u.old_commitment_l2 = U256::from(old);
        u.new_commitment_l3 = U256::from(new);
        u
    }

    fn cfg_with(state_path: PathBuf, stride: u64, abort: u32) -> RelayerConfig {
        RelayerConfig {
            state_path,
            poll_interval: Duration::from_millis(0),
            max_attempts_warn: 16,
            max_attempts_abort: abort,
            bundle_stride: stride,
        }
    }

    #[tokio::test]
    async fn tick_applies_a_burst_back_to_back_then_proves_the_boundary() {
        // Shellnet shape at stride 1024: two rotations ahead of the cursor,
        // the second past the next boundary. Both apply with no verifyBlock
        // between them; boundary 1024 is then proven with the set active at
        // it (the first rotation's), and only then does the prover take the
        // second rotation.
        let dir = tempdir().unwrap();
        let bridge = Arc::new(MockBridgeClient::with_genesis_stride(
            U256::from(BK),
            U256::ZERO,
            1024,
            Arc::new(|_| true),
        ));
        let prover = Arc::new(FakeProver::new(1024, U256::from(BK)));
        prover.queue_update(rotation(500, BK, 0xC0FFEE));
        prover.queue_update(rotation(1500, 0xC0FFEE, 0xD00D));
        let mut boundary = block(1024, U256::ZERO);
        boundary.bk_set_commitment = U256::from(0xC0FFEEu64);
        prover.insert_block(boundary);
        let cfg = cfg_with(dir.path().join("state.json"), 1024, 3);
        let mut r = Relayer::new(cfg, prover.clone(), prover.clone(), bridge.clone()).unwrap();

        // N1 = 500: the next boundary 1024 is above it, so the prover rotates
        // at once.
        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::BkUpdateApplied {
                seq_no: 500,
                ..
            }
        ));
        assert_eq!(prover.acks.load(std::sync::atomic::Ordering::SeqCst), 1);
        // N2 = 1500 applies back-to-back with the cursor still at 0; it is
        // past the boundary, so the prover keeps N1's set for block 1024.
        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::BkUpdateApplied {
                seq_no: 1500,
                ..
            }
        ));
        assert_eq!(bridge.read_state().await.unwrap().last_seen_block_seq_no, 0);
        assert_eq!(prover.acks.load(std::sync::atomic::Ordering::SeqCst), 1);
        // verifyBlock(1024) under the outgoing set; the next boundary, 2048,
        // is above N2, so the prover now takes the second rotation.
        assert!(matches!(r.tick().await.unwrap(), TickOutcome::Verified {
            seq_no: 1024,
            ..
        }));
        assert_eq!(prover.acks.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn tick_defers_a_rotation_the_contract_would_refuse() {
        // N1 = 2000 is past the owed boundary 1024, so the contract reverts
        // N2 until verifyBlock(1024). The relayer must not submit it: it
        // logs the deferral and falls through to the verifyBlock lane.
        let dir = tempdir().unwrap();
        let bridge = Arc::new(MockBridgeClient::with_genesis_stride(
            U256::from(BK),
            U256::ZERO,
            1024,
            Arc::new(|_| true),
        ));
        let bk = Arc::new(QueueBkUpdates {
            queued: std::sync::Mutex::new(vec![
                rotation(2000, BK, 0xC0FFEE),
                rotation(2500, 0xC0FFEE, 0xD00D),
            ]),
        });
        let source = Arc::new(InMemoryBlockSource::new());
        let cfg = cfg_with(dir.path().join("state.json"), 1024, 3);
        let mut r = Relayer::new(cfg, source, bk, bridge.clone()).unwrap();

        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::BkUpdateApplied {
                seq_no: 2000,
                ..
            }
        ));
        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::NotYetAvailable {
                target_seq_no: 1
            }
        ));
        assert_eq!(
            bridge.read_state().await.unwrap().last_bk_set_update_seq_no,
            2000,
            "N2 must not be submitted"
        );
        assert_eq!(r.state().bk_update_attempts_since_progress, 0);
    }

    #[tokio::test]
    async fn polls_and_unconfirmed_submits_do_not_count_toward_abort() {
        // Waiting for a bundle used to count as attempts, so with the default
        // abort threshold of 3 the first RPC hiccup after a wait stopped the
        // daemon.
        let dir = tempdir().unwrap();
        let bridge = Arc::new(FlakyBridge::new(MockBridgeClient::with_genesis(
            U256::from(BK),
            U256::ZERO,
            Arc::new(|_| true),
        )));
        bridge.verify_script.lock().unwrap().extend([
            Some(
                "verifyBlock send failed: server returned an error response: error code -32005: \
                 rate limit exceeded"
                    .to_string(),
            ),
            Some("tx confirmation error: Timeout".to_string()),
        ]);
        let source = Arc::new(InMemoryBlockSource::new());
        let cfg = cfg_with(dir.path().join("state.json"), BUNDLE_STRIDE_L1, 3);
        let mut r =
            Relayer::new(cfg, source.clone(), Arc::new(EmptyBkUpdateSource), bridge).unwrap();

        for _ in 0..5 {
            assert!(matches!(
                r.tick().await.unwrap(),
                TickOutcome::NotYetAvailable { .. }
            ));
        }
        assert_eq!(r.state().polls_since_progress, 5);
        assert_eq!(r.state().attempts_since_progress, 0);

        source.insert(block(1, U256::ZERO));
        for _ in 0..2 {
            assert!(matches!(
                r.tick().await.unwrap(),
                TickOutcome::BridgeReverted { .. }
            ));
        }
        assert_eq!(r.state().attempts_since_progress, 0);
        assert!(matches!(r.tick().await.unwrap(), TickOutcome::Verified {
            seq_no: 1,
            ..
        }));
    }

    #[tokio::test]
    async fn contract_rejections_still_abort() {
        let dir = tempdir().unwrap();
        let bridge = Arc::new(FlakyBridge::new(MockBridgeClient::with_genesis(
            U256::from(BK),
            U256::ZERO,
            Arc::new(|_| true),
        )));
        let revert = "verifyBlock send failed: server returned an error response: error code 3: \
                      execution reverted, data: \"0x87bf1c06\"";
        bridge
            .verify_script
            .lock()
            .unwrap()
            .extend(std::iter::repeat_n(Some(revert.to_string()), 3));
        let source = Arc::new(InMemoryBlockSource::new());
        source.insert(block(1, U256::ZERO));
        let cfg = cfg_with(dir.path().join("state.json"), BUNDLE_STRIDE_L1, 3);
        let mut r = Relayer::new(cfg, source, Arc::new(EmptyBkUpdateSource), bridge).unwrap();

        for _ in 0..2 {
            assert!(matches!(
                r.tick().await.unwrap(),
                TickOutcome::BridgeReverted { .. }
            ));
        }
        assert!(matches!(
            r.tick().await.unwrap_err(),
            RelayerError::Stuck { .. }
        ));
    }

    #[tokio::test]
    async fn tick_survives_a_rotation_that_landed_without_a_receipt() {
        // A burst at stride 1024: N1 = 500 applies normally; N2 = 900 lands
        // but its receipt read fails, so the snapshot still says 500 while
        // the chain says 900 with the cursor at 0. The old Check B called
        // that uncovered and failed every later tick; the contract allowed
        // it (next boundary 1024 > 500), so the relayer must carry on and
        // rotate the prover.
        let dir = tempdir().unwrap();
        let bridge = Arc::new(FlakyBridge::new(MockBridgeClient::with_genesis_stride(
            U256::from(BK),
            U256::ZERO,
            1024,
            Arc::new(|_| true),
        )));
        bridge.bk_script.lock().unwrap().extend([
            None,
            Some(("tx confirmation error: Timeout".to_string(), true)),
        ]);
        let prover = Arc::new(FakeProver::new(1024, U256::from(BK)));
        prover.queue_update(rotation(500, BK, 0xC0FFEE));
        prover.queue_update(rotation(900, 0xC0FFEE, 0xD00D));
        let cfg = cfg_with(dir.path().join("state.json"), 1024, 3);
        let mut r = Relayer::new(cfg, prover.clone(), prover.clone(), bridge.clone()).unwrap();

        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::BkUpdateApplied {
                seq_no: 500,
                ..
            }
        ));
        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::BkUpdateReverted {
                seq_no: 900,
                ..
            }
        ));
        assert_eq!(
            bridge.read_state().await.unwrap().last_bk_set_update_seq_no,
            900
        );
        assert_eq!(r.state().bk_update_attempts_since_progress, 0);
        let out = r.tick().await.expect("a landed rotation is not drift");
        assert!(
            matches!(out, TickOutcome::NotYetAvailable { .. }),
            "got {out:?}"
        );
        assert_eq!(
            prover.acks.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the prover takes the landed rotation"
        );
    }

    #[tokio::test]
    async fn a_rewind_that_outlasts_the_lag_window_is_reported_as_drift() {
        let dir = tempdir().unwrap();
        let bridge = Arc::new(FlakyBridge::new(MockBridgeClient::with_genesis(
            U256::from(BK),
            U256::ZERO,
            Arc::new(|_| true),
        )));
        let source = Arc::new(InMemoryBlockSource::new());
        source.insert(block(1, U256::ZERO));
        let cfg = cfg_with(dir.path().join("state.json"), BUNDLE_STRIDE_L1, 3);
        let mut r = Relayer::new(
            cfg.clone(),
            source.clone(),
            Arc::new(EmptyBkUpdateSource),
            bridge.clone(),
        )
        .unwrap();
        assert!(matches!(r.tick().await.unwrap(), TickOutcome::Verified {
            seq_no: 1,
            ..
        }));

        let mut rewound = bridge.inner.read_state().await.unwrap();
        rewound.last_seen_block_seq_no = 0;
        *bridge.rewound.lock().unwrap() = Some(rewound);
        // Inside the lag window: retried.
        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::NotYetAvailable { .. }
        ));
        // Past it (no allowance at all here): reported as drift.
        let mut strict = Relayer::new(cfg, source, Arc::new(EmptyBkUpdateSource), bridge)
            .unwrap()
            .with_max_rewind_wait(Duration::ZERO);
        let err = strict.tick().await.unwrap_err();
        assert!(
            err.to_string().contains("last_seen_block_seq_no_rewound"),
            "{err}"
        );
    }
}
