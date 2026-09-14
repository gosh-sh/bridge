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
    /// After the first accepted `submitUpdate`, call `setLightClient` +
    /// `disableOwnerAnchors` + `disableOwnerRotation` with the relayer keys
    /// (must be the owner pubkey). Default **true**. `--no-flip-owner` opts
    /// out.
    pub flip_owner: bool,
    /// With `enable_rotate = false`: on a period jump, prove a step of the
    /// new period and advance the committee with the owner key
    /// (`setCommitteeCommitment`) instead of stopping at
    /// [`TickOutcome::RotateRequired`]. A shadow-only convenience; the
    /// contract refuses it after `disableOwnerRotation`. Default **false**.
    pub owner_hop: bool,
    /// Call `submitAncestry` after each accepted checkpoint. Default
    /// **false**. Two headers already cost ~130 M gas against the 10 M
    /// per-transaction limit, so the call cannot succeed until a keccak-256
    /// builtin lands; leaving this on burned ~0.7 vmshell every epoch past
    /// `tvm.accept()`. `ETH_RPC_URL` still attaches an execution source so
    /// the daemon can run `link_headers` locally without sending the tx.
    pub submit_ancestry: bool,
}

impl RelayerConfig {
    pub fn new(state_path: PathBuf) -> Self {
        Self {
            state_path,
            poll_interval: Duration::from_secs(64),
            enable_rotate: true,
            flip_owner: true,
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
    /// When set, each accepted `submitUpdate` walks the epoch parent-hash
    /// chain and calls `submitAncestry` (31/32 coverage). CLI-only without it.
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
                self.maybe_cover_epoch(update.execution_block_hash).await;
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
    /// push often bounced because `setLightClient` had not run yet), then
    /// optionally walk the epoch parent chain when an execution RPC is
    /// configured. On-chain `submitAncestry` is a separate flag — the call
    /// cannot succeed on Acki Nacki today (see `submit_ancestry`).
    async fn maybe_cover_epoch(&mut self, checkpoint: [u8; 32]) {
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
        let headers = match rpc.ancestry_headers(checkpoint, 32).await {
            Ok(h) => h,
            Err(e) => {
                warn!(error = %e, "epoch ancestry fetch failed");
                return;
            },
        };
        if headers.len() < 2 {
            warn!(n = headers.len(), "epoch ancestry shorter than 2 headers");
            return;
        }
        if let Err(e) = crate::header_rlp::link_headers(&headers) {
            warn!(error = %e, "epoch ancestry failed local link_headers");
            return;
        }
        if !self.config.submit_ancestry {
            info!(
                n = headers.len(),
                "epoch ancestry linked locally; on-chain submitAncestry skipped \
                 (--submit-ancestry, default off: two headers already exceed the 10 M gas limit)"
            );
            return;
        }
        match self.submitter.submit_ancestry(&headers).await {
            Ok(SubmitOutcome::Accepted {
                ..
            }) => {
                info!(n = headers.len(), "submitAncestry accepted");
            },
            Ok(other) => {
                warn!(?other, "submitAncestry did not accept");
            },
            Err(e) => {
                warn!(error = %e, "submitAncestry failed");
            },
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
        // Period jump: committee advanced by the owner, then the same bundle lands.
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
        assert!(r.state().owner_flip_done);
    }

    #[tokio::test]
    async fn first_tick_flips_owner_on_chain_writers() {
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
        assert!(r.state().owner_flip_done);
        assert!(mock.light_client_set());
        assert!(!mock.owner_anchors_enabled());
        assert!(!mock.owner_rotation_enabled());
        assert_eq!(mock.re_push_count(), 1);
    }

    #[tokio::test]
    async fn no_flip_owner_leaves_owner_path() {
        let dir = Box::leak(Box::new(tempdir().unwrap()));
        let mut cfg = RelayerConfig::new(dir.path().join("state.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = false;
        cfg.flip_owner = false;
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

    #[tokio::test]
    async fn execution_source_walks_ancestry_after_checkpoint() {
        let (child, parent) = crate::header_rlp::dummy_linked_headers();
        let ckpt = crate::header_rlp::keccak256(&child);
        let parent_hash = crate::header_rlp::keccak256(&parent);
        let dir = Box::leak(Box::new(tempdir().unwrap()));
        let mut cfg = RelayerConfig::new(dir.path().join("state.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = false;
        cfg.submit_ancestry = true;
        let mock = Arc::new(MockAnSubmitter::accepting());
        let mut r = Relayer::new(
            cfg,
            Arc::new(InMemoryBeaconSource::new(vec![update_with_exec(
                100, 96, ckpt,
            )])),
            Arc::new(MockProofGenerator::new()),
            mock.clone(),
        )
        .unwrap()
        .with_execution(Arc::new(crate::source::InMemoryExecution::single(
            ckpt,
            vec![child, parent],
        )));
        r.tick().await.unwrap();
        assert_eq!(mock.re_push_count(), 1);
        assert_eq!(mock.ancestry_count(), 1);
        assert!(mock.is_proven(&ckpt));
        assert!(mock.is_proven(&parent_hash));
    }

    #[tokio::test]
    async fn execution_rpc_without_submit_flag_skips_on_chain_ancestry() {
        let (child, parent) = crate::header_rlp::dummy_linked_headers();
        let ckpt = crate::header_rlp::keccak256(&child);
        let dir = Box::leak(Box::new(tempdir().unwrap()));
        let mut cfg = RelayerConfig::new(dir.path().join("state.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = false;
        let mock = Arc::new(MockAnSubmitter::accepting());
        let mut r = Relayer::new(
            cfg,
            Arc::new(InMemoryBeaconSource::new(vec![update_with_exec(
                100, 96, ckpt,
            )])),
            Arc::new(MockProofGenerator::new()),
            mock.clone(),
        )
        .unwrap()
        .with_execution(Arc::new(crate::source::InMemoryExecution::single(
            ckpt,
            vec![child, parent],
        )));
        r.tick().await.unwrap();
        assert_eq!(mock.re_push_count(), 1);
        assert_eq!(mock.ancestry_count(), 0);
    }
}
