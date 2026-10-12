//! One-tick loop: fetch beacon head → prove step (or demand rotate) → submit.

use std::{path::PathBuf, sync::Arc, time::Duration};

use tracing::{info, warn};

use crate::{
    error::RelayerError,
    prover::ProofGenerator,
    source::{BeaconSource, ExecutionSource},
    state::RelayerState,
    submitter::{AnSubmitter, SubmitOutcome},
    types::{FinalityUpdate, SLOTS_PER_SYNC_PERIOD},
};

#[derive(Clone, Debug)]
pub struct RelayerConfig {
    pub state_path: PathBuf,
    pub poll_interval: Duration,
    /// When the beacon period is ahead of `last_committee_period`, attempt
    /// `generate_rotate` + `submitRotate`. Default **true**: tvm-sdk#284
    /// (KZG accumulator decider) co-deploys with this contract. `--no-rotate`
    /// / `enable_rotate = false` returns [`TickOutcome::RotateRequired`]
    /// instead. Rotate prove is still a ~40 GB n14 job.
    pub enable_rotate: bool,
    /// After the first accepted `submitUpdate`, call `disableOwnerAnchors` +
    /// `disableOwnerRotation` with the relayer keys (must be the owner
    /// pubkey). Default **false**; `--flip-owner` opts in. While the light
    /// client anchors only one checkpoint block per epoch and no L2, the flip
    /// leaves every other deposit block without a writer.
    pub flip_owner: bool,
    /// With `enable_rotate = false`: on a period jump, prove a step of the
    /// new period and advance the committee with the owner key
    /// (`setCommitteeCommitment`) instead of stopping at
    /// [`TickOutcome::RotateRequired`]. A shadow-only convenience; the
    /// contract refuses it after `disableOwnerRotation`. Default **false**.
    pub owner_hop: bool,
    /// Call `submitAncestry` after each accepted checkpoint. Default
    /// **false**. The call needs a proven anchor at both ends of the span, so
    /// it sends nothing until a previous checkpoint is on record — the state
    /// file carries one across a restart — and nothing when the gap to it is
    /// wider than a span. `ETH_RPC_URL` still attaches an execution source so
    /// the daemon can run `link_headers` locally without sending the tx.
    pub submit_ancestry: bool,
}

impl RelayerConfig {
    pub fn new(state_path: PathBuf) -> Self {
        Self {
            state_path,
            poll_interval: Duration::from_secs(64),
            enable_rotate: true,
            flip_owner: false,
            owner_hop: false,
            submit_ancestry: false,
        }
    }
}

#[derive(Clone, Debug)]
pub enum TickOutcome {
    SubmittedUpdate {
        finalized_slot: u64,
        tx_hash: Option<[u8; 32]>,
    },
    AlreadyOnHead {
        finalized_slot: u64,
    },
    Unchanged {
        finalized_slot: u64,
    },
    RotateRequired {
        from_period: u64,
        to_period: u64,
    },
    Rotated {
        period: u64,
        tx_hash: Option<[u8; 32]>,
    },
    ProofFailed {
        reason: String,
    },
    AnRejected {
        reason: String,
    },
    AnPending {
        reason: String,
    },
    WeakSubjectivityLag {
        lag_slots: u64,
        finalized_slot: u64,
    },
}

pub struct Relayer<S: BeaconSource, P: ProofGenerator, A: AnSubmitter> {
    config: RelayerConfig,
    source: Arc<S>,
    prover: Arc<P>,
    submitter: Arc<A>,
    state: RelayerState,
    /// When set, the daemon fetches the span between its previous checkpoint
    /// and the new one and calls `submitAncestry` (31/32 coverage). CLI-only
    /// without it.
    execution: Option<Arc<dyn ExecutionSource>>,
}

impl<S: BeaconSource, P: ProofGenerator, A: AnSubmitter> Relayer<S, P, A> {
    pub fn new(
        config: RelayerConfig,
        source: Arc<S>,
        prover: Arc<P>,
        submitter: Arc<A>,
    ) -> Result<Self, RelayerError> {
        let state = RelayerState::load(&config.state_path)?.unwrap_or_default();
        Ok(Self {
            config,
            source,
            prover,
            submitter,
            state,
            execution: None,
        })
    }

    pub fn with_execution(mut self, rpc: Arc<dyn ExecutionSource>) -> Self {
        self.execution = Some(rpc);
        self
    }

    pub fn state(&self) -> &RelayerState {
        &self.state
    }

    pub async fn tick(&mut self) -> Result<TickOutcome, RelayerError> {
        self.cover_pending_span().await;
        let update = self.source.fetch_finality().await?;
        self.guard_network(&update)?;
        if let Some(lag) = self.ws_lag(&update) {
            warn!(
                lag_slots = lag,
                last = ?self.state.last_finalized_slot,
                attested = update.attested_slot,
                "light-client lag approaching / past one sync-committee period (~27 h)"
            );
        }

        let period = update.period();
        if let Some(last_p) = self.state.last_committee_period {
            if period > last_p {
                return self.handle_period_jump(update, last_p, period).await;
            }
        }

        if self.state.last_finalized_slot == Some(update.finalized_slot) {
            return Ok(TickOutcome::Unchanged {
                finalized_slot: update.finalized_slot,
            });
        }

        self.submit_step(update).await
    }

    async fn handle_period_jump(
        &mut self,
        update: FinalityUpdate,
        from_period: u64,
        to_period: u64,
    ) -> Result<TickOutcome, RelayerError> {
        if !self.config.enable_rotate {
            if self.config.owner_hop {
                return self.owner_hop(update, from_period, to_period).await;
            }
            self.state.record_rotate_pending(to_period);
            self.persist()?;
            warn!(
                from_period,
                to_period,
                "committee period advanced; rotate prove is off — owner setCommitteeCommitment or \
                 prove rotate_tree_n8 on n14"
            );
            return Ok(TickOutcome::RotateRequired {
                from_period,
                to_period,
            });
        }
        let bundle = match self.prover.generate_rotate(to_period).await {
            Ok(b) => b,
            Err(e) => {
                return Ok(TickOutcome::ProofFailed {
                    reason: e.to_string(),
                });
            },
        };
        match self.submitter.submit_rotate(&bundle).await? {
            SubmitOutcome::Accepted {
                tx_hash,
            } => {
                self.state.record_rotate(to_period);
                self.persist()?;
                info!(to_period, "submitRotate accepted");
                Ok(TickOutcome::Rotated {
                    period: to_period,
                    tx_hash,
                })
            },
            SubmitOutcome::AlreadyOnHead => Ok(TickOutcome::AlreadyOnHead {
                finalized_slot: self.state.last_finalized_slot.unwrap_or(0),
            }),
            SubmitOutcome::Rejected {
                reason,
            } => Ok(TickOutcome::AnRejected {
                reason,
            }),
            SubmitOutcome::Pending {
                reason,
            } => Ok(TickOutcome::AnPending {
                reason,
            }),
        }
    }

    /// Shadow period hop: the step proof of the new period already carries the
    /// commitment of the committee that signed it (public input 5), so the
    /// owner writes that commitment for the signing period and the same
    /// bundle is submitted as the first update of the period.
    async fn owner_hop(
        &mut self,
        update: FinalityUpdate,
        from_period: u64,
        to_period: u64,
    ) -> Result<TickOutcome, RelayerError> {
        let bundle = match self.prover.generate_step(&update).await {
            Ok(b) => b,
            Err(e) => {
                return Ok(TickOutcome::ProofFailed {
                    reason: e.to_string(),
                });
            },
        };
        let period = update.signing_period();
        match self
            .submitter
            .set_committee_commitment(bundle.parsed.committee_commitment, period)
            .await?
        {
            SubmitOutcome::Accepted {
                tx_hash,
            } => {
                self.state.record_rotate(period);
                self.persist()?;
                info!(
                    from_period,
                    to_period,
                    period,
                    tx = ?tx_hash.map(hex::encode),
                    "owner hop: setCommitteeCommitment accepted (shadow, no rotate proof)"
                );
            },
            other => {
                warn!(
                    from_period,
                    to_period,
                    ?other,
                    "owner hop refused; leaving period pending"
                );
                self.state.record_rotate_pending(to_period);
                self.persist()?;
                return Ok(TickOutcome::AnRejected {
                    reason: format!("owner hop setCommitteeCommitment: {other:?}"),
                });
            },
        }
        self.submit_step_bundle(update, bundle).await
    }

    async fn submit_step(&mut self, update: FinalityUpdate) -> Result<TickOutcome, RelayerError> {
        let bundle = match self.prover.generate_step(&update).await {
            Ok(b) => b,
            Err(e) => {
                return Ok(TickOutcome::ProofFailed {
                    reason: e.to_string(),
                });
            },
        };
        self.submit_step_bundle(update, bundle).await
    }

    async fn submit_step_bundle(
        &mut self,
        update: FinalityUpdate,
        bundle: crate::types::StepProofBundle,
    ) -> Result<TickOutcome, RelayerError> {
        let prev_anchor = self
            .state
            .ancestry_anchor
            .or(self.state.last_execution_block_hash);
        match self.submitter.submit_update(&bundle).await? {
            SubmitOutcome::Accepted {
                tx_hash,
            } => {
                self.state.record_update(
                    update.finalized_slot,
                    update.period(),
                    update.execution_block_hash,
                    update.attested_slot,
                );
                self.persist()?;
                info!(slot = update.finalized_slot, "submitUpdate accepted");
                self.maybe_flip_owner().await?;
                self.maybe_cover_epoch(prev_anchor, update.execution_block_hash)
                    .await;
                Ok(TickOutcome::SubmittedUpdate {
                    finalized_slot: update.finalized_slot,
                    tx_hash,
                })
            },
            SubmitOutcome::AlreadyOnHead => {
                self.state.record_update(
                    update.finalized_slot,
                    update.period(),
                    update.execution_block_hash,
                    update.attested_slot,
                );
                self.persist()?;
                Ok(TickOutcome::AlreadyOnHead {
                    finalized_slot: update.finalized_slot,
                })
            },
            SubmitOutcome::Rejected {
                reason,
            } => Ok(TickOutcome::AnRejected {
                reason,
            }),
            SubmitOutcome::Pending {
                reason,
            } => Ok(TickOutcome::AnPending {
                reason,
            }),
        }
    }

    async fn maybe_flip_owner(&mut self) -> Result<(), RelayerError> {
        if !self.config.flip_owner || self.state.owner_flip_done {
            return Ok(());
        }
        match self.submitter.flip_owner().await? {
            SubmitOutcome::Accepted {
                tx_hash,
            } => {
                self.state.owner_flip_done = true;
                self.persist()?;
                info!(tx = ?tx_hash.map(hex::encode), "owner flip accepted (disableOwnerAnchors + disableOwnerRotation)");
            },
            SubmitOutcome::Rejected {
                reason,
            } => {
                warn!(
                    reason,
                    "owner flip rejected; will retry next accepted update"
                );
            },
            SubmitOutcome::Pending {
                reason,
            } => {
                warn!(
                    reason,
                    "owner flip pending; will retry next accepted update"
                );
            },
            other => {
                warn!(?other, "owner flip unexpected outcome");
            },
        }
        Ok(())
    }

    /// After a proven checkpoint: re-push the hash to USDCBridge (the first
    /// push often bounced because the light client was not yet the sink
    /// writer), then, when an execution RPC is configured, fetch the span
    /// between the previous anchor and this one and optionally send it on
    /// chain. `submitAncestry` needs a proven anchor at both ends, so the span
    /// only exists when the previous checkpoint is known and no more than one
    /// epoch back; a skipped epoch leaves a gap no span can bridge.
    async fn maybe_cover_epoch(&mut self, prev_anchor: Option<[u8; 32]>, checkpoint: [u8; 32]) {
        match self.submitter.re_push_anchor(checkpoint).await {
            Ok(SubmitOutcome::Accepted {
                ..
            }) => {
                info!(
                    hash = %hex::encode(checkpoint),
                    "rePushAnchor accepted"
                );
            },
            Ok(other) => {
                warn!(?other, "rePushAnchor did not accept");
            },
            Err(e) => {
                warn!(error = %e, "rePushAnchor failed");
            },
        }
        let Some(rpc) = &self.execution else {
            return;
        };
        let Some(older) = prev_anchor else {
            self.advance_ancestry_anchor(checkpoint);
            info!("no previous anchor on record; epoch ancestry needs one at each end");
            return;
        };
        if older == checkpoint {
            return;
        }
        let headers = match rpc.ancestry_span(checkpoint, older, 33).await {
            Ok(h) => h,
            Err(e) => {
                // A gap wider than one span can never be sent, so retrying it
                // every tick would only repeat the same 33 reads. Anything
                // else is worth another attempt, and the cursor stays put.
                let unreachable = e.to_string().contains("older anchor not reached");
                if unreachable {
                    self.advance_ancestry_anchor(checkpoint);
                }
                warn!(error = %e, retry = !unreachable, "epoch ancestry fetch failed");
                return;
            },
        };
        if let Err(e) = crate::header_rlp::link_headers(&headers) {
            self.advance_ancestry_anchor(checkpoint);
            warn!(error = %e, "epoch ancestry failed local link_headers");
            return;
        }
        if !self.config.submit_ancestry {
            self.advance_ancestry_anchor(checkpoint);
            info!(
                n = headers.len(),
                "epoch ancestry linked locally; on-chain submitAncestry skipped \
                 (--submit-ancestry, default off)"
            );
            return;
        }
        match self.submitter.submit_ancestry(&headers).await {
            Ok(SubmitOutcome::Accepted {
                ..
            }) => {
                self.advance_ancestry_anchor(checkpoint);
                info!(n = headers.len(), "submitAncestry accepted");
            },
            Ok(other) => {
                warn!(?other, retry = true, "submitAncestry did not accept");
            },
            Err(e) => {
                warn!(error = %e, retry = true, "submitAncestry failed");
            },
        }
    }

    /// Retry the span a previous tick could not send. Coverage is driven from
    /// here as well as from an accepted update, because the next accepted
    /// checkpoint can be two epochs away — by then the span the cursor still
    /// points at would be out of reach and both epochs would be lost.
    async fn cover_pending_span(&mut self) {
        let (Some(cursor), Some(head)) =
            (self.state.ancestry_anchor, self.state.last_execution_block_hash)
        else {
            return;
        };
        if cursor == head {
            return;
        }
        info!(
            from = %hex::encode(cursor),
            to = %hex::encode(head),
            "retrying the ancestry span the cursor is still behind"
        );
        self.maybe_cover_epoch(Some(cursor), head).await;
    }

    /// Move the span cursor up to `hash`. Called only where the span is done
    /// with — sent, not wanted, or impossible because the gap is wider than
    /// one call — so that a transient failure leaves the cursor behind the
    /// head and the next tick retries it.
    fn advance_ancestry_anchor(&mut self, hash: [u8; 32]) {
        self.state.ancestry_anchor = Some(hash);
        if let Err(e) = self.persist() {
            warn!(error = %e, "could not persist the ancestry cursor");
        }
    }

    /// Pin the state file to one beacon network. The first update with a
    /// resolved signing domain records its `genesis_validators_root`; any
    /// later update from a different network is an error, not a silent
    /// head advance with proofs the contract will reject.
    fn guard_network(&mut self, update: &FinalityUpdate) -> Result<(), RelayerError> {
        let Some(chain) = &update.chain else {
            return Ok(());
        };
        let seen = chain.genesis_validators_root_hex();
        match &self.state.genesis_validators_root {
            Some(known) if *known != seen => Err(RelayerError::other(format!(
                "beacon network mismatch: state file was built on genesis_validators_root \
                 {known}, source reports {seen}; refusing to mix networks"
            ))),
            Some(_) => Ok(()),
            None => {
                info!(
                    genesis_validators_root = %seen,
                    fork_version = %chain.fork_version_hex(),
                    "pinning state file to beacon network"
                );
                self.state.genesis_validators_root = Some(seen);
                self.persist()
            },
        }
    }

    fn ws_lag(&self, update: &FinalityUpdate) -> Option<u64> {
        let last = self.state.last_finalized_slot?;
        let lag = update.attested_slot.saturating_sub(last);
        (lag >= SLOTS_PER_SYNC_PERIOD).then_some(lag)
    }

    fn persist(&mut self) -> Result<(), RelayerError> {
        self.state.save(&self.config.state_path)
    }

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

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::{
        prover::MockProofGenerator, source::InMemoryBeaconSource, submitter::MockAnSubmitter,
        types::parse_finality_update,
    };

    fn update(attested: u64, finalized: u64) -> FinalityUpdate {
        let root = format!("0x{}", "11".repeat(32));
        let hash = {
            let b = format!("{:02x}", (finalized % 250) as u8);
            format!("0x{}{}", b, "11".repeat(31))
        };
        let bits = format!("0x{}ff", "00".repeat(63));
        let json = format!(
            r#"{{"data":{{
              "attested_header":{{"beacon":{{"slot":"{attested}","state_root":"{root}"}}}},
              "finalized_header":{{
                "beacon":{{"slot":"{finalized}","state_root":"{root}"}},
                "execution":{{"block_hash":"{hash}"}}
              }},
              "sync_aggregate":{{"sync_committee_bits":"{bits}"}}
            }}}}"#
        );
        parse_finality_update(&json).unwrap()
    }

    fn with_chain(mut u: FinalityUpdate, gvr_byte: u8) -> FinalityUpdate {
        u.chain = Some(crate::types::BeaconChainParams {
            fork_version: [0x90, 0, 0, 0x75],
            genesis_validators_root: [gvr_byte; 32],
        });
        u
    }

    #[tokio::test]
    async fn network_guard_pins_then_refuses_other_network() {
        let mut r = make_relayer(
            vec![
                with_chain(update(10, 8), 0xd8),
                with_chain(update(12, 10), 0xd8),
                with_chain(update(14, 12), 0x4b),
            ],
            true,
        );
        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::SubmittedUpdate { .. }
        ));
        assert_eq!(
            r.state().genesis_validators_root.as_deref(),
            Some(format!("0x{}", "d8".repeat(32)).as_str())
        );
        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::SubmittedUpdate { .. }
        ));
        let err = r.tick().await.unwrap_err().to_string();
        assert!(err.contains("network mismatch"), "{err}");
        // State was not advanced by the foreign update.
        assert_eq!(r.state().last_finalized_slot, Some(10));
    }

    #[tokio::test]
    async fn owner_hop_advances_committee_and_submits_first_update() {
        let dir = tempdir().unwrap();
        let mut cfg = RelayerConfig::new(dir.path().join("state.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = false;
        cfg.owner_hop = true;
        cfg.flip_owner = false;
        let submitter = Arc::new(MockAnSubmitter::accepting());
        let p = SLOTS_PER_SYNC_PERIOD;
        let mut r = Relayer::new(
            cfg,
            Arc::new(InMemoryBeaconSource::new(vec![
                update(10, 8),
                update(p + 40, p + 8),
                update(p + 72, p + 40),
            ])),
            Arc::new(MockProofGenerator::new()),
            submitter.clone(),
        )
        .unwrap();
        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::SubmittedUpdate { .. }
        ));
        assert_eq!(r.state().last_committee_period, Some(0));
        // Period jump: committee advanced by the owner, then the same bundle
        // lands.
        let out = r.tick().await.unwrap();
        assert!(
            matches!(out, TickOutcome::SubmittedUpdate { finalized_slot, .. } if finalized_slot == p + 8),
            "{out:?}"
        );
        assert_eq!(r.state().last_committee_period, Some(1));
        assert_eq!(r.state().rotate_pending_period, None);
        assert_eq!(submitter.head_slot(), Some(p + 8));
        // Next update in the same period is a plain step.
        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::SubmittedUpdate { .. }
        ));
    }

    #[tokio::test]
    async fn owner_hop_refused_after_flip_leaves_period_pending() {
        let dir = tempdir().unwrap();
        let mut cfg = RelayerConfig::new(dir.path().join("state.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = false;
        cfg.owner_hop = true;
        cfg.flip_owner = true;
        let submitter = Arc::new(MockAnSubmitter::accepting());
        let p = SLOTS_PER_SYNC_PERIOD;
        let mut r = Relayer::new(
            cfg,
            Arc::new(InMemoryBeaconSource::new(vec![
                update(10, 8),
                update(p + 40, p + 8),
            ])),
            Arc::new(MockProofGenerator::new()),
            submitter.clone(),
        )
        .unwrap();
        // First accepted update flips the owner off (disableOwnerRotation).
        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::SubmittedUpdate { .. }
        ));
        assert!(!submitter.owner_rotation_enabled());
        let out = r.tick().await.unwrap();
        assert!(matches!(out, TickOutcome::AnRejected { .. }), "{out:?}");
        assert_eq!(r.state().rotate_pending_period, Some(1));
        assert_eq!(r.state().last_committee_period, Some(0));
    }

    fn make_relayer(
        updates: Vec<FinalityUpdate>,
        enable_rotate: bool,
    ) -> Relayer<InMemoryBeaconSource, MockProofGenerator, MockAnSubmitter> {
        let dir = Box::leak(Box::new(tempdir().unwrap()));
        let mut cfg = RelayerConfig::new(dir.path().join("state.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = enable_rotate;
        Relayer::new(
            cfg,
            Arc::new(InMemoryBeaconSource::new(updates)),
            Arc::new(MockProofGenerator::new()),
            Arc::new(MockAnSubmitter::accepting()),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn first_tick_submits() {
        let mut r = make_relayer(vec![update(100, 96)], false);
        match r.tick().await.unwrap() {
            TickOutcome::SubmittedUpdate {
                finalized_slot, ..
            } => {
                assert_eq!(finalized_slot, 96);
            },
            other => panic!("{other:?}"),
        }
        assert_eq!(r.state().last_finalized_slot, Some(96));
        assert!(!r.state().owner_flip_done);
    }

    #[tokio::test]
    async fn first_tick_flips_owner_on_chain_writers() {
        let dir = Box::leak(Box::new(tempdir().unwrap()));
        let mut cfg = RelayerConfig::new(dir.path().join("state.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = false;
        cfg.flip_owner = true;
        let mock = Arc::new(MockAnSubmitter::accepting());
        let mut r = Relayer::new(
            cfg,
            Arc::new(InMemoryBeaconSource::new(vec![update(100, 96)])),
            Arc::new(MockProofGenerator::new()),
            mock.clone(),
        )
        .unwrap();
        r.tick().await.unwrap();
        assert!(r.state().owner_flip_done);
        assert!(!mock.owner_anchors_enabled());
        assert!(!mock.owner_rotation_enabled());
        assert_eq!(mock.re_push_count(), 1);
    }

    #[tokio::test]
    async fn rejected_flip_keeps_the_tick_and_retries_on_the_next_update() {
        let dir = Box::leak(Box::new(tempdir().unwrap()));
        let mut cfg = RelayerConfig::new(dir.path().join("state.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = false;
        cfg.flip_owner = true;
        let mock = Arc::new(MockAnSubmitter::rejecting_flip());
        let mut r = Relayer::new(
            cfg,
            Arc::new(InMemoryBeaconSource::new(vec![
                update(100, 96),
                update(132, 128),
            ])),
            Arc::new(MockProofGenerator::new()),
            mock.clone(),
        )
        .unwrap();
        let out = r.tick().await.unwrap();
        assert!(
            matches!(out, TickOutcome::SubmittedUpdate { .. }),
            "{out:?}"
        );
        assert!(!r.state().owner_flip_done);
        assert!(mock.owner_anchors_enabled());
        assert_eq!(mock.re_push_count(), 1);

        mock.accept_flip();
        let out = r.tick().await.unwrap();
        assert!(
            matches!(out, TickOutcome::SubmittedUpdate { .. }),
            "{out:?}"
        );
        assert!(r.state().owner_flip_done);
        assert!(!mock.owner_anchors_enabled());
        assert_eq!(mock.re_push_count(), 2);
    }

    #[tokio::test]
    async fn default_config_leaves_owner_path() {
        let dir = Box::leak(Box::new(tempdir().unwrap()));
        let mut cfg = RelayerConfig::new(dir.path().join("state.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = false;
        let mock = Arc::new(MockAnSubmitter::accepting());
        let mut r = Relayer::new(
            cfg,
            Arc::new(InMemoryBeaconSource::new(vec![update(100, 96)])),
            Arc::new(MockProofGenerator::new()),
            mock.clone(),
        )
        .unwrap();
        r.tick().await.unwrap();
        assert!(!r.state().owner_flip_done);
        assert!(mock.owner_anchors_enabled());
        assert!(mock.owner_rotation_enabled());
    }

    #[tokio::test]
    async fn second_tick_same_slot_is_unchanged() {
        let mut r = make_relayer(vec![update(100, 96)], false);
        r.tick().await.unwrap();
        match r.tick().await.unwrap() {
            TickOutcome::Unchanged {
                finalized_slot,
            } => assert_eq!(finalized_slot, 96),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn advances_to_newer_checkpoint() {
        let src = InMemoryBeaconSource::new(vec![update(100, 96), update(200, 192)]);
        let dir = tempdir().unwrap();
        let mut cfg = RelayerConfig::new(dir.path().join("s.json"));
        cfg.poll_interval = Duration::from_millis(1);
        let mut r = Relayer::new(
            cfg,
            Arc::new(src),
            Arc::new(MockProofGenerator::new()),
            Arc::new(MockAnSubmitter::accepting()),
        )
        .unwrap();
        r.tick().await.unwrap();
        match r.tick().await.unwrap() {
            TickOutcome::SubmittedUpdate {
                finalized_slot, ..
            } => {
                assert_eq!(finalized_slot, 192);
            },
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn period_jump_without_rotate_flag_blocks() {
        // period 0 (slot 96) then period 1 (slot 8192).
        let src = InMemoryBeaconSource::new(vec![update(100, 96), update(9000, 8192)]);
        let dir = tempdir().unwrap();
        let mut cfg = RelayerConfig::new(dir.path().join("s.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = false;
        let mut r = Relayer::new(
            cfg,
            Arc::new(src),
            Arc::new(MockProofGenerator::new()),
            Arc::new(MockAnSubmitter::accepting()),
        )
        .unwrap();
        r.tick().await.unwrap();
        match r.tick().await.unwrap() {
            TickOutcome::RotateRequired {
                from_period,
                to_period,
            } => {
                assert_eq!(from_period, 0);
                assert_eq!(to_period, 1);
            },
            other => panic!("{other:?}"),
        }
        assert_eq!(r.state().rotate_pending_period, Some(1));
    }

    #[tokio::test]
    async fn enable_rotate_submits_rotate() {
        let src = InMemoryBeaconSource::new(vec![update(100, 96), update(9000, 8192)]);
        let dir = tempdir().unwrap();
        let mut cfg = RelayerConfig::new(dir.path().join("s.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = true;
        let mut r = Relayer::new(
            cfg,
            Arc::new(src),
            Arc::new(MockProofGenerator::new()),
            Arc::new(MockAnSubmitter::accepting()),
        )
        .unwrap();
        r.tick().await.unwrap();
        match r.tick().await.unwrap() {
            TickOutcome::Rotated {
                period, ..
            } => assert_eq!(period, 1),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn proof_failure_is_recoverable() {
        let mut prover = MockProofGenerator::new();
        prover.fail_step = true;
        let dir = tempdir().unwrap();
        let cfg = RelayerConfig::new(dir.path().join("s.json"));
        let mut r = Relayer::new(
            cfg,
            Arc::new(InMemoryBeaconSource::new(vec![update(10, 8)])),
            Arc::new(prover),
            Arc::new(MockAnSubmitter::accepting()),
        )
        .unwrap();
        match r.tick().await.unwrap() {
            TickOutcome::ProofFailed {
                ..
            } => {},
            other => panic!("{other:?}"),
        }
        assert!(r.state().last_finalized_slot.is_none());
    }

    #[tokio::test]
    async fn an_reject_does_not_advance_cursor() {
        let dir = tempdir().unwrap();
        let cfg = RelayerConfig::new(dir.path().join("s.json"));
        let mut r = Relayer::new(
            cfg,
            Arc::new(InMemoryBeaconSource::new(vec![update(10, 8)])),
            Arc::new(MockProofGenerator::new()),
            Arc::new(MockAnSubmitter::rejecting()),
        )
        .unwrap();
        match r.tick().await.unwrap() {
            TickOutcome::AnRejected {
                reason,
            } => {
                assert!(reason.contains("ZKHALO2VERIFYWITHVK"));
            },
            other => panic!("{other:?}"),
        }
        assert!(r.state().last_finalized_slot.is_none());
    }

    #[tokio::test]
    async fn restart_resumes_unchanged() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("s.json");
        let src = Arc::new(InMemoryBeaconSource::new(vec![update(10, 8)]));
        {
            let cfg = RelayerConfig::new(path.clone());
            let mut r = Relayer::new(
                cfg,
                src.clone(),
                Arc::new(MockProofGenerator::new()),
                Arc::new(MockAnSubmitter::accepting()),
            )
            .unwrap();
            r.tick().await.unwrap();
        }
        let cfg = RelayerConfig::new(path);
        let mut r = Relayer::new(
            cfg,
            src,
            Arc::new(MockProofGenerator::new()),
            Arc::new(MockAnSubmitter::accepting()),
        )
        .unwrap();
        match r.tick().await.unwrap() {
            TickOutcome::Unchanged {
                finalized_slot,
            } => assert_eq!(finalized_slot, 8),
            other => panic!("{other:?}"),
        }
    }

    fn update_with_exec(attested: u64, finalized: u64, exec: [u8; 32]) -> FinalityUpdate {
        let root = format!("0x{}", "11".repeat(32));
        let hash = format!("0x{}", hex::encode(exec));
        let bits = format!("0x{}ff", "00".repeat(63));
        let json = format!(
            r#"{{"data":{{
              "attested_header":{{"beacon":{{"slot":"{attested}","state_root":"{root}"}}}},
              "finalized_header":{{
                "beacon":{{"slot":"{finalized}","state_root":"{root}"}},
                "execution":{{"block_hash":"{hash}"}}
              }},
              "sync_aggregate":{{"sync_committee_bits":"{bits}"}}
            }}}}"#
        );
        parse_finality_update(&json).unwrap()
    }

    #[tokio::test]
    async fn no_execution_rpc_skips_ancestry() {
        let dir = Box::leak(Box::new(tempdir().unwrap()));
        let mut cfg = RelayerConfig::new(dir.path().join("state.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = false;
        let mock = Arc::new(MockAnSubmitter::accepting());
        let mut r = Relayer::new(
            cfg,
            Arc::new(InMemoryBeaconSource::new(vec![update(100, 96)])),
            Arc::new(MockProofGenerator::new()),
            mock.clone(),
        )
        .unwrap();
        r.tick().await.unwrap();
        assert_eq!(mock.re_push_count(), 1);
        assert_eq!(mock.ancestry_count(), 0);
    }

    /// Two checkpoints one span apart, with the span's headers on the
    /// execution source. `submit_ancestry` decides whether the span is sent.
    fn span_relayer(
        submit_ancestry: bool,
    ) -> (
        Relayer<InMemoryBeaconSource, MockProofGenerator, MockAnSubmitter>,
        Arc<MockAnSubmitter>,
        Vec<Vec<u8>>,
    ) {
        let chain = crate::header_rlp::dummy_linked_chain(4);
        let older = crate::header_rlp::keccak256(&chain[0]);
        let newer = crate::header_rlp::keccak256(&chain[3]);
        let dir = Box::leak(Box::new(tempdir().unwrap()));
        let mut cfg = RelayerConfig::new(dir.path().join("state.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = false;
        cfg.submit_ancestry = submit_ancestry;
        let mock = Arc::new(MockAnSubmitter::accepting());
        let r = Relayer::new(
            cfg,
            Arc::new(InMemoryBeaconSource::new(vec![
                update_with_exec(100, 96, older),
                update_with_exec(132, 128, newer),
            ])),
            Arc::new(MockProofGenerator::new()),
            mock.clone(),
        )
        .unwrap()
        .with_execution(Arc::new(crate::source::InMemoryExecution::single(
            newer,
            chain.clone(),
        )));
        (r, mock, chain)
    }

    #[tokio::test]
    async fn execution_source_sends_the_span_between_two_checkpoints() {
        let (mut r, mock, chain) = span_relayer(true);
        r.tick().await.unwrap();
        assert_eq!(
            mock.ancestry_count(),
            0,
            "first checkpoint has no older anchor yet"
        );
        r.tick().await.unwrap();
        assert_eq!(mock.re_push_count(), 2);
        assert_eq!(mock.ancestry_count(), 1);
        assert!(mock.is_proven(&crate::header_rlp::keccak256(&chain[1])));
        assert!(mock.is_proven(&crate::header_rlp::keccak256(&chain[2])));
    }

    /// The span the daemon could not send is sent on a later tick, without
    /// waiting for the next checkpoint — by then the gap would be too wide.
    #[tokio::test]
    async fn a_refused_span_is_sent_on_a_later_tick() {
        let (mut r, mock, chain) = span_relayer(true);
        mock.reject_ancestry();
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        assert_eq!(mock.ancestry_count(), 0, "both sends were refused");
        assert_eq!(
            r.state.ancestry_anchor,
            Some(crate::header_rlp::keccak256(&chain[0])),
            "the cursor stays on the older anchor while the span is unsent"
        );

        mock.accept_ancestry();
        r.tick().await.unwrap();
        assert_eq!(mock.ancestry_count(), 1, "the retry must actually send the span");
        assert!(mock.is_proven(&crate::header_rlp::keccak256(&chain[1])));
        assert!(mock.is_proven(&crate::header_rlp::keccak256(&chain[2])));
        assert_eq!(
            r.state.ancestry_anchor,
            Some(crate::header_rlp::keccak256(&chain[3])),
            "a sent span moves the cursor to the head"
        );
    }

    #[tokio::test]
    async fn a_sent_span_moves_the_cursor_to_the_head() {
        let (mut r, mock, chain) = span_relayer(true);
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        assert_eq!(mock.ancestry_count(), 1);
        assert_eq!(
            r.state.ancestry_anchor,
            Some(crate::header_rlp::keccak256(&chain[3]))
        );
    }

    #[tokio::test]
    async fn execution_rpc_without_submit_flag_skips_on_chain_ancestry() {
        let (mut r, mock, chain) = span_relayer(false);
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        assert_eq!(mock.re_push_count(), 2);
        assert_eq!(mock.ancestry_count(), 0);
        assert!(!mock.is_proven(&crate::header_rlp::keccak256(&chain[1])));
    }
}
