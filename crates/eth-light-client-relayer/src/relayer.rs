//! One-tick loop: fetch beacon head → prove step (or demand rotate) → submit.

use std::{path::PathBuf, sync::Arc, time::Duration};

use tracing::{error, info, warn};

use crate::{
    error::RelayerError,
    prover::ProofGenerator,
    rotate::{
        check_before_submit, remove_rotate_dirs_before, run_rotate, PreSubmit, RotateDir,
        RotateInputs, RotateOutput, RotateStageRunner,
    },
    source::{BeaconSource, ExecutionSource},
    state::RelayerState,
    submitter::{AnSubmitter, SubmitOutcome},
    types::{FinalityUpdate, SLOTS_PER_SYNC_PERIOD},
};

#[derive(Clone, Debug)]
pub struct RelayerConfig {
    pub state_path: PathBuf,
    pub poll_interval: Duration,
    /// Prove the hand-off from the committee the contract holds in the
    /// background, during that committee's period, and send it with
    /// `submitRotate` ([`Relayer::with_rotate`]). Default **true**.
    /// `--no-rotate` / `enable_rotate = false`: a period jump returns
    /// [`TickOutcome::RotateRequired`], or takes the owner hop.
    pub enable_rotate: bool,
    /// After the first accepted `submitUpdate`, call `disableOwnerAnchors` +
    /// `disableOwnerRotation` with the relayer keys (must be the owner
    /// pubkey). Default **false**; `--flip-owner` opts in. While the light
    /// client anchors only one checkpoint block per epoch and no L2, the flip
    /// leaves every other deposit block without a writer.
    pub flip_owner: bool,
    /// On a period jump the contract did not get a rotate for, prove a step
    /// of the new period and advance the committee with the owner key
    /// (`setCommitteeCommitment`) instead of stopping at
    /// [`TickOutcome::RotateRequired`]. With rotate on this is the fallback,
    /// logged as an ERROR with the reason the rotate did not make it. The
    /// contract refuses it after `disableOwnerRotation`. Default **false**.
    pub owner_hop: bool,
    /// Call `submitAncestry` after each accepted checkpoint. Default
    /// **false**. The call needs a proven anchor at both ends of the span, so
    /// it sends nothing until a previous checkpoint is on record — the state
    /// file carries one across a restart — and nothing when the gap to it is
    /// wider than a span. `ETH_RPC_URL` still attaches an execution source so
    /// the daemon can run `link_headers` locally without sending the tx.
    pub submit_ancestry: bool,
    /// Use the beacon spec's Gloas slot instead of `getGloasForkSlot`.
    /// `--dry-run` sets this: `MockAnSubmitter` has no contract, and on
    /// Sepolia its default `GLOAS_NOT_SCHEDULED` would refuse every live
    /// update (`gloas_under_beacon` is already true).
    pub pin_fork_slot_from_beacon: bool,
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
            pin_fork_slot_from_beacon: false,
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
    rotate: Option<RotateWiring>,
}

/// The background rotate: its stages, where they write, and the job.
struct RotateWiring {
    runner: Arc<dyn RotateStageRunner>,
    work_dir: PathBuf,
    vk_pin: [u8; 32],
    job: Option<RotateJob>,
    /// A finished proof not yet settled with the contract.
    ready: Option<RotateOutput>,
    /// Why the last rotate did not reach the contract.
    note: Option<String>,
    /// A period whose proof was refused: this process does not prove it
    /// again.
    refused: Option<u64>,
    /// `submitRotate` calls for the period, none of them confirmed.
    sends: PeriodCount,
    /// Rotates of the period that ended in a failed stage.
    failures: PeriodCount,
    /// No new attempt before this, after a failed stage.
    retry_at: Option<std::time::Instant>,
    /// Fetch the inputs again before the next attempt; a changed answer
    /// discards the stages proven from the old one.
    refetch: bool,
    /// The contract refused an owner hop: the boundary waits for the rotate
    /// instead.
    hop_refused: bool,
}

/// A count that starts again at zero for another period.
#[derive(Clone, Copy, Debug, Default)]
struct PeriodCount {
    period: u64,
    count: u32,
}

impl PeriodCount {
    fn bump(&mut self, period: u64) -> u32 {
        if self.period != period {
            *self = Self {
                period,
                count: 0,
            };
        }
        self.count += 1;
        self.count
    }

    fn of(&self, period: u64) -> u32 {
        if self.period == period {
            self.count
        } else {
            0
        }
    }
}

/// `submitRotate` calls without a confirmation the owner hop waits for at
/// the boundary. The proof itself is kept and sent again past them.
const MAX_ROTATE_SENDS: u32 = 5;

/// Failed rotates of one period before it is not started again.
const MAX_ROTATE_FAILURES: u32 = 3;

/// Wait after the first failed rotate of a period; doubled after each next.
const ROTATE_RETRY_BACKOFF: Duration = Duration::from_secs(10 * 60);

struct RotateJob {
    period: u64,
    started: std::time::Instant,
    handle: tokio::task::JoinHandle<Result<RotateOutput, RelayerError>>,
}

/// A job that is dropped — superseded, or the daemon stopping — is aborted,
/// and the prover it runs is killed with it (`kill_on_drop`). The stages it
/// finished stay on disk.
impl Drop for RotateJob {
    fn drop(&mut self) {
        self.handle.abort();
    }
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
            rotate: None,
        })
    }

    /// Prove rotates with `runner` in `<work_dir>/rotate-<P>/`, and send only
    /// a proof whose VK blob hashes to `vk_pin`
    /// ([`crate::rotate::pinned_rotate_vk_sha256`] outside tests).
    pub fn with_rotate(
        mut self,
        runner: Arc<dyn RotateStageRunner>,
        work_dir: PathBuf,
        vk_pin: [u8; 32],
    ) -> Self {
        // The stages run with the prover directory as their working directory.
        let work_dir = std::path::absolute(&work_dir).unwrap_or(work_dir);
        self.rotate = Some(RotateWiring {
            runner,
            work_dir,
            vk_pin,
            job: None,
            ready: None,
            note: None,
            refused: None,
            sends: PeriodCount::default(),
            failures: PeriodCount::default(),
            retry_at: None,
            refetch: false,
            hop_refused: false,
        });
        self
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
        let mut update = self.source.fetch_finality().await?;
        self.guard_network(&update)?;
        if let Some(lag) = self.ws_lag(&update) {
            warn!(
                lag_slots = lag,
                last = ?self.state.last_finalized_slot,
                attested = update.attested_slot,
                "light-client lag approaching / past one sync-committee period (~27 h)"
            );
        }

        if let Some(outcome) = self.drive_rotate(&update).await? {
            return Ok(outcome);
        }

        let period = update.period();
        if let Some(last_p) = self.state.last_committee_period {
            if period > last_p {
                // A manual `submit-rotate` lands on chain without writing
                // this process's state. Read the contract so the next tick
                // submits a step instead of staying in RotateRequired.
                match self.submitter.get_committee_period().await {
                    Ok(on_chain) if on_chain >= period => {
                        self.state.record_rotate(on_chain);
                        self.persist()?;
                        self.drop_rotate_before(on_chain);
                        info!(
                            on_chain,
                            last_p, "committee already rotated on chain; recording it"
                        );
                    },
                    // Not knowing whether the rotate landed is no reason to
                    // hop over it; the next tick reads the contract again.
                    Err(e) if self.config.enable_rotate => {
                        warn!(error = %e, "getCommitteeState at the period boundary failed; next tick");
                        return Ok(TickOutcome::AnPending {
                            reason: format!("getCommitteeState at the period boundary: {e}"),
                        });
                    },
                    _ => return self.handle_period_jump(update, last_p, period).await,
                }
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

        // After Unchanged: an idle tick must not download the light-client
        // account. Pin only when this tick will prove.
        if let Err(outcome) = self.pin_gloas_fork_slot(&mut update).await {
            return Ok(outcome);
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
                let mut update = update;
                if let Err(outcome) = self.pin_gloas_fork_slot(&mut update).await {
                    return Ok(outcome);
                }
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
        // A proof that went out and is not confirmed yet may still land; it
        // gets its remaining sends before the owner hop.
        if let Some(rotate) = self.rotate.as_ref() {
            let sends = rotate.sends.of(from_period);
            if self.config.owner_hop
                && rotate
                    .ready
                    .as_ref()
                    .is_some_and(|out| out.from_period == from_period)
                && sends < MAX_ROTATE_SENDS
            {
                return Ok(TickOutcome::AnPending {
                    reason: format!(
                        "the rotate proof from period {from_period} is unconfirmed after {sends} \
                         of {MAX_ROTATE_SENDS} sends; the owner hop waits for the rest"
                    ),
                });
            }
        }
        // The rotate is proven during the period it rotates from; reaching
        // the boundary without it on chain means it did not make it in time.
        let why = self.rotate_status(from_period);
        let hop_refused = self.rotate.as_ref().is_some_and(|r| r.hop_refused);
        if self.config.owner_hop && !hop_refused {
            error!(
                from_period,
                to_period,
                reason = %why,
                "committee period advanced before a rotate was accepted; falling back to the owner \
                 hop"
            );
            let mut update = update;
            if let Err(outcome) = self.pin_gloas_fork_slot(&mut update).await {
                return Ok(outcome);
            }
            let outcome = self.owner_hop(update, from_period, to_period).await?;
            if let Some(period) = self
                .state
                .last_committee_period
                .filter(|p| *p > from_period)
            {
                self.drop_rotate_before(period);
            }
            return Ok(outcome);
        }
        self.state.record_rotate_pending(to_period);
        self.persist()?;
        warn!(
            from_period,
            to_period,
            reason = %why,
            hop_refused,
            "committee period advanced; waiting for the rotate proof (no --owner-hop, or the \
             contract refused it)"
        );
        Ok(TickOutcome::RotateRequired {
            from_period,
            to_period,
        })
    }

    /// The background rotate, once a tick: settle a finished proof with the
    /// contract, or start the rotate the contract is due. `Some` when this
    /// tick moved the committee; the step waits for the next tick. A send
    /// that is refused or not confirmed leaves the tick to the step and the
    /// period boundary.
    async fn drive_rotate(
        &mut self,
        update: &FinalityUpdate,
    ) -> Result<Option<TickOutcome>, RelayerError> {
        if !self.config.enable_rotate || self.rotate.is_none() {
            return Ok(None);
        }
        self.collect_rotate(false).await;
        let rotate = self.rotate.as_ref().expect("checked above");
        if rotate.ready.is_some() {
            return Ok(match self.settle_ready_rotate().await? {
                Some(
                    moved @ (TickOutcome::Rotated {
                        ..
                    }
                    | TickOutcome::AlreadyOnHead {
                        ..
                    }),
                ) => Some(moved),
                _ => None,
            });
        }
        if rotate.job.is_none() {
            self.maybe_start_rotate(update).await?;
        }
        Ok(None)
    }

    /// Take the result of a finished job (all of it when `block`, which only
    /// tests use).
    async fn collect_rotate(&mut self, block: bool) {
        let Some(rotate) = self.rotate.as_mut() else {
            return;
        };
        let finished = rotate
            .job
            .as_ref()
            .is_some_and(|job| block || job.handle.is_finished());
        if !finished {
            return;
        }
        let mut job = rotate.job.take().expect("checked above");
        match (&mut job.handle).await {
            Ok(Ok(out)) => {
                info!(
                    period = job.period,
                    mins = job.started.elapsed().as_secs() / 60,
                    "rotate proof finished"
                );
                rotate.ready = Some(out);
            },
            Ok(Err(e)) => {
                let failed = rotate.failures.bump(job.period);
                if failed >= MAX_ROTATE_FAILURES {
                    error!(
                        period = job.period,
                        failed,
                        error = %e,
                        "rotate failed again; not started again by this process"
                    );
                    rotate.refused = Some(job.period);
                    rotate.note = Some(format!(
                        "the rotate from period {} failed {failed} times, last: {e}. Fix the \
                         cause, remove {} and restart the daemon",
                        job.period,
                        RotateDir::new(&rotate.work_dir, job.period)
                            .root()
                            .display()
                    ));
                } else {
                    let wait = ROTATE_RETRY_BACKOFF * 2u32.pow(failed - 1);
                    warn!(
                        period = job.period,
                        failed,
                        retry_in_mins = wait.as_secs() / 60,
                        error = %e,
                        "rotate failed; trying again with the inputs fetched anew"
                    );
                    rotate.retry_at = Some(std::time::Instant::now() + wait);
                    rotate.refetch = true;
                    rotate.note = Some(format!(
                        "the rotate from period {} failed ({failed} of {MAX_ROTATE_FAILURES}): {e}",
                        job.period
                    ));
                }
            },
            Err(e) => {
                rotate.note = Some(format!(
                    "the rotate from period {} stopped: {e}",
                    job.period
                ));
            },
        }
    }

    /// Test hook: wait for the background rotate and take its result.
    #[doc(hidden)]
    pub async fn wait_for_rotate(&mut self) {
        self.collect_rotate(true).await;
    }

    /// Period of the rotate running in the background.
    pub fn rotate_running(&self) -> Option<u64> {
        self.rotate.as_ref()?.job.as_ref().map(|job| job.period)
    }

    async fn settle_ready_rotate(&mut self) -> Result<Option<TickOutcome>, RelayerError> {
        let chain = match self.submitter.get_committee_state().await {
            Ok(chain) => chain,
            Err(e) => {
                warn!(error = %e, "getCommitteeState before submitRotate failed; next tick");
                return Ok(None);
            },
        };
        let rotate = self.rotate.as_mut().expect("drive_rotate checked");
        let out = rotate.ready.clone().expect("drive_rotate checked");
        let from = out.from_period;
        match check_before_submit(&out, &rotate.vk_pin, &chain) {
            PreSubmit::Stale {
                on_chain,
            } => {
                info!(
                    from_period = from,
                    on_chain, "rotate proof finished after the contract moved on; not sent"
                );
                rotate.ready = None;
                RotateDir::new(&rotate.work_dir, from).remove();
                if self
                    .state
                    .last_committee_period
                    .is_none_or(|p| p < on_chain)
                {
                    self.state.record_rotate(on_chain);
                    self.persist()?;
                }
                Ok(None)
            },
            PreSubmit::Refuse(reason) => {
                error!(from_period = from, %reason, "rotate proof not sent");
                rotate.ready = None;
                rotate.refused = Some(from);
                rotate.note = Some(format!(
                    "the rotate proof from period {from} was not sent: {reason}"
                ));
                Ok(None)
            },
            PreSubmit::Send => {
                if let Some(why) = self.gloas_slot_blocks(&out).await {
                    self.rotate_note(why);
                    return Ok(None);
                }
                let rotate = self.rotate.as_mut().expect("drive_rotate checked");
                rotate.sends.bump(from);
                // A transport error says as little about the chain as a wait
                // that timed out.
                let outcome = match self.submitter.submit_rotate(&out.bundle).await {
                    Ok(outcome) => outcome,
                    Err(e) => SubmitOutcome::NotSent {
                        reason: format!("submitRotate: {e}"),
                    },
                };
                self.settle_rotate(from, out.bundle.period, outcome)
                    .map(Some)
            },
        }
    }

    /// Why `out` cannot go out against the contract's `getGloasForkSlot`
    /// now. A proof for another slot is discarded and proven again from
    /// inputs fetched anew; an unset slot keeps it until the owner sets one.
    async fn gloas_slot_blocks(&mut self, out: &RotateOutput) -> Option<String> {
        if self.config.pin_fork_slot_from_beacon {
            return None;
        }
        let proven = out.gloas_fork_slot();
        match self.submitter.get_gloas_fork_slot().await {
            Ok((slot, true)) if slot == proven => None,
            Ok((slot, true)) => {
                let from = out.from_period;
                warn!(
                    from_period = from,
                    proven,
                    contract = slot,
                    "rotate proof is for another Gloas slot than the contract's; proving it again"
                );
                let rotate = self.rotate.as_mut().expect("drive_rotate checked");
                rotate.ready = None;
                RotateDir::new(&rotate.work_dir, from).remove();
                Some(format!(
                    "the rotate proof from period {from} names Gloas slot {proven}, the contract \
                     {slot}; proving it again"
                ))
            },
            Ok((_, false)) => Some(
                "getGloasForkSlot is unset (exit 256); the rotate proof waits for owner \
                 set-gloas-fork-slot"
                    .into(),
            ),
            Err(e) => Some(format!("getGloasForkSlot before submitRotate: {e}")),
        }
    }

    /// Start the rotate from the committee the contract holds once the
    /// beacon chain is in that committee's period, unless the boundary is
    /// already behind and the owner hop takes it.
    async fn maybe_start_rotate(&mut self, update: &FinalityUpdate) -> Result<(), RelayerError> {
        let Some(period) = self.state.last_committee_period else {
            return Ok(());
        };
        let rotate = self.rotate.as_ref().expect("drive_rotate checked");
        // Past the boundary the owner hop takes the period, unless the
        // contract refused it; then the rotate is the only way on.
        let hop_takes_it = self.config.owner_hop && !rotate.hop_refused && update.period() > period;
        if update.signing_period() < period
            || hop_takes_it
            || rotate.refused == Some(period)
            || rotate
                .retry_at
                .is_some_and(|at| std::time::Instant::now() < at)
        {
            return Ok(());
        }
        let chain = match self.submitter.get_committee_state().await {
            Ok(chain) => chain,
            Err(e) => {
                self.rotate_note(format!("getCommitteeState: {e}"));
                return Ok(());
            },
        };
        if chain.period != period {
            if chain.period > period {
                info!(
                    recorded = period,
                    on_chain = chain.period,
                    "committee moved on chain; recording it"
                );
                self.state.record_rotate(chain.period);
                self.persist()?;
                self.drop_rotate_before(chain.period);
            } else {
                warn!(
                    recorded = period,
                    on_chain = chain.period,
                    "the state file is ahead of the contract; no rotate is started"
                );
                self.rotate_note(format!(
                    "the state file records committee period {period} and the contract holds {}: \
                     a state directory from another deployment, or a re-bootstrapped contract",
                    chain.period
                ));
            }
            return Ok(());
        }
        let work_dir = self
            .rotate
            .as_ref()
            .expect("drive_rotate checked")
            .work_dir
            .clone();
        remove_rotate_dirs_before(&work_dir, period);
        let dir = RotateDir::new(&work_dir, period);
        let refetch =
            std::mem::take(&mut self.rotate.as_mut().expect("drive_rotate checked").refetch);
        let inputs = match dir.load_inputs() {
            Ok(Some(saved)) if refetch => match self.rotate_inputs(period).await {
                Ok(fresh) if fresh == saved => saved,
                Ok(fresh) => {
                    warn!(
                        period,
                        "the beacon node answers other inputs than the failed rotate had; proving \
                         from the start"
                    );
                    dir.remove();
                    dir.save_inputs(&fresh)?;
                    fresh
                },
                Err(why) => {
                    info!(period, reason = %why, "rotate not started");
                    self.rotate_note(why);
                    if let Some(rotate) = self.rotate.as_mut() {
                        rotate.refetch = true;
                    }
                    return Ok(());
                },
            },
            Ok(Some(inputs)) => inputs,
            Ok(None) => match self.rotate_inputs(period).await {
                Ok(inputs) => {
                    dir.save_inputs(&inputs)?;
                    inputs
                },
                Err(why) => {
                    info!(period, reason = %why, "rotate not started");
                    self.rotate_note(why);
                    return Ok(());
                },
            },
            Err(e) => {
                warn!(dir = %dir.root().display(), error = %e, "unreadable rotate inputs; removing");
                dir.remove();
                return Ok(());
            },
        };
        let rotate = self.rotate.as_mut().expect("drive_rotate checked");
        let runner = rotate.runner.clone();
        let job_dir = dir.clone();
        let handle =
            tokio::spawn(async move { run_rotate(runner.as_ref(), &job_dir, &inputs).await });
        rotate.job = Some(RotateJob {
            period,
            started: std::time::Instant::now(),
            handle,
        });
        info!(period, dir = %dir.root().display(), "rotate started in the background");
        Ok(())
    }

    /// The update of `period` and of the period before, checked; the reason
    /// when the beacon node does not have them yet.
    async fn rotate_inputs(&self, period: u64) -> Result<RotateInputs, String> {
        let update = self
            .source
            .period_update(period)
            .await
            .map_err(|e| format!("update of period {period}: {e}"))?;
        let committee = self
            .source
            .period_update(period.saturating_sub(1))
            .await
            .map_err(|e| format!("update of period {}: {e}", period.saturating_sub(1)))?;
        let mut chain = update.chain;
        if !self.config.pin_fork_slot_from_beacon {
            match self.submitter.get_gloas_fork_slot().await {
                Ok((slot, true)) => {
                    if let Some(chain) = chain.as_mut() {
                        chain.gloas_fork_slot = slot;
                    }
                },
                Ok((_, false)) => {
                    return Err("getGloasForkSlot is unset; owner set-gloas-fork-slot".into());
                },
                Err(e) => return Err(format!("getGloasForkSlot: {e}")),
            }
        }
        let inputs = RotateInputs {
            period,
            update_json: update.raw_json,
            committee_json: committee.raw_json,
            chain,
        };
        inputs.check().map_err(|e| e.to_string())?;
        Ok(inputs)
    }

    fn rotate_note(&mut self, note: String) {
        if let Some(rotate) = self.rotate.as_mut() {
            rotate.note = Some(note);
        }
    }

    /// Why the rotate from `period` is not on chain, for the log.
    fn rotate_status(&self, period: u64) -> String {
        let Some(rotate) = self.rotate.as_ref() else {
            return "no rotate prover is configured".into();
        };
        if let Some(job) = &rotate.job {
            return format!(
                "the rotate from period {} is still proving ({} min in)",
                job.period,
                job.started.elapsed().as_secs() / 60
            );
        }
        if let Some(out) = &rotate.ready {
            return format!(
                "the rotate proof from period {} is waiting to be sent",
                out.from_period
            );
        }
        rotate
            .note
            .clone()
            .unwrap_or_else(|| format!("no rotate from period {period} was started"))
    }

    /// Stop and remove every rotate from a period before `period`: the
    /// contract is past it.
    fn drop_rotate_before(&mut self, period: u64) {
        let Some(rotate) = self.rotate.as_mut() else {
            return;
        };
        if rotate.job.as_ref().is_some_and(|job| job.period < period) {
            rotate.job = None;
        }
        if rotate
            .ready
            .as_ref()
            .is_some_and(|out| out.from_period < period)
        {
            rotate.ready = None;
        }
        rotate.note = None;
        rotate.retry_at = None;
        rotate.refetch = false;
        remove_rotate_dirs_before(&rotate.work_dir, period);
    }

    fn settle_rotate(
        &mut self,
        from_period: u64,
        to_period: u64,
        outcome: SubmitOutcome,
    ) -> Result<TickOutcome, RelayerError> {
        match outcome {
            SubmitOutcome::Accepted {
                tx_hash,
            } => {
                self.state.record_rotate(to_period);
                self.persist()?;
                self.drop_rotate_before(to_period);
                info!(to_period, tx = ?tx_hash.map(hex::encode), "submitRotate accepted");
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
            } => {
                // A rotation whose own attempt went out and was never
                // confirmed is refused on the retry, because the committee it
                // rotates to is the one the chain already holds:
                // `ERR_WRONG_COMMITTEE` on the current commitment, or
                // `ERR_SELF_ROTATION` on the next one. Read as a plain
                // refusal, that re-proves the same period on every later tick
                // and the head never advances again, so the landed rotation is
                // recorded here. Only for the period that was actually sent:
                // without an attempt on record the same codes mean the chain
                // sits on a committee this daemon cannot rotate from, which is
                // an operator's business.
                let code = crate::submitter::parsed_exit_code(&reason);
                if self.state.rotate_in_flight_period == Some(to_period)
                    && matches!(code, Some(242) | Some(253))
                {
                    let before = self.state.clone();
                    self.state.record_rotate(to_period);
                    if let Err(e) = self.persist() {
                        self.state = before;
                        return Err(e);
                    }
                    self.drop_rotate_before(to_period);
                    warn!(
                        to_period,
                        reason = %reason,
                        "submitRotate refused against the committee it rotates to; the earlier \
                         attempt landed — recording the rotation"
                    );
                    return Ok(TickOutcome::AlreadyOnHead {
                        finalized_slot: self.state.last_finalized_slot.unwrap_or(0),
                    });
                }
                error!(from_period, to_period, %reason, "submitRotate refused");
                // Anything but the contract's own refusal is sent again.
                let Some(code) = code else {
                    self.note_unconfirmed_rotate(from_period, &reason);
                    return Ok(TickOutcome::AnRejected {
                        reason,
                    });
                };
                if let Some(rotate) = self.rotate.as_mut() {
                    rotate.note = Some(format!("submitRotate refused: {reason}"));
                    match code {
                        // Another Gloas slot than the proof's: proven again
                        // from inputs that carry the contract's.
                        255 => {
                            rotate.ready = None;
                            RotateDir::new(&rotate.work_dir, from_period).remove();
                        },
                        // Gloas slot unset: the proof waits for the owner.
                        256 => {},
                        // The contract's own refusal is not proven again by
                        // this process.
                        _ => {
                            rotate.ready = None;
                            rotate.refused = Some(from_period);
                        },
                    }
                }
                Ok(TickOutcome::AnRejected {
                    reason,
                })
            },
            // The proof went out and its fate is unknown: the chain may have
            // run the rotation after the wait gave up. The proof is kept and
            // sent again; the checks before it see a landed rotation.
            SubmitOutcome::Pending {
                reason,
            } => {
                self.mark_rotate_in_flight(to_period)?;
                self.note_unconfirmed_rotate(from_period, &reason);
                Ok(TickOutcome::AnPending {
                    reason,
                })
            },
            // A timeout is classified here too: `call_contract` broadcasts and
            // waits in one call, so "nothing was paid for" is a billing
            // statement and not a promise that the message never landed.
            SubmitOutcome::NotSent {
                reason,
            } => {
                self.mark_rotate_in_flight(to_period)?;
                self.note_unconfirmed_rotate(from_period, &reason);
                Ok(TickOutcome::AnPending {
                    reason,
                })
            },
        }
    }

    /// After a send without a confirmation: the proof is kept and sent again
    /// on the next tick, for as long as the contract has not moved past it.
    /// `MAX_ROTATE_SENDS` only bounds how long the owner hop waits for it.
    fn note_unconfirmed_rotate(&mut self, from_period: u64, reason: &str) {
        let Some(rotate) = self.rotate.as_mut() else {
            return;
        };
        let sends = rotate.sends.of(from_period);
        warn!(from_period, sends, %reason, "submitRotate not confirmed; sending it again next tick");
        rotate.note = Some(format!(
            "the rotate proof from period {from_period} is unconfirmed after {sends} sends (the \
             owner hop waits for {MAX_ROTATE_SENDS}): {reason}"
        ));
    }

    /// Prove against `getGloasForkSlot()`, not the beacon spec. A published
    /// `GLOAS_FORK_EPOCH` that the contract does not hold would otherwise
    /// spend a full step prove and come back 255. An update that is still
    /// Electra under both values proceeds with the contract slot.
    async fn pin_gloas_fork_slot(
        &self,
        update: &mut FinalityUpdate,
    ) -> Result<Option<u64>, TickOutcome> {
        let beacon_slot = update.chain.map(|c| c.gloas_fork_slot);
        if self.config.pin_fork_slot_from_beacon {
            return Ok(beacon_slot);
        }
        let (contract_slot, is_set) = match self.submitter.get_gloas_fork_slot().await {
            Ok(v) => v,
            Err(e) => {
                return Err(TickOutcome::AnRejected {
                    reason: format!("getGloasForkSlot: {e}"),
                });
            },
        };
        if !is_set {
            return Err(TickOutcome::AnRejected {
                reason: "getGloasForkSlot is unset; owner set-gloas-fork-slot before proving \
                         (contract exit 256)"
                    .into(),
            });
        }
        if let Some(beacon_slot) = update.chain.map(|c| c.gloas_fork_slot) {
            if beacon_slot != contract_slot {
                let slot = update.attested_slot;
                let gloas_under_beacon = slot >= beacon_slot;
                let gloas_under_contract = slot >= contract_slot;
                if gloas_under_beacon || gloas_under_contract {
                    let recovery = format!(
                        "Owner setGloasForkSlot to the beacon value {beacon_slot} (callable after \
                         disableOwnerRotation)."
                    );
                    return Err(TickOutcome::ProofFailed {
                        reason: format!(
                            "beacon Gloas slot {beacon_slot} != contract getGloasForkSlot \
                             {contract_slot}; update slot {slot} would prove against the wrong \
                             fork. {recovery}"
                        ),
                    });
                }
                warn!(
                    beacon_slot,
                    contract_slot,
                    slot,
                    "beacon spec Gloas slot differs from the contract; this update is still \
                     Electra under both — proving against the contract"
                );
            }
        }
        if let Some(chain) = update.chain.as_mut() {
            chain.gloas_fork_slot = contract_slot;
        }
        Ok(beacon_slot)
    }

    fn mark_rotate_in_flight(&mut self, period: u64) -> Result<(), RelayerError> {
        if self.state.rotate_in_flight_period == Some(period) {
            return Ok(());
        }
        let before = self.state.clone();
        self.state.record_rotate_in_flight(period);
        if let Err(e) = self.persist() {
            self.state = before;
            return Err(e);
        }
        Ok(())
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
                // The contract's refusal, `disableOwnerRotation` most likely:
                // from here the boundary waits for the rotate instead.
                if matches!(other, SubmitOutcome::Rejected { .. }) {
                    if let Some(rotate) = self.rotate.as_mut() {
                        rotate.hop_refused = true;
                    }
                }
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
    use std::path::Path;

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
            gloas_fork_slot: crate::types::GLOAS_NOT_SCHEDULED,
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
        submitter.set_gloas_fork(1, true);
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
        mock.set_gloas_fork(1, true);
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

    /// An idle tick must not download the light-client account. Pin only
    /// when this tick will prove.
    #[tokio::test]
    async fn an_unchanged_tick_does_not_read_the_gloas_slot() {
        let mock = Arc::new(MockAnSubmitter::accepting());
        let dir = tempdir().unwrap();
        let mut cfg = RelayerConfig::new(dir.path().join("s.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = false;
        let mut r = Relayer::new(
            cfg,
            Arc::new(InMemoryBeaconSource::new(vec![update(100, 96)])),
            Arc::new(MockProofGenerator::new()),
            mock.clone(),
        )
        .unwrap();
        r.tick().await.unwrap();
        let after_prove = mock.gloas_gets();
        assert!(after_prove >= 1, "the first tick proves and must pin");
        match r.tick().await.unwrap() {
            TickOutcome::Unchanged {
                ..
            } => {},
            other => panic!("{other:?}"),
        }
        assert_eq!(
            mock.gloas_gets(),
            after_prove,
            "an unchanged tick must not call getGloasForkSlot"
        );
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
    /// `/updates?start_period=P` answer with an update attested at `slot`.
    fn period_update(slot: u64) -> String {
        format!(
            r#"[{{"version":"fulu","data":{{"attested_header":{{"beacon":{{"slot":"{slot}"}}}}}}}}]"#
        )
    }

    struct RotateRig {
        relayer: Relayer<InMemoryBeaconSource, MockProofGenerator, MockAnSubmitter>,
        submitter: Arc<MockAnSubmitter>,
        runner: Arc<crate::rotate::MockRotateRunner>,
        work: PathBuf,
        state: PathBuf,
    }

    /// The contract holds committee 1 and the beacon chain is in period 1:
    /// the rotate 1→2 is due. `updates` are the finality updates in order.
    fn rotate_rig(
        updates: Vec<FinalityUpdate>,
        owner_hop: bool,
        runner: crate::rotate::MockRotateRunner,
        dir: &Path,
    ) -> RotateRig {
        let state = dir.join("state.json");
        let work = dir.join("work");
        let mut cfg = RelayerConfig::new(state.clone());
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = true;
        cfg.owner_hop = owner_hop;
        let source = InMemoryBeaconSource::new(updates);
        source.set_period_update(0, period_update(100));
        source.set_period_update(1, period_update(SLOTS_PER_SYNC_PERIOD + 100));
        let submitter = Arc::new(MockAnSubmitter::accepting());
        submitter.set_committee_period(1);
        let runner = Arc::new(runner);
        let relayer = Relayer::new(
            cfg,
            Arc::new(source),
            Arc::new(MockProofGenerator::new()),
            submitter.clone(),
        )
        .unwrap()
        .with_rotate(
            runner.clone(),
            work.clone(),
            crate::rotate::MockRotateRunner::vk_pin(),
        );
        RotateRig {
            relayer,
            submitter,
            runner,
            work,
            state,
        }
    }

    fn held_runner(
        stage: crate::rotate::Stage,
    ) -> (crate::rotate::MockRotateRunner, Arc<tokio::sync::Notify>) {
        let notify = Arc::new(tokio::sync::Notify::new());
        let mut runner = crate::rotate::MockRotateRunner::new([0xC0; 32]);
        runner.hold = Some((stage, notify.clone()));
        (runner, notify)
    }

    /// The rotate starts inside the period it rotates from, the head keeps
    /// moving while it is proven, and the proof goes out once it is done —
    /// before the boundary.
    #[tokio::test]
    async fn the_rotate_runs_in_its_period_while_steps_go_on() {
        use crate::rotate::Stage;
        let p = SLOTS_PER_SYNC_PERIOD;
        let dir = tempdir().unwrap();
        let (runner, release) = held_runner(Stage::Tree);
        let mut rig = rotate_rig(
            vec![
                update(p + 70, p + 64),
                update(p + 102, p + 96),
                update(p + 134, p + 128),
            ],
            false,
            runner,
            dir.path(),
        );
        let r = &mut rig.relayer;
        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::SubmittedUpdate { .. }
        ));
        assert_eq!(
            r.rotate_running(),
            None,
            "the committee period is learnt on this tick"
        );
        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::SubmittedUpdate { .. }
        ));
        assert_eq!(r.rotate_running(), Some(1));
        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::SubmittedUpdate { .. }
        ));
        assert_eq!(
            r.rotate_running(),
            Some(1),
            "still proving; the head moved anyway"
        );
        assert_eq!(rig.submitter.rotate_submits(), 0);

        release.notify_one();
        r.wait_for_rotate().await;
        match r.tick().await.unwrap() {
            TickOutcome::Rotated {
                period, ..
            } => assert_eq!(period, 2),
            other => panic!("{other:?}"),
        }
        assert_eq!(r.state().last_committee_period, Some(2));
        assert_eq!(rig.submitter.get_committee_period().await.unwrap(), 2);
        assert_eq!(rig.submitter.current_committee(), [0xD1; 32]);
        assert!(
            !rig.work.join("rotate-1").exists(),
            "an accepted rotate leaves no directory"
        );
        assert_eq!(rig.runner.runs(), vec![
            (1, Stage::Shard),
            (1, Stage::StepSnark),
            (1, Stage::Tree)
        ]);
        // Period 1 still has updates to come; the committee period does not
        // drop back with them, and no second rotate starts.
        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::Unchanged { .. }
        ));
        assert_eq!(r.state().last_committee_period, Some(2));
        assert_eq!(r.rotate_running(), None);
    }

    /// A restart after a stage was cut short: the finished stages are not run
    /// again, the cut one is, from an empty directory.
    #[tokio::test]
    async fn a_restart_runs_only_the_stage_that_did_not_finish() {
        use crate::rotate::Stage;
        let p = SLOTS_PER_SYNC_PERIOD;
        let dir = tempdir().unwrap();
        let updates = vec![update(p + 70, p + 64), update(p + 102, p + 96)];
        let runner = crate::rotate::MockRotateRunner::new([0xC0; 32]);
        *runner.fail_once.lock().unwrap() = Some(Stage::Tree);
        let mut rig = rotate_rig(updates.clone(), false, runner, dir.path());
        rig.relayer.tick().await.unwrap();
        rig.relayer.tick().await.unwrap();
        rig.relayer.wait_for_rotate().await;
        let rotate_dir = rig.work.join("rotate-1");
        assert!(rotate_dir.join("shard").is_dir());
        assert!(rotate_dir.join("step").is_dir());
        assert!(!rotate_dir.join("tree").exists());
        assert!(
            rotate_dir.join("tree.partial/started").exists(),
            "the cut stage left its partial"
        );
        let state = rig.state.clone();
        drop(rig);

        // The same state and work directories, a new process.
        let runner = crate::rotate::MockRotateRunner::new([0xC0; 32]);
        let mut rig = rotate_rig(vec![update(p + 134, p + 128)], false, runner, dir.path());
        assert_eq!(rig.state, state);
        rig.relayer.tick().await.unwrap();
        assert_eq!(rig.relayer.rotate_running(), Some(1));
        rig.relayer.wait_for_rotate().await;
        assert_eq!(rig.runner.runs(), vec![(1, Stage::Tree)]);
        assert!(matches!(
            rig.relayer.tick().await.unwrap(),
            TickOutcome::Rotated {
                period: 2,
                ..
            }
        ));
    }

    /// The boundary passes with the rotate still proving. With `--owner-hop`
    /// the owner moves the committee, and the rotate from the period left
    /// behind is stopped and removed.
    #[tokio::test]
    async fn at_the_boundary_without_a_rotate_the_owner_hop_takes_over() {
        use crate::rotate::Stage;
        let p = SLOTS_PER_SYNC_PERIOD;
        let dir = tempdir().unwrap();
        let (runner, _never) = held_runner(Stage::StepSnark);
        let mut rig = rotate_rig(
            vec![
                update(p + 70, p + 64),
                update(p + 102, p + 96),
                update(2 * p + 40, 2 * p + 8),
            ],
            true,
            runner,
            dir.path(),
        );
        let r = &mut rig.relayer;
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        assert_eq!(r.rotate_running(), Some(1));
        let out = r.tick().await.unwrap();
        assert!(
            matches!(out, TickOutcome::SubmittedUpdate { finalized_slot, .. } if finalized_slot == 2 * p + 8),
            "{out:?}"
        );
        assert_eq!(r.state().last_committee_period, Some(2));
        assert_eq!(rig.submitter.get_committee_period().await.unwrap(), 2);
        assert_eq!(rig.submitter.rotate_submits(), 0);
        assert_eq!(
            r.rotate_running(),
            None,
            "the rotate from period 1 is stopped"
        );
        assert!(!rig.work.join("rotate-1").exists());
    }

    /// Without `--owner-hop` the daemon waits for the proof at the boundary,
    /// and sends it when it is done: the contract still holds period 1.
    #[tokio::test]
    async fn at_the_boundary_without_the_owner_hop_the_rotate_is_awaited() {
        use crate::rotate::Stage;
        let p = SLOTS_PER_SYNC_PERIOD;
        let dir = tempdir().unwrap();
        let (runner, release) = held_runner(Stage::Tree);
        let mut rig = rotate_rig(
            vec![
                update(p + 70, p + 64),
                update(p + 102, p + 96),
                update(2 * p + 40, 2 * p + 8),
            ],
            false,
            runner,
            dir.path(),
        );
        let r = &mut rig.relayer;
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        match r.tick().await.unwrap() {
            TickOutcome::RotateRequired {
                from_period,
                to_period,
            } => assert_eq!((from_period, to_period), (1, 2)),
            other => panic!("{other:?}"),
        }
        assert_eq!(r.rotate_running(), Some(1));
        release.notify_one();
        r.wait_for_rotate().await;
        assert!(matches!(r.tick().await.unwrap(), TickOutcome::Rotated {
            period: 2,
            ..
        }));
        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::SubmittedUpdate { .. }
        ));
        assert_eq!(r.state().last_finalized_slot, Some(2 * p + 8));
    }

    /// The committee moved on chain while the rotate was proven — an owner
    /// hop or a manual `submit-rotate`: the finished proof is not sent.
    #[tokio::test]
    async fn a_rotate_finished_after_the_contract_moved_on_is_not_sent() {
        use crate::rotate::Stage;
        let p = SLOTS_PER_SYNC_PERIOD;
        let dir = tempdir().unwrap();
        let (runner, release) = held_runner(Stage::Tree);
        let mut rig = rotate_rig(
            vec![
                update(p + 70, p + 64),
                update(p + 102, p + 96),
                update(p + 134, p + 128),
            ],
            false,
            runner,
            dir.path(),
        );
        let r = &mut rig.relayer;
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        rig.submitter.set_committee_period(2);
        release.notify_one();
        r.wait_for_rotate().await;
        assert!(matches!(
            r.tick().await.unwrap(),
            TickOutcome::SubmittedUpdate { .. }
        ));
        assert_eq!(rig.submitter.rotate_submits(), 0);
        assert_eq!(r.state().last_committee_period, Some(2));
        assert!(!rig.work.join("rotate-1").exists());
    }

    /// A proof whose VK blob is not the pinned one, or that starts from
    /// another committee than the contract's, is never sent — and not proven
    /// again on the next tick.
    #[tokio::test]
    async fn a_rotate_that_fails_the_checks_is_not_sent() {
        let p = SLOTS_PER_SYNC_PERIOD;
        for wrong in ["vk", "committee"] {
            let dir = tempdir().unwrap();
            let runner = crate::rotate::MockRotateRunner::new([0xC0; 32]);
            let mut rig = rotate_rig(
                vec![update(p + 70, p + 64), update(p + 102, p + 96)],
                false,
                runner,
                dir.path(),
            );
            if wrong == "vk" {
                rig.relayer.rotate.as_mut().unwrap().vk_pin = [0; 32];
            } else {
                rig.submitter.set_current_committee([0x77; 32]);
            }
            let r = &mut rig.relayer;
            r.tick().await.unwrap();
            r.tick().await.unwrap();
            r.wait_for_rotate().await;
            r.tick().await.unwrap();
            r.tick().await.unwrap();
            assert_eq!(rig.submitter.rotate_submits(), 0, "{wrong}");
            assert_eq!(r.rotate_running(), None, "{wrong}");
            assert_eq!(rig.runner.runs().len(), 3, "{wrong}: proven once");
            assert_eq!(r.state().last_committee_period, Some(1), "{wrong}");
            let status = r.rotate_status(1);
            let named = if wrong == "vk" {
                "rotate_vk_blob.bin sha256"
            } else {
                "currentCommittee"
            };
            assert!(status.contains(named), "{wrong}: {status}");
        }
    }

    /// No update attested in the period yet, or no update of the period
    /// before: nothing is started.
    #[tokio::test]
    async fn without_live_inputs_no_prover_starts() {
        let p = SLOTS_PER_SYNC_PERIOD;
        for (period_1, period_0) in [
            (Some(period_update(100)), Some(period_update(100))),
            (Some(period_update(p + 100)), None),
            (Some(period_update(p + 100)), Some(period_update(p + 100))),
            (None, Some(period_update(100))),
            (Some(String::new()), Some(period_update(100))),
        ] {
            let dir = tempdir().unwrap();
            let source =
                InMemoryBeaconSource::new(vec![update(p + 70, p + 64), update(p + 102, p + 96)]);
            if let Some(json) = &period_1 {
                source.set_period_update(1, json.clone());
            }
            if let Some(json) = &period_0 {
                source.set_period_update(0, json.clone());
            }
            let submitter = Arc::new(MockAnSubmitter::accepting());
            submitter.set_committee_period(1);
            let runner = Arc::new(crate::rotate::MockRotateRunner::new([0xC0; 32]));
            let mut cfg = RelayerConfig::new(dir.path().join("state.json"));
            cfg.poll_interval = Duration::from_millis(1);
            let mut r = Relayer::new(
                cfg,
                Arc::new(source),
                Arc::new(MockProofGenerator::new()),
                submitter,
            )
            .unwrap()
            .with_rotate(
                runner.clone(),
                dir.path().join("work"),
                crate::rotate::MockRotateRunner::vk_pin(),
            );
            r.tick().await.unwrap();
            assert!(matches!(
                r.tick().await.unwrap(),
                TickOutcome::SubmittedUpdate { .. }
            ));
            assert_eq!(r.rotate_running(), None, "{period_1:?} / {period_0:?}");
            assert!(runner.runs().is_empty());
            assert!(!dir.path().join("work/rotate-1").exists());
        }
    }

    /// `call_contract` broadcasts and waits in one call, so a wait that times
    /// out can still have left the rotation on the chain. The retry is then
    /// refused with `ERR_WRONG_COMMITTEE` (242) against the committee it
    /// rotates to. Unless that refusal is read as "it landed", every later
    /// tick re-enters the period jump and the head never moves again.
    #[tokio::test]
    async fn a_rotate_that_timed_out_and_then_is_refused_as_applied_is_recorded() {
        let p = SLOTS_PER_SYNC_PERIOD;
        let dir = tempdir().unwrap();
        let runner = crate::rotate::MockRotateRunner::new([0xC0; 32]);
        let mut rig = rotate_rig(
            vec![
                update(p + 70, p + 64),
                update(p + 102, p + 96),
                update(2 * p + 40, 2 * p + 8),
            ],
            false,
            runner,
            dir.path(),
        );
        rig.submitter.script_rotate(vec![
            SubmitOutcome::NotSent {
                reason: "submitRotate: Error: wait message timed out".into(),
            },
            SubmitOutcome::Rejected {
                reason: "submitRotate failed: tvm error (account=0abc) (exit_code=242)".into(),
            },
        ]);
        let r = &mut rig.relayer;
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        r.wait_for_rotate().await;
        // The unconfirmed send leaves the tick to the step, which is past the
        // boundary.
        let out = r.tick().await.unwrap();
        assert!(matches!(out, TickOutcome::RotateRequired { .. }), "{out:?}");
        assert_eq!(r.state().rotate_in_flight_period, Some(2));
        assert_eq!(r.state().last_committee_period, Some(1));

        let out = r.tick().await.unwrap();
        assert!(matches!(out, TickOutcome::AlreadyOnHead { .. }), "{out:?}");
        assert_eq!(r.state().last_committee_period, Some(2));
        assert_eq!(r.state().rotate_in_flight_period, None);

        // The point of recording it: the daemon reaches the step again.
        let out = r.tick().await.unwrap();
        assert!(
            matches!(out, TickOutcome::SubmittedUpdate { .. }),
            "the head must advance after the rotation is recorded: {out:?}"
        );
        assert_eq!(r.state().last_finalized_slot, Some(2 * p + 8));
    }

    /// The same refusal without an attempt of this daemon's own on record
    /// means the chain sits on a committee it cannot rotate from. That is not
    /// a rotation to record — it needs an operator.
    #[tokio::test]
    async fn a_wrong_committee_refusal_with_no_attempt_in_flight_is_not_recorded() {
        let p = SLOTS_PER_SYNC_PERIOD;
        let dir = tempdir().unwrap();
        let runner = crate::rotate::MockRotateRunner::new([0xC0; 32]);
        let mut rig = rotate_rig(
            vec![update(p + 70, p + 64), update(p + 102, p + 96)],
            false,
            runner,
            dir.path(),
        );
        rig.submitter.script_rotate(vec![SubmitOutcome::Rejected {
            reason: "submitRotate failed: tvm error (account=0abc) (exit_code=242)".into(),
        }]);
        let r = &mut rig.relayer;
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        r.wait_for_rotate().await;
        let out = r.tick().await.unwrap();
        assert!(matches!(out, TickOutcome::Unchanged { .. }), "{out:?}");
        assert_eq!(rig.submitter.rotate_submits(), 1);
        assert_eq!(r.state().last_committee_period, Some(1));
        assert_eq!(r.state().rotate_in_flight_period, None);
        assert!(
            r.rotate_status(1).contains("exit_code=242"),
            "{}",
            r.rotate_status(1)
        );
        // The contract's own refusal: not proven again by this process.
        r.tick().await.unwrap();
        assert_eq!(r.rotate_running(), None);
        assert_eq!(rig.runner.runs().len(), 3);
    }

    /// A send whose fate stays unknown past the boundary gets
    /// `MAX_ROTATE_SENDS` in all; the owner hop waits for them, then takes the
    /// period.
    #[tokio::test]
    async fn an_unconfirmed_rotate_has_its_sends_before_the_owner_hop() {
        let p = SLOTS_PER_SYNC_PERIOD;
        let dir = tempdir().unwrap();
        let runner = crate::rotate::MockRotateRunner::new([0xC0; 32]);
        let mut rig = rotate_rig(
            vec![
                update(p + 70, p + 64),
                update(p + 102, p + 96),
                update(2 * p + 40, 2 * p + 8),
            ],
            true,
            runner,
            dir.path(),
        );
        rig.submitter.script_rotate(
            (0..MAX_ROTATE_SENDS)
                .map(|_| SubmitOutcome::NotSent {
                    reason: "submitRotate: Error: wait message timed out".into(),
                })
                .collect(),
        );
        let r = &mut rig.relayer;
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        r.wait_for_rotate().await;
        for send in 1..MAX_ROTATE_SENDS {
            let out = r.tick().await.unwrap();
            assert!(
                matches!(out, TickOutcome::AnPending { .. }),
                "send {send}: {out:?}"
            );
            assert_eq!(rig.submitter.rotate_submits(), send);
            assert_eq!(r.state().last_committee_period, Some(1));
        }
        let out = r.tick().await.unwrap();
        assert_eq!(rig.submitter.rotate_submits(), MAX_ROTATE_SENDS);
        assert_eq!(r.state().last_committee_period, Some(2), "{out:?}");
        r.tick().await.unwrap();
        assert_eq!(rig.submitter.rotate_submits(), MAX_ROTATE_SENDS);
    }

    /// `--dry-run` with rotate on: the mock contract is bootstrapped by the
    /// first step, so the rotate runs and the loop crosses the boundary.
    #[tokio::test]
    async fn a_dry_run_rotates_across_the_boundary() {
        let p = SLOTS_PER_SYNC_PERIOD;
        let dir = tempdir().unwrap();
        let source = InMemoryBeaconSource::new(vec![
            update(p + 70, p + 64),
            update(p + 102, p + 96),
            update(2 * p + 40, 2 * p + 8),
        ]);
        source.set_period_update(0, period_update(100));
        source.set_period_update(1, period_update(p + 100));
        let submitter = Arc::new(MockAnSubmitter::dry_run());
        let mut cfg = RelayerConfig::new(dir.path().join("state.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = true;
        cfg.pin_fork_slot_from_beacon = true;
        let mut r = Relayer::new(
            cfg,
            Arc::new(source),
            Arc::new(MockProofGenerator::new()),
            submitter.clone(),
        )
        .unwrap()
        .with_rotate(
            Arc::new(crate::rotate::MockRotateRunner::new([0xC0; 32])),
            "work-relative".into(),
            crate::rotate::MockRotateRunner::vk_pin(),
        );
        assert!(r.rotate.as_ref().unwrap().work_dir.is_absolute());
        r.rotate.as_mut().unwrap().work_dir = dir.path().join("work");
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        assert_eq!(r.rotate_running(), Some(1));
        r.wait_for_rotate().await;
        let out = r.tick().await.unwrap();
        assert!(matches!(out, TickOutcome::Rotated { .. }), "{out:?}");
        let out = r.tick().await.unwrap();
        assert!(
            matches!(out, TickOutcome::SubmittedUpdate { .. }),
            "{out:?}"
        );
        assert_eq!(r.state().last_committee_period, Some(2));
    }

    /// A node outage that eats more sends than the owner hop waits for does
    /// not cost the proof: without the hop it is sent until it lands.
    #[tokio::test]
    async fn an_unconfirmed_rotate_is_kept_past_the_send_count() {
        let p = SLOTS_PER_SYNC_PERIOD;
        let dir = tempdir().unwrap();
        let runner = crate::rotate::MockRotateRunner::new([0xC0; 32]);
        let mut rig = rotate_rig(
            vec![update(p + 70, p + 64), update(p + 102, p + 96)],
            false,
            runner,
            dir.path(),
        );
        rig.submitter.script_rotate(
            (0..MAX_ROTATE_SENDS + 1)
                .map(|_| SubmitOutcome::NotSent {
                    reason: "submitRotate: error sending request: Connection refused".into(),
                })
                .collect(),
        );
        let r = &mut rig.relayer;
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        r.wait_for_rotate().await;
        for _ in 0..=MAX_ROTATE_SENDS {
            r.tick().await.unwrap();
        }
        assert_eq!(rig.submitter.rotate_submits(), MAX_ROTATE_SENDS + 1);
        let out = r.tick().await.unwrap();
        assert!(matches!(out, TickOutcome::Rotated { .. }), "{out:?}");
        assert_eq!(r.state().last_committee_period, Some(2));
        assert_eq!(rig.runner.runs().len(), 3);
    }

    /// Once the contract refuses the owner hop (`disableOwnerRotation`), the
    /// boundary waits for the rotate, and the rotate is started past it.
    #[tokio::test]
    async fn a_refused_owner_hop_leaves_the_period_to_the_rotate() {
        let p = SLOTS_PER_SYNC_PERIOD;
        let dir = tempdir().unwrap();
        let runner = crate::rotate::MockRotateRunner::new([0xC0; 32]);
        let mut rig = rotate_rig(
            vec![update(p + 70, p + 64), update(2 * p + 40, 2 * p + 8)],
            true,
            runner,
            dir.path(),
        );
        rig.submitter.flip_owner().await.unwrap();
        let r = &mut rig.relayer;
        r.tick().await.unwrap();
        let out = r.tick().await.unwrap();
        assert!(matches!(out, TickOutcome::AnRejected { .. }), "{out:?}");
        assert_eq!(r.rotate_running(), None);
        let out = r.tick().await.unwrap();
        assert!(matches!(out, TickOutcome::RotateRequired { .. }), "{out:?}");
        assert_eq!(r.rotate_running(), Some(1));
        r.wait_for_rotate().await;
        let out = r.tick().await.unwrap();
        assert!(matches!(out, TickOutcome::Rotated { .. }), "{out:?}");
        assert_eq!(r.state().last_committee_period, Some(2));
    }

    /// A failed stage is started again after a backoff, from inputs fetched
    /// anew, and after `MAX_ROTATE_FAILURES` not at all.
    #[tokio::test]
    async fn a_failing_rotate_backs_off_and_then_stops() {
        let p = SLOTS_PER_SYNC_PERIOD;
        let dir = tempdir().unwrap();
        let runner = crate::rotate::MockRotateRunner::new([0xC0; 32]);
        *runner.fail_once.lock().unwrap() = Some(crate::rotate::Stage::Shard);
        let mut rig = rotate_rig(
            vec![update(p + 70, p + 64), update(p + 102, p + 96)],
            false,
            runner,
            dir.path(),
        );
        let r = &mut rig.relayer;
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        r.wait_for_rotate().await;
        for failed in 1..MAX_ROTATE_FAILURES {
            r.tick().await.unwrap();
            assert_eq!(r.rotate_running(), None, "failure {failed}: backing off");
            let status = r.rotate_status(1);
            assert!(
                status.contains(&format!("{failed} of {MAX_ROTATE_FAILURES}")),
                "{status}"
            );
            *rig.runner.fail_once.lock().unwrap() = Some(crate::rotate::Stage::Shard);
            r.rotate.as_mut().unwrap().retry_at = Some(std::time::Instant::now());
            r.tick().await.unwrap();
            assert_eq!(
                r.rotate_running(),
                Some(1),
                "failure {failed}: started again"
            );
            r.wait_for_rotate().await;
        }
        assert_eq!(rig.runner.runs().len(), MAX_ROTATE_FAILURES as usize);
        r.rotate.as_mut().unwrap().retry_at = None;
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        assert_eq!(r.rotate_running(), None);
        assert_eq!(rig.runner.runs().len(), MAX_ROTATE_FAILURES as usize);
        let status = r.rotate_status(1);
        assert!(status.contains("rotate-1"), "{status}");
    }

    /// A proof for another Gloas slot than the contract's is not sent; it is
    /// proven again from inputs that carry the contract's slot.
    #[tokio::test]
    async fn a_rotate_for_another_gloas_slot_is_proven_again() {
        let p = SLOTS_PER_SYNC_PERIOD;
        let dir = tempdir().unwrap();
        let runner = crate::rotate::MockRotateRunner::new([0xC0; 32]);
        let mut rig = rotate_rig(
            vec![
                with_chain(update(p + 70, p + 64), 1),
                with_chain(update(p + 102, p + 96), 1),
            ],
            false,
            runner,
            dir.path(),
        );
        let r = &mut rig.relayer;
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        r.wait_for_rotate().await;
        rig.submitter.set_gloas_fork(5 * p, true);
        r.tick().await.unwrap();
        assert_eq!(rig.submitter.rotate_submits(), 0);
        assert!(!rig.work.join("rotate-1").exists());
        r.tick().await.unwrap();
        assert_eq!(r.rotate_running(), Some(1));
        r.wait_for_rotate().await;
        let out = r.tick().await.unwrap();
        assert!(matches!(out, TickOutcome::Rotated { .. }), "{out:?}");
        assert_eq!(rig.submitter.rotate_submits(), 1);
        assert_eq!(rig.runner.runs().len(), 6);
    }

    /// Not knowing whether a rotate landed is no reason to hop over it.
    #[tokio::test]
    async fn a_failed_committee_read_at_the_boundary_does_not_hop() {
        let p = SLOTS_PER_SYNC_PERIOD;
        let dir = tempdir().unwrap();
        let (runner, _never) = held_runner(crate::rotate::Stage::Shard);
        let mut rig = rotate_rig(
            vec![update(p + 70, p + 64), update(2 * p + 40, 2 * p + 8)],
            true,
            runner,
            dir.path(),
        );
        rig.submitter.fail_committee_period_reads();
        let r = &mut rig.relayer;
        r.tick().await.unwrap();
        let out = r.tick().await.unwrap();
        assert!(matches!(out, TickOutcome::AnPending { .. }), "{out:?}");
        assert_eq!(r.state().last_committee_period, Some(1));
    }

    /// A daemon that stops — SIGTERM ends `run_until_shutdown` and drops the
    /// relayer — kills the stage prover rather than waiting hours for it.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn stopping_the_daemon_kills_the_running_stage() {
        use crate::rotate::{SubprocessRotateConfig, SubprocessRotateRunner};
        let p = SLOTS_PER_SYNC_PERIOD;
        let dir = tempdir().unwrap();
        let params = dir.path().join("params");
        std::fs::create_dir_all(&params).unwrap();
        let pid_file = dir.path().join("pid");
        let bin = dir.path().join("shard.sh");
        let script = dir.path().join("shard.src");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho $$ > {}\nexec sleep 60\n",
                pid_file.display()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Copied rather than written in place: see `prover::tests::install_script`.
        assert!(std::process::Command::new("cp")
            .arg(&script)
            .arg(&bin)
            .status()
            .unwrap()
            .success());
        let runner = SubprocessRotateRunner::new(SubprocessRotateConfig {
            shard_prover_bin: bin.clone(),
            step_snark_prover_bin: bin.clone(),
            rotate_prover_bin: bin,
            prover_dir: params,
            log_dir: dir.path().join("logs"),
            timeout: Duration::from_secs(600),
        });
        let source =
            InMemoryBeaconSource::new(vec![update(p + 70, p + 64), update(p + 102, p + 96)]);
        source.set_period_update(0, period_update(100));
        source.set_period_update(1, period_update(p + 100));
        let submitter = Arc::new(MockAnSubmitter::accepting());
        submitter.set_committee_period(1);
        let mut cfg = RelayerConfig::new(dir.path().join("state.json"));
        cfg.poll_interval = Duration::from_millis(1);
        let mut r = Relayer::new(
            cfg,
            Arc::new(source),
            Arc::new(MockProofGenerator::new()),
            submitter,
        )
        .unwrap()
        .with_rotate(Arc::new(runner), dir.path().join("work"), [0; 32]);
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        assert_eq!(r.rotate_running(), Some(1));
        let mut pid = String::new();
        for _ in 0..200 {
            if let Ok(text) = std::fs::read_to_string(&pid_file) {
                if text.ends_with('\n') {
                    pid = text.trim().to_string();
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!pid.is_empty(), "the stage prover never started");
        let alive = || {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .map(|stat| !stat.split(") ").nth(1).unwrap_or("").starts_with('Z'))
                .unwrap_or(false)
        };
        assert!(alive());
        drop(r);
        for _ in 0..200 {
            if !alive() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!alive(), "the stage prover outlived the daemon");
        let rotate_dir = dir.path().join("work/rotate-1");
        assert!(
            rotate_dir.join("inputs/update.json").is_file(),
            "the inputs stay for the restart"
        );
        assert!(
            !rotate_dir.join("shard").exists(),
            "a killed stage is not done"
        );
    }

    /// `submit-rotate` is a one-shot against the contract. The running
    /// daemon only learns the new period by reading `getCommitteeState`.
    #[tokio::test]
    async fn a_manual_submit_rotate_is_picked_up_on_the_next_tick() {
        let src = InMemoryBeaconSource::new(vec![update(100, 96), update(9000, 8192)]);
        let mock = Arc::new(MockAnSubmitter::accepting());
        let dir = tempdir().unwrap();
        let mut cfg = RelayerConfig::new(dir.path().join("s.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = true;
        let mut r = Relayer::new(
            cfg,
            Arc::new(src),
            Arc::new(MockProofGenerator::new()),
            mock.clone(),
        )
        .unwrap();
        r.tick().await.unwrap();
        mock.set_committee_period(1);
        match r.tick().await.unwrap() {
            TickOutcome::SubmittedUpdate {
                finalized_slot, ..
            } => {
                assert_eq!(finalized_slot, 8192);
            },
            other => panic!("{other:?}"),
        }
        assert_eq!(r.state().last_committee_period, Some(1));
    }

    #[tokio::test]
    async fn unset_gloas_fork_slot_refuses_before_prove() {
        let mock = Arc::new(MockAnSubmitter::accepting());
        mock.set_gloas_fork(0, false);
        let dir = tempdir().unwrap();
        let mut cfg = RelayerConfig::new(dir.path().join("s.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = false;
        let mut r = Relayer::new(
            cfg,
            Arc::new(InMemoryBeaconSource::new(vec![update(100, 96)])),
            Arc::new(MockProofGenerator::new()),
            mock,
        )
        .unwrap();
        match r.tick().await.unwrap() {
            TickOutcome::AnRejected {
                reason,
            } => {
                assert!(reason.contains("unset"), "{reason}");
            },
            other => panic!("{other:?}"),
        }
        assert_eq!(r.state().last_finalized_slot, None);
    }

    #[tokio::test]
    async fn beacon_gloas_ahead_of_the_contract_refuses_a_post_fork_update() {
        let mock = Arc::new(MockAnSubmitter::accepting());
        mock.set_gloas_fork(crate::types::GLOAS_NOT_SCHEDULED, true);
        let sepolia = 11_296_768;
        let dir = tempdir().unwrap();
        let mut cfg = RelayerConfig::new(dir.path().join("s.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = false;
        let mut u = with_chain(update(sepolia + 32, sepolia), 0xd8);
        if let Some(c) = u.chain.as_mut() {
            c.gloas_fork_slot = sepolia;
        }
        let mut r = Relayer::new(
            cfg,
            Arc::new(InMemoryBeaconSource::new(vec![u])),
            Arc::new(MockProofGenerator::new()),
            mock,
        )
        .unwrap();
        match r.tick().await.unwrap() {
            TickOutcome::ProofFailed {
                reason,
            } => {
                assert!(reason.contains("beacon Gloas slot"), "{reason}");
                assert!(reason.contains("setGloasForkSlot"), "{reason}");
            },
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn beacon_gloas_ahead_of_the_contract_still_proves_an_electra_update() {
        let mock = Arc::new(MockAnSubmitter::accepting());
        mock.set_gloas_fork(crate::types::GLOAS_NOT_SCHEDULED, true);
        let dir = tempdir().unwrap();
        let mut cfg = RelayerConfig::new(dir.path().join("s.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = false;
        let mut u = with_chain(update(100, 96), 0xd8);
        if let Some(c) = u.chain.as_mut() {
            c.gloas_fork_slot = 11_296_768;
        }
        let mut r = Relayer::new(
            cfg,
            Arc::new(InMemoryBeaconSource::new(vec![u])),
            Arc::new(MockProofGenerator::new()),
            mock,
        )
        .unwrap();
        match r.tick().await.unwrap() {
            TickOutcome::SubmittedUpdate {
                finalized_slot, ..
            } => {
                assert_eq!(finalized_slot, 96);
            },
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn dry_run_uses_the_beacon_gloas_slot() {
        let sepolia = 11_296_768;
        let mut u = with_chain(update(sepolia + 32, sepolia), 0xd8);
        if let Some(c) = u.chain.as_mut() {
            c.gloas_fork_slot = sepolia;
        }
        let dir = tempdir().unwrap();
        let mut cfg = RelayerConfig::new(dir.path().join("s.json"));
        cfg.poll_interval = Duration::from_millis(1);
        cfg.enable_rotate = false;
        cfg.pin_fork_slot_from_beacon = true;
        let mut r = Relayer::new(
            cfg,
            Arc::new(InMemoryBeaconSource::new(vec![u])),
            Arc::new(MockProofGenerator::new()),
            Arc::new(MockAnSubmitter::accepting()),
        )
        .unwrap();
        match r.tick().await.unwrap() {
            TickOutcome::SubmittedUpdate {
                finalized_slot, ..
            } => {
                assert_eq!(finalized_slot, sepolia);
            },
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
