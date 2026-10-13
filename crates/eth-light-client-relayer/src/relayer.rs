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

/// How long the headers for one span may stay unfetchable before it is
/// written off. Generous enough to ride out an execution-RPC outage, short
/// enough that an anchor no RPC will ever serve does not pin the cursor for
/// the life of the daemon.
const SPAN_FETCH_GRACE_S: u64 = 15 * 60;

/// How many spans may wait at once. A daemon that cannot send for a long
/// time would otherwise grow the queue without bound; past this the oldest is
/// given up, which is the one whose anchors are most likely out of the
/// one-year window anyway.
const MAX_OWED_SPANS: usize = 64;

/// How many paid submissions one ancestry span may cost before it is written
/// off. Failed header fetches are not counted: they cost nothing on chain, and
/// an execution RPC that is briefly unavailable must not spend the budget.
const MAX_SPAN_ATTEMPTS: u32 = 3;

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
        self.cover_front_span().await;
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

        // Not just equality. A beacon node restarting into sync serves a
        // finalized slot behind the one on record, and proving it would end in
        // `ERR_STALE_UPDATE` — which this daemon reads as "already on the
        // head" and would record, moving its own head backwards and pairing
        // the next span with the wrong end.
        if let Some(recorded) = self.state.last_finalized_slot {
            if update.finalized_slot < recorded {
                warn!(
                    reported = update.finalized_slot,
                    recorded, "beacon source reports a finalized slot behind the one on record"
                );
            }
            if update.finalized_slot <= recorded {
                return Ok(TickOutcome::Unchanged {
                    finalized_slot: update.finalized_slot,
                });
            }
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
            // Nothing reached the chain, so the next tick simply tries again.
            SubmitOutcome::NotSent {
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
        let old_head = self.state.last_execution_block_hash;
        match self.submitter.submit_update(&bundle).await? {
            SubmitOutcome::Accepted {
                tx_hash,
            } => {
                // The head and the span it owes land in one write, and a write
                // that fails leaves the state as it was: otherwise memory
                // would hold a head the file does not, the next tick would
                // read it as unchanged, and the span would be owed by nobody.
                let before = self.state.clone();
                self.state.record_update(
                    update.finalized_slot,
                    update.period(),
                    update.execution_block_hash,
                    update.attested_slot,
                );
                self.queue_span(old_head, update.execution_block_hash);
                if let Err(e) = self.persist() {
                    self.state = before;
                    return Err(e);
                }
                info!(slot = update.finalized_slot, "submitUpdate accepted");
                self.re_push_checkpoint(update.execution_block_hash).await;
                self.maybe_flip_owner().await?;
                Ok(TickOutcome::SubmittedUpdate {
                    finalized_slot: update.finalized_slot,
                    tx_hash,
                })
            },
            SubmitOutcome::AlreadyOnHead => {
                // The update is on the head whether this call put it there or
                // an earlier one did whose outcome never came back. Either way
                // the head has moved, so the span behind it is owed — queuing
                // it here is what keeps a lost confirmation from silently
                // costing those blocks their anchors, and it goes into the
                // same write as the head.
                let before = self.state.clone();
                self.state.record_update(
                    update.finalized_slot,
                    update.period(),
                    update.execution_block_hash,
                    update.attested_slot,
                );
                self.queue_span(old_head, update.execution_block_hash);
                if let Err(e) = self.persist() {
                    self.state = before;
                    return Err(e);
                }
                self.re_push_checkpoint(update.execution_block_hash).await;
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
            // Nothing reached the chain, so the next tick simply tries again.
            SubmitOutcome::NotSent {
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

    /// Queue the span back to the checkpoint the head stood on before. State
    /// only, no await: the caller persists it in the same write as the head it
    /// belongs to. That is the whole point of keeping this synchronous — a span
    /// queued after an await is a span a dropped tick loses, and the head is
    /// already on file by then, so no later tick reconstructs it. `tick` runs
    /// inside a `select!` against shutdown, so Ctrl-C drops the future at the
    /// first await, not only a crash.
    ///
    /// The span is never sent here: one tick works one span, so a checkpoint
    /// cannot push a second submission into the same tick.
    fn queue_span(&mut self, prev_head: Option<[u8; 32]>, checkpoint: [u8; 32]) {
        let Some(older) = prev_head else {
            info!("no previous checkpoint on record; a span needs an anchor at each end");
            return;
        };
        if older == checkpoint {
            return;
        }
        if self.state.owed_spans.len() >= MAX_OWED_SPANS {
            let dropped = self.state.owed_spans.remove(0);
            warn!(
                from = %hex::encode(dropped.older),
                to = %hex::encode(dropped.newer),
                "owed-span queue full: dropping the oldest, those blocks need \
                 setAcceptedBlockHash"
            );
        }
        self.state.owed_spans.push(crate::state::OwedSpan {
            older,
            newer: checkpoint,
            paid_attempts: 0,
            fetch_since: None,
        });
    }

    /// Re-push the proven hash to USDCBridge. The first push often bounced
    /// because the light client was not yet the sink writer. Nothing here
    /// touches the state file, so losing this call costs nothing a later
    /// `rePushAnchor` cannot redo.
    async fn re_push_checkpoint(&mut self, checkpoint: [u8; 32]) {
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
    }

    /// Work the span at the front of the queue, if any. Exactly one span per
    /// tick: the pair was fixed when it was queued, so it can never be
    /// re-paired with a later checkpoint, and its accounting is its own.
    async fn cover_front_span(&mut self) {
        let Some(front) = self.state.owed_spans.first().cloned() else {
            return;
        };
        let Some(rpc) = &self.execution else {
            // Nothing will ever fetch the headers, so holding the queue would
            // only repeat this for ever.
            self.settle_front("no execution source configured", false);
            return;
        };
        let headers = match rpc.ancestry_span(front.newer, front.older, 33).await {
            Ok(Some(h)) => {
                // The headers arrived, so the grace period starts over if a
                // later fetch fails; leaving the old start time would give the
                // span up on the first error after it elapsed.
                if let Some(f) = self.state.owed_spans.first_mut() {
                    if f.fetch_since.take().is_some() {
                        if let Err(e) = self.persist() {
                            warn!(error = %e, "could not persist the owed-span queue");
                        }
                    }
                }
                h
            },
            Ok(None) => {
                self.settle_front(
                    "no span joins these two anchors — further apart than one call carries, not \
                     on one chain, or adjacent",
                    true,
                );
                return;
            },
            Err(e) => {
                self.retry_front_fetch(e.to_string());
                return;
            },
        };
        if let Err(e) = crate::header_rlp::link_headers(&headers) {
            self.settle_front(&format!("local link_headers refused the span: {e}"), true);
            return;
        }
        if !self.config.submit_ancestry {
            info!(
                n = headers.len(),
                "span linked locally; on-chain submitAncestry skipped (--submit-ancestry, default \
                 off)"
            );
            self.settle_front("--submit-ancestry is off", false);
            return;
        }
        match self.submitter.submit_ancestry(&headers).await {
            Ok(SubmitOutcome::Accepted {
                ..
            }) => {
                info!(n = headers.len(), "submitAncestry accepted");
                self.settle_front("sent", false);
            },
            // Everything else costs an attempt, including the outcomes that
            // read like "it never got there". `call_contract` sends the
            // message and waits for it in one call, so a timeout or a dropped
            // connection arrives after the broadcast: the message may already
            // have been executed, and a ~2M-gas span charged. The daemon
            // cannot tell that from the error text, so it must assume it paid.
            // Treating these as free retries resent one span every tick
            // without limit and held back every span behind it. A fetch
            // failure is different and stays free — nothing of it reaches the
            // chain, which is what `retry_front_fetch` is for.
            Ok(other) => {
                self.retry_front_paid(format!("{other:?}"));
            },
            Err(e) => {
                self.retry_front_paid(format!("call failed, outcome unknown: {e}"));
            },
        }
    }

    /// Take the front span off the queue. `uncovered` marks the blocks as
    /// needing the owner, which is worth a warning; a span that was sent, or
    /// one nobody asked to send, is not.
    fn settle_front(&mut self, reason: &str, uncovered: bool) {
        if self.state.owed_spans.is_empty() {
            return;
        }
        let span = self.state.owed_spans.remove(0);
        if uncovered {
            warn!(
                from = %hex::encode(span.older),
                to = %hex::encode(span.newer),
                reason,
                "span given up: these blocks need setAcceptedBlockHash"
            );
        } else {
            info!(
                from = %hex::encode(span.older),
                to = %hex::encode(span.newer),
                reason,
                "span settled"
            );
        }
        if let Err(e) = self.persist() {
            warn!(error = %e, "could not persist the owed-span queue");
        }
    }

    /// A paid submission the chain refused. Bounded, because each try costs
    /// the light client and an old contract or a node without `KECCAK256`
    /// will refuse this span for ever.
    fn retry_front_paid(&mut self, reason: String) {
        let Some(front) = self.state.owed_spans.first_mut() else {
            return;
        };
        front.paid_attempts = front.paid_attempts.saturating_add(1);
        let spent = front.paid_attempts;
        if spent >= MAX_SPAN_ATTEMPTS {
            self.settle_front(&format!("refused {spent} times: {reason}"), true);
            return;
        }
        warn!(reason, attempt = spent, "span not sent; will retry");
        if let Err(e) = self.persist() {
            warn!(error = %e, "could not persist the owed-span queue");
        }
    }

    /// A header fetch costs nothing on chain, so it must not spend the paid
    /// budget — an execution RPC that rate-limits for a few polls would
    /// otherwise cost coverage. It cannot retry for ever either, so the bound
    /// is wall time, which does not move with the poll interval.
    fn retry_front_fetch(&mut self, reason: String) {
        let now = crate::state::now_secs();
        let Some(front) = self.state.owed_spans.first_mut() else {
            return;
        };
        let since = *front.fetch_since.get_or_insert(now);
        let waited = now.saturating_sub(since);
        if waited >= SPAN_FETCH_GRACE_S {
            self.settle_front(
                &format!("headers unfetchable for {waited}s: {reason}"),
                true,
            );
            return;
        }
        warn!(
            reason,
            waited_s = waited,
            "span headers did not fetch; will retry for free"
        );
        if let Err(e) = self.persist() {
            warn!(error = %e, "could not persist the owed-span queue");
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

    /// Three checkpoints one span apart, each span on the execution source.
    fn three_checkpoint_relayer() -> (
        Relayer<InMemoryBeaconSource, MockProofGenerator, MockAnSubmitter>,
        Arc<MockAnSubmitter>,
        Vec<Vec<u8>>,
    ) {
        let chain = crate::header_rlp::dummy_linked_chain(7);
        let k = |i: usize| crate::header_rlp::keccak256(&chain[i]);
        let dir = Box::leak(Box::new(tempdir().unwrap()));
        let mut cfg = RelayerConfig::new(dir.path().join("state.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = false;
        cfg.submit_ancestry = true;
        let mock = Arc::new(MockAnSubmitter::accepting());
        let r = Relayer::new(
            cfg,
            Arc::new(InMemoryBeaconSource::new(vec![
                update_with_exec(100, 96, k(0)),
                update_with_exec(132, 128, k(3)),
                update_with_exec(164, 160, k(6)),
            ])),
            Arc::new(MockProofGenerator::new()),
            mock.clone(),
        )
        .unwrap()
        .with_execution(Arc::new(crate::source::InMemoryExecution::many(vec![
            (k(3), chain[0..4].to_vec()),
            (k(6), chain[3..7].to_vec()),
        ])));
        (r, mock, chain)
    }

    /// The sequence that lost spans in every earlier version: a span is
    /// refused, the next checkpoint lands, and the new span succeeds. The
    /// refused one must still be waiting, not silently dropped.
    #[tokio::test]
    async fn a_refused_span_is_not_dropped_when_the_next_one_succeeds() {
        let (mut r, mock, chain) = three_checkpoint_relayer();
        let k = |i: usize| crate::header_rlp::keccak256(&chain[i]);

        r.tick().await.unwrap(); // checkpoint 0, nothing owed yet
        mock.reject_ancestry();
        r.tick().await.unwrap(); // checkpoint 3 accepted, span 0..3 queued
        r.tick().await.unwrap(); // works 0..3, refused
        assert_eq!(mock.ancestry_attempts(), 1, "one refused submission");
        mock.accept_ancestry();
        r.tick().await.unwrap(); // works 0..3 again, sent

        assert!(
            mock.is_proven(&k(1)),
            "the refused span must be sent, not dropped"
        );
        assert!(mock.is_proven(&k(2)));
        assert!(
            r.state.owed_spans.iter().all(|sp| sp.older != k(0)),
            "span 0..3 must be off the queue once sent"
        );
    }

    /// Two owed spans must not be merged into one impossible request, and
    /// must not share a budget.
    #[tokio::test]
    async fn two_owed_spans_keep_their_own_pairing_and_budget() {
        let (mut r, mock, chain) = three_checkpoint_relayer();
        let k = |i: usize| crate::header_rlp::keccak256(&chain[i]);

        mock.reject_ancestry();
        r.tick().await.unwrap();
        r.tick().await.unwrap(); // 0..3 queued
        r.tick().await.unwrap(); // 0..3 refused once
        r.tick().await.unwrap(); // checkpoint 6 accepted, 3..6 queued too
        assert_eq!(r.state.owed_spans.len(), 2, "both spans wait separately");
        assert_eq!(r.state.owed_spans[0].older, k(0));
        assert_eq!(r.state.owed_spans[1].older, k(3));
        assert_eq!(
            r.state.owed_spans[1].paid_attempts, 0,
            "the second span must not inherit the first span's attempts"
        );

        mock.accept_ancestry();
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        for i in [1, 2, 4, 5] {
            assert!(mock.is_proven(&k(i)), "header {i} was never anchored");
        }
        assert!(r.state.owed_spans.is_empty(), "both spans settled");
    }

    /// A resubmitted update that the chain already has still moved the head,
    /// so the span behind it is owed. Queuing only on `Accepted` lost it.
    #[tokio::test]
    async fn an_update_already_on_head_still_owes_its_span() {
        let (mut r, mock, chain) = span_relayer(true);
        let k = |i: usize| crate::header_rlp::keccak256(&chain[i]);
        r.tick().await.unwrap();
        // The second update's finalized slot is 128 (attested 132).
        mock.set_head_slot(128);
        r.tick().await.unwrap();
        assert_eq!(
            r.state.owed_spans.len(),
            1,
            "the span behind the head must be queued even when the update was already there"
        );
        assert_eq!(r.state.owed_spans[0].older, k(0));
        assert_eq!(r.state.owed_spans[0].newer, k(3));
    }

    #[tokio::test]
    async fn execution_source_sends_the_span_between_two_checkpoints() {
        let (mut r, mock, chain) = span_relayer(true);
        r.tick().await.unwrap();
        assert_eq!(
            mock.ancestry_count(),
            0,
            "the first checkpoint owes nothing"
        );
        r.tick().await.unwrap(); // second checkpoint queues the span
        r.tick().await.unwrap(); // and this tick sends it
        assert_eq!(mock.re_push_count(), 2);
        assert_eq!(mock.ancestry_count(), 1);
        assert!(mock.is_proven(&crate::header_rlp::keccak256(&chain[1])));
        assert!(mock.is_proven(&crate::header_rlp::keccak256(&chain[2])));
        assert!(r.state.owed_spans.is_empty());
    }

    /// A span the chain will never accept must stop costing submissions.
    #[tokio::test]
    async fn a_span_refused_for_good_is_written_off_after_the_attempt_budget() {
        let (mut r, mock, _chain) = span_relayer(true);
        mock.reject_ancestry();
        for _ in 0..(super::MAX_SPAN_ATTEMPTS + 6) {
            r.tick().await.unwrap();
        }
        assert!(r.state.owed_spans.is_empty(), "the span must be given up");
        assert_eq!(
            mock.ancestry_attempts(),
            super::MAX_SPAN_ATTEMPTS,
            "a refused span must cost exactly the budget: not more, and not fewer, or the budget \
             is not what decides"
        );
    }

    /// The head and the span it owes have to become durable together. `tick`
    /// runs inside a `select!` against shutdown, so Ctrl-C — not only a crash
    /// — drops the tick future at its first await. If the head is on disk and
    /// the span is not, the next tick reads the head as unchanged and those
    /// blocks are owed by nobody, with no warning naming them.
    #[tokio::test]
    async fn the_owed_span_is_durable_before_the_first_await_after_the_head_moves() {
        let chain = crate::header_rlp::dummy_linked_chain(4);
        let older = crate::header_rlp::keccak256(&chain[0]);
        let newer = crate::header_rlp::keccak256(&chain[3]);
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut cfg = RelayerConfig::new(path.clone());
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = false;
        let mock = Arc::new(MockAnSubmitter::accepting());
        mock.watch_state_file(path.clone());
        let mut r = Relayer::new(
            cfg,
            Arc::new(InMemoryBeaconSource::new(vec![
                update_with_exec(100, 96, older),
                update_with_exec(132, 128, newer),
            ])),
            Arc::new(MockProofGenerator::new()),
            mock.clone(),
        )
        .unwrap();

        r.tick().await.unwrap();
        r.tick().await.unwrap();

        let seen = mock
            .state_at_re_push()
            .expect("rePushAnchor must have been called with a state file on disk");
        let on_disk: crate::state::RelayerState =
            serde_json::from_str(&seen).expect("the captured state file must parse");
        assert!(
            on_disk
                .owed_spans
                .iter()
                .any(|s| s.older == older && s.newer == newer),
            "the span was not on disk yet when the first await ran: {:?}",
            on_disk.owed_spans
        );
        assert_eq!(
            on_disk.last_finalized_slot,
            Some(128),
            "the head it belongs to must be in the same file"
        );
    }

    /// A beacon node restarting into sync serves a finalized slot behind the
    /// one on record. Proving it ends in `ERR_STALE_UPDATE`, which this daemon
    /// reads as "already on the head" and would record — moving its own head
    /// backwards and pairing the next span with an end the chain never took.
    /// It has to be refused before a proof is even built.
    #[tokio::test]
    async fn a_finalized_slot_behind_the_record_is_neither_proved_nor_recorded() {
        let chain = crate::header_rlp::dummy_linked_chain(4);
        let older = crate::header_rlp::keccak256(&chain[0]);
        let newer = crate::header_rlp::keccak256(&chain[3]);
        let dir = tempdir().unwrap();
        let mut cfg = RelayerConfig::new(dir.path().join("state.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = false;
        let mock = Arc::new(MockAnSubmitter::accepting());
        let mut r = Relayer::new(
            cfg,
            Arc::new(InMemoryBeaconSource::new(vec![
                update_with_exec(132, 128, newer),
                update_with_exec(100, 96, older),
            ])),
            Arc::new(MockProofGenerator::new()),
            mock.clone(),
        )
        .unwrap();

        r.tick().await.unwrap();
        assert_eq!(r.state.last_finalized_slot, Some(128));
        let proven = mock.proven_count();
        let spans = r.state.owed_spans.len();

        let out = r.tick().await.unwrap();
        assert!(
            matches!(out, TickOutcome::Unchanged { .. }),
            "a slot behind the record must be left alone, got {out:?}"
        );
        assert_eq!(
            r.state.last_finalized_slot,
            Some(128),
            "the head must not move backwards"
        );
        assert_eq!(r.state.last_execution_block_hash, Some(newer));
        assert_eq!(
            mock.proven_count(),
            proven,
            "nothing may be submitted for a stale slot"
        );
        assert_eq!(
            r.state.owed_spans.len(),
            spans,
            "and no span may be queued against it"
        );
    }

    /// An outcome that reads like "it never reached the chain" still costs an
    /// attempt. `call_contract` sends and waits in one call, so a wait timeout
    /// comes after the broadcast and the span may already have been executed
    /// and paid for. Treated as free, one span was resent every tick for ever,
    /// each repeat a ~2M-gas charge, and it blocked every span behind it.
    #[tokio::test]
    async fn an_unknown_submission_outcome_costs_an_attempt() {
        let (mut r, mock, _chain) = span_relayer(true);
        mock.not_send_ancestry();
        for _ in 0..(super::MAX_SPAN_ATTEMPTS + 6) {
            r.tick().await.unwrap();
        }
        assert!(
            r.state.owed_spans.is_empty(),
            "the span must be given up, not retried for ever"
        );
        assert_eq!(
            mock.ancestry_attempts(),
            super::MAX_SPAN_ATTEMPTS,
            "an unknown outcome must cost exactly one attempt each, like a refusal"
        );
    }

    /// And the same for the call failing outright rather than answering: the
    /// error arrives after the broadcast too, so it is counted, not retried
    /// for free.
    #[tokio::test]
    async fn a_failed_submission_call_also_costs_an_attempt() {
        let (mut r, mock, _chain) = span_relayer(true);
        mock.error_ancestry();
        for _ in 0..(super::MAX_SPAN_ATTEMPTS + 6) {
            r.tick().await.unwrap();
        }
        assert!(
            r.state.owed_spans.is_empty(),
            "the span must be given up, not retried for ever"
        );
        assert_eq!(
            mock.ancestry_attempts(),
            super::MAX_SPAN_ATTEMPTS,
            "a failed call must cost one attempt each"
        );
    }

    /// The free-retry arm has to end somewhere. A span whose headers never
    /// fetch is given up once the grace is spent, naming the blocks an
    /// operator has to anchor; otherwise it sits at the front of the queue for
    /// ever and blocks every span behind it.
    #[tokio::test]
    async fn a_span_whose_headers_never_fetch_is_given_up_when_the_grace_runs_out() {
        let (mut r, mock, _chain) = span_relayer(true);
        r.state.owed_spans.clear();
        // An end the execution source does not know: every fetch errors.
        r.state.owed_spans.push(crate::state::OwedSpan {
            older: [0xA1; 32],
            newer: [0xA2; 32],
            paid_attempts: 0,
            fetch_since: Some(crate::state::now_secs() - super::SPAN_FETCH_GRACE_S - 1),
        });
        r.cover_front_span().await;
        assert!(
            r.state.owed_spans.is_empty(),
            "the span must be given up once the grace is spent"
        );
        assert_eq!(
            mock.ancestry_attempts(),
            0,
            "nothing was ever sent, so nothing may have been charged"
        );
    }

    /// And not one tick early: inside the grace the span stays, and a fetch
    /// that failed costs no paid attempt.
    #[tokio::test]
    async fn a_span_inside_the_fetch_grace_is_kept_and_costs_nothing() {
        let (mut r, mock, _chain) = span_relayer(true);
        r.state.owed_spans.clear();
        r.state.owed_spans.push(crate::state::OwedSpan {
            older: [0xA1; 32],
            newer: [0xA2; 32],
            paid_attempts: 0,
            fetch_since: None,
        });
        r.cover_front_span().await;
        assert_eq!(r.state.owed_spans.len(), 1, "still inside the grace");
        assert_eq!(
            r.state.owed_spans[0].paid_attempts, 0,
            "a fetch failure is free"
        );
        assert!(
            r.state.owed_spans[0].fetch_since.is_some(),
            "the grace has to start counting from the first failure"
        );
        assert_eq!(mock.ancestry_attempts(), 0);
    }

    /// The queue is bounded, and when it overflows the span dropped is the
    /// oldest — its anchors are the ones closest to leaving the one-year
    /// window, so it is the one least likely to be sendable at all.
    #[test]
    fn the_queue_cap_drops_the_oldest_span_and_keeps_the_newest() {
        let (mut r, _mock, _chain) = span_relayer(false);
        r.state.owed_spans.clear();
        for i in 0..super::MAX_OWED_SPANS {
            r.state.owed_spans.push(crate::state::OwedSpan {
                older: [i as u8; 32],
                newer: [(i + 1) as u8; 32],
                paid_attempts: 0,
                fetch_since: None,
            });
        }
        let oldest = r.state.owed_spans[0].newer;
        r.queue_span(Some([0xF0; 32]), [0xF1; 32]);
        assert_eq!(
            r.state.owed_spans.len(),
            super::MAX_OWED_SPANS,
            "the cap must hold"
        );
        assert_ne!(
            r.state.owed_spans[0].newer, oldest,
            "the oldest span is the one that must go"
        );
        assert_eq!(
            r.state.owed_spans.last().expect("non-empty").newer,
            [0xF1; 32],
            "the new span must be queued"
        );
    }

    /// A header fetch costs nothing on chain, so it must not spend the paid
    /// budget: the span keeps waiting instead of being given up.
    #[tokio::test]
    async fn a_failing_fetch_does_not_spend_the_submission_budget() {
        let (mut r, mock, chain) = span_relayer(true);
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        assert_eq!(mock.ancestry_count(), 1);

        // Queue a span whose newer end the execution source has no headers
        // for: every fetch for it errors, the way a rate-limited RPC does.
        r.state.owed_spans.push(crate::state::OwedSpan {
            older: crate::header_rlp::keccak256(&chain[0]),
            newer: [0x5A; 32],
            paid_attempts: 0,
            fetch_since: None,
        });
        for _ in 0..(super::MAX_SPAN_ATTEMPTS + 4) {
            r.tick().await.unwrap();
        }
        let front = r
            .state
            .owed_spans
            .first()
            .expect("the span must still wait");
        assert_eq!(
            front.paid_attempts, 0,
            "a fetch failure must not be counted"
        );
        assert!(
            front.fetch_since.is_some(),
            "the grace period must be running"
        );
    }

    /// Without an execution source nothing can ever fetch the headers, so the
    /// queue must drain instead of making every tick pay for a rePushAnchor.
    #[tokio::test]
    async fn no_execution_source_drains_the_queue_and_pays_nothing() {
        let (mut r, mock, chain) = span_relayer(true);
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        r.execution = None;
        r.state.owed_spans.push(crate::state::OwedSpan {
            older: crate::header_rlp::keccak256(&chain[0]),
            newer: [0x77; 32],
            paid_attempts: 0,
            fetch_since: None,
        });
        let pushes = mock.re_push_count();
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        assert_eq!(
            mock.re_push_count(),
            pushes,
            "the span path must not rePushAnchor"
        );
        assert!(r.state.owed_spans.is_empty(), "the queue must drain");
    }

    /// An out-of-reach pair is not a transient failure: given up at once.
    #[tokio::test]
    async fn an_out_of_reach_span_is_given_up_at_once() {
        let (mut r, mock, chain) = span_relayer(true);
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        let attempts = mock.ancestry_attempts();
        r.state.owed_spans.push(crate::state::OwedSpan {
            older: [0x99; 32],
            newer: crate::header_rlp::keccak256(&chain[3]),
            paid_attempts: 0,
            fetch_since: None,
        });
        r.tick().await.unwrap();
        assert!(r.state.owed_spans.is_empty(), "given up immediately");
        assert_eq!(mock.ancestry_attempts(), attempts, "nothing was submitted");
    }

    #[tokio::test]
    async fn execution_rpc_without_submit_flag_skips_on_chain_ancestry() {
        let (mut r, mock, chain) = span_relayer(false);
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        assert_eq!(mock.re_push_count(), 2);
        assert_eq!(mock.ancestry_count(), 0);
        assert!(!mock.is_proven(&crate::header_rlp::keccak256(&chain[1])));
        assert!(
            r.state.owed_spans.is_empty(),
            "nothing to send, nothing owed"
        );
    }
}
