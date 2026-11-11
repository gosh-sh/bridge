//! Live AN→ETH block / BK-update source over `bridge_prover_lib::live_driver`.
//!
//! Wraps [`LiveProverDriver`] behind [`BlockSource`] + [`BkUpdateSource`] with
//! **ack-after-submit** for bundles: `fetch` stashes a pending payload;
//! [`Relayer::tick`](crate::relayer::Relayer::tick) calls
//! [`LiveBlockSource::ack_last_bundle`] only after ETH `verifyBlock`
//! accepts the tx. Rotation acks are deferred until the next bundle
//! target is above N, so [`ack_last_bk_update`] may run many ticks after
//! `applyBkSetUpdate` (and answers `AlreadyApplied` if that N is already in
//! the driver).
//!
//! Rotations the contract already has are never proven: the driver checks
//! them against their blocks and hands them over as deltas, which wait in
//! the same pending slot as a proven rotation until the relayer acks them.

use std::{path::PathBuf, sync::Arc};

use alloy::primitives::U256;
use async_trait::async_trait;
use bridge_prover_lib::{
    bridge_state::BridgeState,
    live_driver::{
        BkUpdateDelta, BkUpdateProofArtifacts, BundleProofArtifacts, DriverError,
        LiveBkUpdateEvent, LiveBundleEvent, LiveProverDriver,
    },
    prover_bk_set::ProverBkSet,
};
use tokio::sync::Mutex;
use tracing::warn;

use crate::{
    an_chain_guard::ChainBehindGuard,
    error::RelayerError,
    source::{AckOutcome, AppliedRotation, BkUpdateSource, BlockSource},
    types::{AnBlockData, BkSetUpdateData},
};

/// Filesystem paths the live source snapshots after each successful ack.
#[derive(Clone, Debug)]
pub struct StatePaths {
    pub prover_state_json: PathBuf,
    pub prover_bk_set_json: PathBuf,
}

impl StatePaths {
    pub fn under(state_dir: impl Into<PathBuf>) -> Self {
        let state_dir = state_dir.into();
        Self {
            prover_state_json: state_dir.join("prover_state.json"),
            prover_bk_set_json: state_dir.join("prover_bk_set.json"),
        }
    }
}

/// The rotation the source holds between the driver's poll and the ack.
enum PendingRotation {
    /// Proven by this driver; served for `applyBkSetUpdate` until acked.
    Proven(BkUpdateProofArtifacts),
    /// Already on the contract, checked against its block, no proof.
    Applied(BkUpdateDelta),
}

impl PendingRotation {
    fn seq_no(&self) -> u64 {
        match self {
            PendingRotation::Proven(u) => u.block_seq_no,
            PendingRotation::Applied(d) => d.block_seq_no,
        }
    }

    fn rotation(&self) -> AppliedRotation {
        let (seq_no, old, new) = match self {
            PendingRotation::Proven(u) => (
                u.block_seq_no,
                u.old_bk_set_commitment_be,
                u.new_bk_set_commitment_be,
            ),
            PendingRotation::Applied(d) => (
                d.block_seq_no,
                d.old_bk_set_commitment_be,
                d.new_bk_set_commitment_be,
            ),
        };
        // `Fr::to_repr()` bytes, read as `BkSetUpdateData::from` reads them:
        // the numeric field element the contract stores.
        AppliedRotation {
            seq_no,
            old_commitment: U256::from_le_bytes(old),
            new_commitment: U256::from_le_bytes(new),
        }
    }
}

/// What a test driver answers to `ack_bk_update`.
#[cfg(test)]
#[derive(Clone, Copy)]
enum AckStubKind {
    /// Takes the rotation: the stub's cursor moves to it.
    Accept,
    TooEarly,
    Other,
}

#[cfg(test)]
struct AckStub {
    last_bk_seq: u64,
    kind: AckStubKind,
}

/// Live source sharing one [`LiveProverDriver`] across both lanes.
pub struct LiveBlockSource {
    driver: Option<Arc<Mutex<LiveProverDriver>>>,
    pending_bundle: Mutex<Option<BundleProofArtifacts>>,
    pending_bk_update: Mutex<Option<PendingRotation>>,
    state_paths: StatePaths,
    chain_behind: Mutex<ChainBehindGuard>,
    /// Stands in for the driver in tests. `None` in the daemon: every ack
    /// and poll goes to [`LiveProverDriver`]. A test sets it so a deferred
    /// ack can be exercised without loading a proving ceremony.
    #[cfg(test)]
    stub: Mutex<Option<AckStub>>,
    #[cfg(test)]
    poll_bk_calls: Mutex<u32>,
}

impl LiveBlockSource {
    pub fn new(driver: Arc<Mutex<LiveProverDriver>>, state_paths: StatePaths) -> Self {
        Self {
            driver: Some(driver),
            pending_bundle: Mutex::new(None),
            pending_bk_update: Mutex::new(None),
            state_paths,
            chain_behind: Mutex::new(ChainBehindGuard::default()),
            #[cfg(test)]
            stub: Mutex::new(None),
            #[cfg(test)]
            poll_bk_calls: Mutex::new(0),
        }
    }

    fn live(&self) -> &Arc<Mutex<LiveProverDriver>> {
        self.driver.as_ref().expect("live prover driver")
    }

    pub fn driver(&self) -> Arc<Mutex<LiveProverDriver>> {
        Arc::clone(self.live())
    }

    #[cfg(test)]
    fn stubbed(kind: AckStubKind) -> Self {
        Self {
            driver: None,
            pending_bundle: Mutex::new(None),
            pending_bk_update: Mutex::new(None),
            state_paths: StatePaths::under(std::env::temp_dir()),
            chain_behind: Mutex::new(ChainBehindGuard::default()),
            stub: Mutex::new(Some(AckStub {
                last_bk_seq: 0,
                kind,
            })),
            poll_bk_calls: Mutex::new(0),
        }
    }

    /// Read-only clone of the driver's post-ack `BridgeState` (~40 KiB).
    pub async fn driver_snapshot(&self) -> BridgeState {
        self.snapshot_bridge_state().await
    }

    pub async fn driver_prover_bk_set_snapshot(&self) -> ProverBkSet {
        self.live().lock().await.snapshot_prover_bk_set().clone()
    }

    /// Read-only clone of the pending bundle artifact, if any. Used by the
    /// aggregation wrapper
    /// ([`crate::aggregated_source::AggregatedBlockSource`]) to obtain the
    /// raw `BundleProofArtifacts` — including `last_seen_block_seq_no` and
    /// the Blake2b proof bytes — that the shape- preserving
    /// `AnBlockData::from` conversion drops. This is a peek: it does
    /// NOT clear `pending_bundle`; the wrapper's ack path unchanged.
    pub async fn peek_pending_bundle(&self) -> Option<BundleProofArtifacts> {
        self.pending_bundle.lock().await.clone()
    }

    /// Symmetric peek for the BK-update lane. See [`peek_pending_bundle`].
    /// Only a proven rotation: one already on the contract has no proof to
    /// aggregate.
    pub async fn peek_pending_bk_update(&self) -> Option<BkUpdateProofArtifacts> {
        match self.pending_bk_update.lock().await.as_ref() {
            Some(PendingRotation::Proven(u)) => Some(u.clone()),
            Some(PendingRotation::Applied(_)) | None => None,
        }
    }

    /// Advance the driver cursor after ETH `verifyBlock` succeeded.
    pub async fn ack_last_bundle(&self, seq_no: u64) -> Result<(), RelayerError> {
        self.do_ack_bundle(seq_no).await
    }

    /// Advance the driver after the rotation's bundle gate opened.
    /// Takes the pending rotation, proven or applied, only on an exact seq
    /// match (`Acked`); `NothingPending` otherwise. If the driver already
    /// has `stored_last_bk_set_update_seq_no >= seq_no` the answer is
    /// `AlreadyApplied`: a pending rotation at or below that cursor is
    /// dropped, a later one is left in place.
    ///
    /// `AckTooEarly` puts the rotation back. The driver contract is to
    /// re-ack the same artifacts once the cursor has passed N, not to
    /// build the rotation again.
    pub async fn ack_last_bk_update(&self, seq_no: u64) -> Result<AckOutcome, RelayerError> {
        self.do_ack_bk_update(seq_no).await
    }

    async fn do_ack_bundle(&self, seq_no: u64) -> Result<(), RelayerError> {
        let mut pending = self.pending_bundle.lock().await;
        let Some(b) = pending.as_ref() else {
            return Err(RelayerError::other("no pending bundle to ack"));
        };
        if b.block_seq_no != seq_no {
            return Err(RelayerError::other(format!(
                "pending bundle seq_no {} != ack seq_no {}",
                b.block_seq_no, seq_no
            )));
        }
        let b = pending.take().expect("matched pending bundle");
        drop(pending);
        let mut d = self.live().lock().await;
        d.ack_bundle(&b).map_err(map_driver_err)?;
        persist_driver(&d, &self.state_paths)?;
        Ok(())
    }

    async fn stored_last_bk_seq(&self) -> u64 {
        #[cfg(test)]
        if let Some(stub) = self.stub.lock().await.as_ref() {
            return stub.last_bk_seq;
        }
        self.live()
            .lock()
            .await
            .snapshot_state()
            .stored_last_bk_set_update_seq_no
    }

    async fn ack_bk_on_driver(&self, rotation: &PendingRotation) -> Result<(), RelayerError> {
        #[cfg(test)]
        if let Some(stub) = self.stub.lock().await.as_mut() {
            return match stub.kind {
                AckStubKind::Accept => {
                    stub.last_bk_seq = rotation.seq_no();
                    Ok(())
                },
                AckStubKind::TooEarly => Err(map_driver_err(DriverError::AckTooEarly {
                    cursor: 0,
                    rotation_seqno: rotation.seq_no(),
                })),
                AckStubKind::Other => Err(map_driver_err(DriverError::Other(anyhow::anyhow!(
                    "bk ack failed"
                )))),
            };
        }
        let mut d = self.live().lock().await;
        match rotation {
            PendingRotation::Proven(u) => d.ack_bk_update(u),
            PendingRotation::Applied(delta) => d.ack_bk_delta(delta),
        }
        .map_err(map_driver_err)?;
        persist_driver(&d, &self.state_paths)?;
        Ok(())
    }

    async fn do_ack_bk_update(&self, seq_no: u64) -> Result<AckOutcome, RelayerError> {
        let stored = self.stored_last_bk_seq().await;
        if stored >= seq_no {
            let mut pending = self.pending_bk_update.lock().await;
            if pending.as_ref().is_some_and(|p| p.seq_no() <= stored) {
                *pending = None;
            }
            return Ok(AckOutcome::AlreadyApplied);
        }
        let rotation = {
            let mut pending = self.pending_bk_update.lock().await;
            if !pending.as_ref().is_some_and(|p| p.seq_no() == seq_no) {
                return Ok(AckOutcome::NothingPending);
            }
            pending.take().expect("matched pending")
        };
        match self.ack_bk_on_driver(&rotation).await {
            Ok(()) => Ok(AckOutcome::Acked),
            // A proof is already built. Dropping it here makes the next
            // tick call `poll_next_bk_update` and prove the same rotation
            // again. `AckTooEarly` is retryable against these artifacts.
            Err(e) if e.is_rotation_ack_waiting() => {
                *self.pending_bk_update.lock().await = Some(rotation);
                Err(e)
            },
            Err(e) => Err(e),
        }
    }

    async fn snapshot_bridge_state(&self) -> BridgeState {
        self.live().lock().await.snapshot_state().clone()
    }
}

fn persist_driver(d: &LiveProverDriver, paths: &StatePaths) -> Result<(), RelayerError> {
    persist_bk_pair(paths, d.snapshot_prover_bk_set(), Some(d.snapshot_state()))
}

/// Write `prover_bk_set.json`, then `prover_state.json` when `state` is
/// given, each atomically (tmp + rename). Every place that writes both
/// writes them in this order.
pub fn persist_bk_pair(
    paths: &StatePaths,
    set: &ProverBkSet,
    state: Option<&BridgeState>,
) -> Result<(), RelayerError> {
    persist_bk_pair_with(paths, set, state, |_| {})
}

/// [`persist_bk_pair`], reporting each file once it is written:
/// `"prover_bk_set"`, then `"prover_state"`.
pub(crate) fn persist_bk_pair_with(
    paths: &StatePaths,
    set: &ProverBkSet,
    state: Option<&BridgeState>,
    mut on_step: impl FnMut(&'static str),
) -> Result<(), RelayerError> {
    let bk_path = paths
        .prover_bk_set_json
        .to_str()
        .ok_or_else(|| RelayerError::other("prover_bk_set path is not UTF-8"))?;
    let state_path = paths
        .prover_state_json
        .to_str()
        .ok_or_else(|| RelayerError::other("prover_state path is not UTF-8"))?;
    set.save(bk_path)
        .map_err(|e| RelayerError::other(format!("save prover_bk_set: {e}")))?;
    on_step("prover_bk_set");
    if let Some(state) = state {
        state
            .save(state_path)
            .map_err(|e| RelayerError::other(format!("save prover_state: {e}")))?;
        on_step("prover_state");
    }
    Ok(())
}

/// Anyhow payloads are printed with `{:#}`, so the message keeps every
/// cause, not only the outermost context.
fn map_driver_err(e: DriverError) -> RelayerError {
    match e {
        DriverError::GqlTransient(inner) => RelayerError::AckiNacki(format!("{inner:#}")),
        DriverError::Bootstrapping {
            seed_seqno,
            chain_head_seqno,
            source,
        } => RelayerError::AckiNacki(format!(
            "bootstrap seed={seed_seqno} head={chain_head_seqno}: {source:#}"
        )),
        DriverError::GqlSchema(inner) => RelayerError::AckiNacki(format!("gql schema: {inner:#}")),
        DriverError::ProofGen {
            seq_no,
            source,
        } => RelayerError::Other(format!("proof-gen failed for {seq_no}: {source:#}")),
        DriverError::StateInconsistent(inner) => {
            RelayerError::Other(format!("driver state inconsistent: {inner:#}"))
        },
        DriverError::AckTooEarly {
            cursor,
            rotation_seqno,
        } => RelayerError::AckiNacki(format!(
            "ack_bk_update deferred: cursor {cursor} + stride ≤ rotation {rotation_seqno}; retry \
             after the next bundle ack"
        )),
        DriverError::Other(inner) => RelayerError::Other(format!("{inner:#}")),
    }
}

#[async_trait]
impl BlockSource for LiveBlockSource {
    async fn fetch(&self, target: u64) -> Result<Option<AnBlockData>, RelayerError> {
        // Crash recovery: re-hand a pending bundle that was never acked.
        {
            let mut pending = self.pending_bundle.lock().await;
            if let Some(p) = pending.as_ref() {
                if p.block_seq_no >= target {
                    return Ok(Some(AnBlockData::from(p)));
                }
                warn!(
                    pending_seq_no = p.block_seq_no,
                    target, "stale pending bundle; dropping without ack — driver will re-poll",
                );
                *pending = None;
            }
        }

        let mut d = self.live().lock().await;
        match d.poll_next_bundle().await.map_err(map_driver_err)? {
            LiveBundleEvent::Bootstrapping {
                ..
            } => Ok(None),
            // No boundary past the bridge yet. A head below the bridge's
            // last verified block is a restarted or different network, not
            // a wait: the guard stops the daemon if it stays there.
            LiveBundleEvent::Nothing {
                chain_head_seqno, ..
            } => {
                let last_verified = d.snapshot_state().stored_last_seen_block_seq_no;
                self.chain_behind
                    .lock()
                    .await
                    .observe(chain_head_seqno, last_verified)?;
                Ok(None)
            },
            LiveBundleEvent::Bundle(b) => {
                if b.block_seq_no < target {
                    warn!(
                        driver_seq_no = b.block_seq_no,
                        target, "driver produced pre-target bundle — acking + dropping",
                    );
                    d.ack_bundle(&b).map_err(map_driver_err)?;
                    persist_driver(&d, &self.state_paths)?;
                    return Ok(None);
                }
                // A bundle past the bridge: the chain is not behind it.
                self.chain_behind.lock().await.reset();
                let data = AnBlockData::from(&b);
                *self.pending_bundle.lock().await = Some(b);
                Ok(Some(data))
            },
        }
    }

    async fn ack_last_bundle(&self, seq_no: u64) -> Result<(), RelayerError> {
        self.do_ack_bundle(seq_no).await
    }

    async fn driver_snapshot(&self) -> Option<BridgeState> {
        Some(self.snapshot_bridge_state().await)
    }
}

#[async_trait]
impl BkUpdateSource for LiveBlockSource {
    async fn fetch_bk_update(&self, target: u64) -> Result<Option<BkSetUpdateData>, RelayerError> {
        {
            let pending = self.pending_bk_update.lock().await;
            match pending.as_ref() {
                Some(PendingRotation::Proven(p)) if p.block_seq_no >= target => {
                    return Ok(Some(BkSetUpdateData::from(p)));
                },
                // Already applied on-chain (target is last_bk+1) and
                // waiting for its bundle gate before the prover takes it.
                // Keep it; an applied one is never served for submission.
                Some(_) => return Ok(None),
                None => {},
            }
        }

        #[cfg(test)]
        if self.stub.lock().await.is_some() {
            *self.poll_bk_calls.lock().await += 1;
            return Err(RelayerError::other(
                "poll_next_bk_update called while the pending rotation should have been reused",
            ));
        }

        // Rotations up to `target - 1`, the contract's last one, come back
        // as checked deltas; only one above it is proven.
        let mut d = self.live().lock().await;
        match d
            .poll_next_bk_update(target.saturating_sub(1))
            .await
            .map_err(map_driver_err)?
        {
            LiveBkUpdateEvent::Bootstrapping {
                ..
            }
            | LiveBkUpdateEvent::Nothing => Ok(None),
            LiveBkUpdateEvent::Applied(delta) => {
                *self.pending_bk_update.lock().await = Some(PendingRotation::Applied(delta));
                Ok(None)
            },
            LiveBkUpdateEvent::BkUpdate(u) => {
                if u.block_seq_no < target {
                    // A rotation below the target needs no proof: store it as
                    // `Proven` so the relayer acks it, and serve nothing.
                    *self.pending_bk_update.lock().await = Some(PendingRotation::Proven(u));
                    return Ok(None);
                }
                let data = BkSetUpdateData::from(&u);
                *self.pending_bk_update.lock().await = Some(PendingRotation::Proven(u));
                Ok(Some(data))
            },
        }
    }

    async fn next_applied_rotation(
        &self,
        applied_up_to: u64,
    ) -> Result<Option<AppliedRotation>, RelayerError> {
        {
            let pending = self.pending_bk_update.lock().await;
            if let Some(p) = pending.as_ref() {
                // A proven one at or below the contract's N is ours, applied
                // and waiting for its gate. One above it is still to submit.
                return Ok((p.seq_no() <= applied_up_to).then(|| p.rotation()));
            }
        }

        #[cfg(test)]
        if self.stub.lock().await.is_some() {
            *self.poll_bk_calls.lock().await += 1;
            return Err(RelayerError::other(
                "poll_next_applied_bk_update called while a pending rotation should have answered",
            ));
        }

        let mut d = self.live().lock().await;
        let Some(delta) = d
            .poll_next_applied_bk_update(applied_up_to)
            .await
            .map_err(map_driver_err)?
        else {
            return Ok(None);
        };
        // Pending until the ack, or the ack would find nothing to take.
        let pending = PendingRotation::Applied(delta);
        let rotation = pending.rotation();
        *self.pending_bk_update.lock().await = Some(pending);
        Ok(Some(rotation))
    }

    async fn ack_last_bk_update(&self, seq_no: u64) -> Result<AckOutcome, RelayerError> {
        self.do_ack_bk_update(seq_no).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use alloy::primitives::U256;
    use anyhow::Context;
    use bridge_prover_lib::{
        bridge_state::BridgeState,
        live_driver::{BkUpdateDelta, BkUpdateProofArtifacts, BundleFinalizationType, DriverError},
        prover_bk_set::ProverBkSet,
        transcript::TranscriptKind,
    };

    use super::{
        map_driver_err, persist_bk_pair_with, AckStubKind, LiveBlockSource, PendingRotation,
        StatePaths,
    };
    use crate::{
        bridge::BridgeOnChainState,
        source::{AckOutcome, AppliedRotation, BkUpdateSource},
        types::BkSetUpdateData,
    };

    fn rotation(seq: u64) -> BkUpdateProofArtifacts {
        BkUpdateProofArtifacts {
            block_seq_no: seq,
            block_height: seq,
            last_seen_bk_update_seq_no: 0,
            block_id_be: [0; 32],
            fin_type: BundleFinalizationType::Primary,
            old_bk_set_commitment_be: [0; 32],
            new_bk_set_commitment_be: [1; 32],
            merkle_sibling_h01_be: [0; 32],
            merkle_sibling_h4_7_be: [0; 32],
            merkle_sibling_h8_15_be: [0; 32],
            transcript_kind: TranscriptKind::Blake2b,
            attestation_proof: vec![9, 9, 9],
            new_pubkeys: HashMap::new(),
            attestation_proof_gen_ms: 0,
        }
    }

    /// A catch-up tick that is told `AckTooEarly` must keep the proof it
    /// already built. The next fetch holds that slot and does not poll
    /// the driver, which is what would rebuild the rotation.
    #[tokio::test]
    async fn deferred_rotation_ack_keeps_the_proof_for_the_next_tick() {
        let src = LiveBlockSource::stubbed(AckStubKind::TooEarly);
        let proved = rotation(1_890_758);
        *src.pending_bk_update.lock().await = Some(PendingRotation::Proven(proved.clone()));

        let err = src
            .ack_last_bk_update(proved.block_seq_no)
            .await
            .unwrap_err();
        assert!(err.is_rotation_ack_waiting(), "{err}");

        let kept = src
            .peek_pending_bk_update()
            .await
            .expect("proof still pending");
        assert_eq!(kept.block_seq_no, proved.block_seq_no);
        assert_eq!(kept.attestation_proof, proved.attestation_proof);

        let held = src.fetch_bk_update(proved.block_seq_no + 1).await.unwrap();
        assert!(
            held.is_none(),
            "a rotation waiting on the cursor is not served again"
        );
        assert_eq!(*src.poll_bk_calls.lock().await, 0);
        assert!(src.peek_pending_bk_update().await.is_some());

        let err = src
            .ack_last_bk_update(proved.block_seq_no)
            .await
            .unwrap_err();
        assert!(err.is_rotation_ack_waiting(), "{err}");
    }

    /// Any other ack failure still drops the slot, so the next tick polls.
    #[tokio::test]
    async fn other_rotation_ack_failure_drops_the_pending_proof() {
        let src = LiveBlockSource::stubbed(AckStubKind::Other);
        *src.pending_bk_update.lock().await = Some(PendingRotation::Proven(rotation(100)));

        let err = src.ack_last_bk_update(100).await.unwrap_err();
        assert!(!err.is_rotation_ack_waiting(), "{err}");
        assert!(src.peek_pending_bk_update().await.is_none());

        let err = src.fetch_bk_update(100).await.unwrap_err();
        assert!(err.to_string().contains("poll_next_bk_update"), "{err}");
        assert_eq!(*src.poll_bk_calls.lock().await, 1);
    }

    fn delta(seq: u64, old: U256, new: U256) -> BkUpdateDelta {
        BkUpdateDelta {
            block_seq_no: seq,
            block_height: seq,
            old_bk_set_commitment_be: old.to_le_bytes(),
            new_bk_set_commitment_be: new.to_le_bytes(),
            new_pubkeys: HashMap::new(),
        }
    }

    /// The driver keeps commitments as `Fr::to_repr()` bytes; the contract
    /// stores the field element. A delta built from what the contract stores
    /// for N must read back as exactly that, from either kind of pending
    /// rotation, and agree with what a proven one would submit.
    #[tokio::test]
    async fn an_applied_rotation_reads_the_commitments_the_contract_stores() {
        let on_chain = BridgeOnChainState {
            last_seen_block_seq_no: 16_384,
            bk_set_commitment: U256::from(0xC1C1_0102_0304u64),
            prev_bk_set_commitment: U256::from(0xC0C0_0506_0708u64),
            prev_max_level_layer_hash: U256::ZERO,
            last_bk_set_update_seq_no: 20_000,
        };
        let want = AppliedRotation {
            seq_no: 20_000,
            old_commitment: on_chain.prev_bk_set_commitment,
            new_commitment: on_chain.bk_set_commitment,
        };

        let src = LiveBlockSource::stubbed(AckStubKind::Accept);
        *src.pending_bk_update.lock().await = Some(PendingRotation::Applied(delta(
            20_000,
            on_chain.prev_bk_set_commitment,
            on_chain.bk_set_commitment,
        )));
        assert_eq!(src.next_applied_rotation(20_000).await.unwrap(), Some(want));

        let mut proved = rotation(20_000);
        proved.old_bk_set_commitment_be = on_chain.prev_bk_set_commitment.to_le_bytes();
        proved.new_bk_set_commitment_be = on_chain.bk_set_commitment.to_le_bytes();
        let submitted = BkSetUpdateData::from(&proved);
        assert_eq!(submitted.old_commitment_l2, want.old_commitment);
        assert_eq!(submitted.new_commitment_l3, want.new_commitment);
        *src.pending_bk_update.lock().await = Some(PendingRotation::Proven(proved));
        assert_eq!(src.next_applied_rotation(20_000).await.unwrap(), Some(want));
    }

    #[tokio::test]
    async fn rotation_acks_tell_taken_from_already_applied_from_nothing_pending() {
        let src = LiveBlockSource::stubbed(AckStubKind::Accept);
        assert_eq!(
            src.ack_last_bk_update(20_000).await.unwrap(),
            AckOutcome::NothingPending
        );

        // A pending rotation at another height is neither taken nor dropped.
        *src.pending_bk_update.lock().await = Some(PendingRotation::Applied(delta(
            20_000,
            U256::from(1u64),
            U256::from(2u64),
        )));
        assert_eq!(
            src.ack_last_bk_update(18_000).await.unwrap(),
            AckOutcome::NothingPending
        );
        assert_eq!(
            src.next_applied_rotation(20_000)
                .await
                .unwrap()
                .unwrap()
                .seq_no,
            20_000
        );

        assert_eq!(
            src.ack_last_bk_update(20_000).await.unwrap(),
            AckOutcome::Acked
        );
        assert!(src.pending_bk_update.lock().await.is_none());
        assert_eq!(
            src.ack_last_bk_update(20_000).await.unwrap(),
            AckOutcome::AlreadyApplied
        );
        assert_eq!(*src.poll_bk_calls.lock().await, 0);
    }

    #[tokio::test]
    async fn an_applied_rotation_is_never_served_for_submission() {
        let src = LiveBlockSource::stubbed(AckStubKind::Accept);
        *src.pending_bk_update.lock().await = Some(PendingRotation::Applied(delta(
            20_000,
            U256::from(1u64),
            U256::from(2u64),
        )));
        assert!(src.fetch_bk_update(20_000).await.unwrap().is_none());
        assert!(src.fetch_bk_update(20_001).await.unwrap().is_none());
        assert!(src.peek_pending_bk_update().await.is_none());

        // A proven rotation above the contract's N is still to submit, so it
        // is not offered as applied; neither call reaches the driver.
        *src.pending_bk_update.lock().await = Some(PendingRotation::Proven(rotation(30_000)));
        assert_eq!(src.next_applied_rotation(20_000).await.unwrap(), None);
        assert!(src.fetch_bk_update(20_001).await.unwrap().is_some());
        assert_eq!(*src.poll_bk_calls.lock().await, 0);
    }

    /// The operator needs the cause, not only the outermost context.
    #[test]
    fn mapped_driver_errors_keep_the_cause() {
        fn chained() -> anyhow::Error {
            Err::<(), _>(anyhow::anyhow!("connection refused"))
                .context("bk-update 20000: GQL block fetch")
                .unwrap_err()
        }
        let mapped = [
            map_driver_err(DriverError::GqlTransient(chained())),
            map_driver_err(DriverError::GqlSchema(chained())),
            map_driver_err(DriverError::ProofGen {
                seq_no: 20_000,
                source: chained(),
            }),
            map_driver_err(DriverError::StateInconsistent(chained())),
            map_driver_err(DriverError::Bootstrapping {
                seed_seqno: 16_384,
                chain_head_seqno: 100,
                source: chained(),
            }),
            map_driver_err(DriverError::Other(chained())),
        ];
        for err in mapped {
            let msg = err.to_string();
            assert!(
                msg.contains("bk-update 20000: GQL block fetch")
                    && msg.contains("connection refused"),
                "{msg}"
            );
        }
    }

    #[test]
    fn pair_is_written_set_first() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StatePaths::under(dir.path());
        let set = ProverBkSet {
            commitment: [7; 32],
            pubkeys_hex: BTreeMap::new(),
            last_applied_update_seq_no: 20_000,
        };
        let mut state = BridgeState::new(4);
        state.stored_last_bk_set_update_seq_no = 20_000;

        let mut log = Vec::new();
        persist_bk_pair_with(&paths, &set, Some(&state), |step| {
            // What is on disk when each step reports: the set lands alone.
            if step == "prover_bk_set" {
                assert!(paths.prover_bk_set_json.exists());
                assert!(!paths.prover_state_json.exists());
            }
            log.push(step);
        })
        .unwrap();
        assert_eq!(log, ["prover_bk_set", "prover_state"]);

        let saved = ProverBkSet::load(paths.prover_bk_set_json.to_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(saved.commitment, set.commitment);
        let saved = BridgeState::load(paths.prover_state_json.to_str().unwrap(), 4).unwrap();
        assert_eq!(saved.stored_last_bk_set_update_seq_no, 20_000);
    }
}
