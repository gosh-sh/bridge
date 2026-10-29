//! Live AN→ETH block / BK-update source over `bridge_prover_lib::live_driver`.
//!
//! Wraps [`LiveProverDriver`] behind [`BlockSource`] + [`BkUpdateSource`] with
//! **ack-after-submit** for bundles: `fetch` stashes a pending payload;
//! [`Relayer::tick`](crate::relayer::Relayer::tick) calls
//! [`LiveBlockSource::ack_last_bundle`] only after ETH `verifyBlock`
//! accepts the tx. Rotation acks are deferred until the next bundle
//! target is above N, so [`ack_last_bk_update`] may run many ticks after
//! `applyBkSetUpdate` (and is a no-op if that N is already in the
//! driver). See Alina's live-integration plan §4.2 / §5.

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use bridge_prover_lib::{
    bridge_state::BridgeState,
    live_driver::{
        BkUpdateProofArtifacts, BundleProofArtifacts, DriverError, LiveBkUpdateEvent,
        LiveBundleEvent, LiveProverDriver,
    },
    prover_bk_set::ProverBkSet,
};
use tokio::sync::Mutex;
use tracing::warn;

use crate::{
    an_chain_guard::ChainBehindGuard,
    error::RelayerError,
    source::{BkUpdateSource, BlockSource},
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

/// What a test driver answers to `ack_bk_update`.
#[cfg(test)]
#[derive(Clone, Copy)]
enum AckStubKind {
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
    pending_bk_update: Mutex<Option<BkUpdateProofArtifacts>>,
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
    pub async fn peek_pending_bk_update(&self) -> Option<BkUpdateProofArtifacts> {
        self.pending_bk_update.lock().await.clone()
    }

    /// Advance the driver cursor after ETH `verifyBlock` succeeded.
    pub async fn ack_last_bundle(&self, seq_no: u64) -> Result<(), RelayerError> {
        self.do_ack_bundle(seq_no).await
    }

    /// Advance the driver after the next bundle target is above N.
    /// Takes the pending update only on an exact seq match. If the
    /// driver already has `stored_last_bk_set_update_seq_no >= seq_no`,
    /// the ack is a no-op and a different pending N is left in place.
    ///
    /// `AckTooEarly` puts that proof back. The driver contract is to
    /// re-ack the same artifacts once the cursor has passed N, not to
    /// build the rotation again.
    pub async fn ack_last_bk_update(&self, seq_no: u64) -> Result<(), RelayerError> {
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

    async fn ack_bk_on_driver(&self, u: &BkUpdateProofArtifacts) -> Result<(), RelayerError> {
        #[cfg(test)]
        if let Some(kind) = self.stub.lock().await.as_ref().map(|stub| stub.kind) {
            return match kind {
                AckStubKind::TooEarly => Err(map_driver_err(DriverError::AckTooEarly {
                    cursor: 0,
                    rotation_seqno: u.block_seq_no,
                })),
                AckStubKind::Other => Err(map_driver_err(DriverError::Other(anyhow::anyhow!(
                    "bk ack failed"
                )))),
            };
        }
        let mut d = self.live().lock().await;
        d.ack_bk_update(u).map_err(map_driver_err)?;
        persist_driver(&d, &self.state_paths)?;
        Ok(())
    }

    async fn do_ack_bk_update(&self, seq_no: u64) -> Result<(), RelayerError> {
        if self.stored_last_bk_seq().await >= seq_no {
            return Ok(());
        }
        let u = {
            let mut pending = self.pending_bk_update.lock().await;
            let matches = pending.as_ref().is_some_and(|u| u.block_seq_no == seq_no);
            if !matches {
                return Ok(());
            }
            pending.take().expect("matched pending")
        };
        match self.ack_bk_on_driver(&u).await {
            Ok(()) => Ok(()),
            // The proof is already built. Dropping it here makes the next
            // tick call `poll_next_bk_update` and prove the same rotation
            // again. `AckTooEarly` is retryable against these artifacts.
            Err(e) if e.is_rotation_ack_waiting() => {
                *self.pending_bk_update.lock().await = Some(u);
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
    let state_path = paths
        .prover_state_json
        .to_str()
        .ok_or_else(|| RelayerError::other("prover_state path is not UTF-8"))?;
    let bk_path = paths
        .prover_bk_set_json
        .to_str()
        .ok_or_else(|| RelayerError::other("prover_bk_set path is not UTF-8"))?;
    d.snapshot_state()
        .save(state_path)
        .map_err(|e| RelayerError::other(format!("save prover_state: {e}")))?;
    d.snapshot_prover_bk_set()
        .save(bk_path)
        .map_err(|e| RelayerError::other(format!("save prover_bk_set: {e}")))?;
    Ok(())
}

fn map_driver_err(e: DriverError) -> RelayerError {
    match e {
        DriverError::GqlTransient(inner) => RelayerError::AckiNacki(inner.to_string()),
        DriverError::Bootstrapping {
            seed_seqno,
            chain_head_seqno,
            source,
        } => RelayerError::AckiNacki(format!(
            "bootstrap seed={seed_seqno} head={chain_head_seqno}: {source}"
        )),
        DriverError::GqlSchema(inner) => RelayerError::AckiNacki(format!("gql schema: {inner}")),
        DriverError::ProofGen {
            seq_no,
            source,
        } => RelayerError::Other(format!("proof-gen failed for {seq_no}: {source}")),
        DriverError::StateInconsistent(inner) => {
            RelayerError::Other(format!("driver state inconsistent: {inner}"))
        },
        DriverError::AckTooEarly {
            cursor,
            rotation_seqno,
        } => RelayerError::AckiNacki(format!(
            "ack_bk_update deferred: cursor {cursor} + stride ≤ rotation {rotation_seqno}; retry \
             after the next bundle ack"
        )),
        DriverError::Other(inner) => RelayerError::Other(inner.to_string()),
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
            if let Some(p) = pending.as_ref() {
                if p.block_seq_no >= target {
                    return Ok(Some(BkSetUpdateData::from(p)));
                }
                // Already applied on-chain (target is last_bk+1) and
                // waiting for the next bundle to pass N before we ack
                // the prover. Keep the artifacts.
                return Ok(None);
            }
        }

        #[cfg(test)]
        if self.stub.lock().await.is_some() {
            *self.poll_bk_calls.lock().await += 1;
            return Err(RelayerError::other(
                "poll_next_bk_update called while the pending rotation should have been reused",
            ));
        }

        let mut d = self.live().lock().await;
        match d.poll_next_bk_update().await.map_err(map_driver_err)? {
            LiveBkUpdateEvent::Bootstrapping {
                ..
            }
            | LiveBkUpdateEvent::Nothing => Ok(None),
            LiveBkUpdateEvent::BkUpdate(u) => {
                if u.block_seq_no < target {
                    // Re-discovered after a restart, or applied on-chain
                    // while the prover is still on the outgoing set.
                    // Do not rotate the prover here.
                    *self.pending_bk_update.lock().await = Some(u);
                    return Ok(None);
                }
                let data = BkSetUpdateData::from(&u);
                *self.pending_bk_update.lock().await = Some(u);
                Ok(Some(data))
            },
        }
    }

    async fn ack_last_bk_update(&self, seq_no: u64) -> Result<(), RelayerError> {
        self.do_ack_bk_update(seq_no).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use bridge_prover_lib::{
        live_driver::{BkUpdateProofArtifacts, BundleFinalizationType},
        transcript::TranscriptKind,
    };

    use super::{AckStubKind, LiveBlockSource};
    use crate::source::BkUpdateSource;

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
        *src.pending_bk_update.lock().await = Some(proved.clone());

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
        *src.pending_bk_update.lock().await = Some(rotation(100));

        let err = src.ack_last_bk_update(100).await.unwrap_err();
        assert!(!err.is_rotation_ack_waiting(), "{err}");
        assert!(src.peek_pending_bk_update().await.is_none());

        let err = src.fetch_bk_update(100).await.unwrap_err();
        assert!(err.to_string().contains("poll_next_bk_update"), "{err}");
        assert_eq!(*src.poll_bk_calls.lock().await, 1);
    }
}
