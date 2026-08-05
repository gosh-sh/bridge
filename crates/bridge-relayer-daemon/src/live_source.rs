//! Live AN→ETH block / BK-update source over `bridge_prover_lib::live_driver`.
//!
//! Wraps [`LiveProverDriver`] behind [`BlockSource`] + [`BkUpdateSource`] with
//! **ack-after-submit** semantics: `fetch` / `fetch_bk_update` stash a pending
//! payload; [`Relayer::tick`](crate::relayer::Relayer::tick) calls
//! [`LiveBlockSource::ack_last_bundle`] / [`ack_last_bk_update`] only after the
//! ETH bridge accepts the tx. See Alina's live-integration plan §4.2 / §5.

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

/// Live source sharing one [`LiveProverDriver`] across both lanes.
pub struct LiveBlockSource {
    driver: Arc<Mutex<LiveProverDriver>>,
    pending_bundle: Mutex<Option<BundleProofArtifacts>>,
    pending_bk_update: Mutex<Option<BkUpdateProofArtifacts>>,
    state_paths: StatePaths,
}

impl LiveBlockSource {
    pub fn new(driver: Arc<Mutex<LiveProverDriver>>, state_paths: StatePaths) -> Self {
        Self {
            driver,
            pending_bundle: Mutex::new(None),
            pending_bk_update: Mutex::new(None),
            state_paths,
        }
    }

    pub fn driver(&self) -> Arc<Mutex<LiveProverDriver>> {
        Arc::clone(&self.driver)
    }

    /// Read-only clone of the driver's post-ack `BridgeState` (~40 KiB).
    pub async fn driver_snapshot(&self) -> BridgeState {
        self.snapshot_bridge_state().await
    }

    pub async fn driver_prover_bk_set_snapshot(&self) -> ProverBkSet {
        self.driver.lock().await.snapshot_prover_bk_set().clone()
    }

    /// Read-only clone of the pending bundle artifact, if any. Used by the
    /// aggregation wrapper ([`crate::aggregated_source::AggregatedBlockSource`])
    /// to obtain the raw `BundleProofArtifacts` — including
    /// `last_seen_block_seq_no` and the Blake2b proof bytes — that the shape-
    /// preserving `AnBlockData::from` conversion drops. This is a peek: it does
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

    /// Advance the driver cursor after ETH `applyBkSetUpdate` succeeded.
    pub async fn ack_last_bk_update(&self, seq_no: u64) -> Result<(), RelayerError> {
        self.do_ack_bk_update(seq_no).await
    }

    async fn do_ack_bundle(&self, seq_no: u64) -> Result<(), RelayerError> {
        let pending = self.pending_bundle.lock().await.take();
        let Some(b) = pending else {
            return Err(RelayerError::other("no pending bundle to ack"));
        };
        if b.block_seq_no != seq_no {
            return Err(RelayerError::other(format!(
                "pending bundle seq_no {} != ack seq_no {}",
                b.block_seq_no, seq_no
            )));
        }
        let mut d = self.driver.lock().await;
        d.ack_bundle(&b).map_err(map_driver_err)?;
        persist_driver(&d, &self.state_paths)?;
        Ok(())
    }

    async fn do_ack_bk_update(&self, seq_no: u64) -> Result<(), RelayerError> {
        let pending = self.pending_bk_update.lock().await.take();
        let Some(u) = pending else {
            return Err(RelayerError::other("no pending bk-update to ack"));
        };
        if u.block_seq_no != seq_no {
            return Err(RelayerError::other(format!(
                "pending bk-update seq_no {} != ack seq_no {}",
                u.block_seq_no, seq_no
            )));
        }
        let mut d = self.driver.lock().await;
        d.ack_bk_update(&u).map_err(map_driver_err)?;
        persist_driver(&d, &self.state_paths)?;
        Ok(())
    }

    async fn snapshot_bridge_state(&self) -> BridgeState {
        self.driver.lock().await.snapshot_state().clone()
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
        DriverError::GqlSchema(inner) => {
            RelayerError::AckiNacki(format!("gql schema: {inner}"))
        }
        DriverError::ProofGen { seq_no, source } => {
            RelayerError::Other(format!("proof-gen failed for {seq_no}: {source}"))
        }
        DriverError::StateInconsistent(inner) => {
            RelayerError::Other(format!("driver state inconsistent: {inner}"))
        }
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

        let mut d = self.driver.lock().await;
        match d.poll_next_bundle().await.map_err(map_driver_err)? {
            LiveBundleEvent::Bootstrapping { .. } | LiveBundleEvent::Nothing { .. } => Ok(None),
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
                let data = AnBlockData::from(&b);
                *self.pending_bundle.lock().await = Some(b);
                Ok(Some(data))
            }
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
    async fn fetch_bk_update(
        &self,
        target: u64,
    ) -> Result<Option<BkSetUpdateData>, RelayerError> {
        {
            let mut pending = self.pending_bk_update.lock().await;
            if let Some(p) = pending.as_ref() {
                if p.block_seq_no >= target {
                    return Ok(Some(BkSetUpdateData::from(p)));
                }
                warn!(
                    pending_seq_no = p.block_seq_no,
                    target, "stale pending bk-update; dropping without ack — driver will re-poll",
                );
                *pending = None;
            }
        }

        let mut d = self.driver.lock().await;
        match d.poll_next_bk_update().await.map_err(map_driver_err)? {
            LiveBkUpdateEvent::Bootstrapping { .. } | LiveBkUpdateEvent::Nothing => Ok(None),
            LiveBkUpdateEvent::BkUpdate(u) => {
                if u.block_seq_no < target {
                    warn!(
                        driver_seq_no = u.block_seq_no,
                        target, "driver produced pre-target bk-update — acking + dropping",
                    );
                    d.ack_bk_update(&u).map_err(map_driver_err)?;
                    persist_driver(&d, &self.state_paths)?;
                    return Ok(None);
                }
                let data = BkSetUpdateData::from(&u);
                *self.pending_bk_update.lock().await = Some(u);
                Ok(Some(data))
            }
        }
    }

    async fn ack_last_bk_update(&self, seq_no: u64) -> Result<(), RelayerError> {
        self.do_ack_bk_update(seq_no).await
    }
}
