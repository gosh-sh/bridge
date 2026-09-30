//! The deposit driver: one operation from preflight to credit, each state
//! written before the step that could make it wrong. `drive` continues an
//! operation from whatever state it is in; `--resume` uses it too, and
//! `--abandon` releases an operation whose outcome is unknown.
//!
//! Once the wallet may have been asked for the deposit, no error of this
//! driver is exit 2: a failure that has no code of its own takes the one
//! the operation's stage on disk calls for, the code an interrupt at that
//! point would give ([`signals::exit_for`]). The one exception is a resume
//! or an abandon under a command line that contradicts the operation's
//! record: that is refused with exit 2 before anything is done.

use std::{
    sync::{Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use alloy_primitives::{Address, B256, U256};
use anyhow::{anyhow, Context as _};
use serde_json::json;
use tokio::time::Instant;

use crate::{
    args::UsdcAmount,
    deposit::{
        an::{AccStatus, AnRead},
        anchor_wait::{self, WaitCtx, WaitExit},
        args::{an_network_id, AnTarget, DepositParams, Network, OpRef, RunMode},
        binding::{check_explicit, claims_of_others},
        credit::{self, Identity},
        evm::{deposit_calldata, parse_deposit_log, read_balance, BlockTag, DEPOSIT_CALLDATA_LEN},
        evm_confirm::{self, Expect, Negative, Outcome},
        evm_steps::{
            build_deposit_request, ensure_allowance, request_deposit, ApproveOutcome,
            RequestOutcome,
        },
        finalize::{self, FinCtx, FinalizeExit},
        identity::{offline_context, voucher_account_id, DepositIdentity, BRIDGE_ABI},
        lc_readiness::AnchorPlan,
        locks::{DirLock, OpLock},
        pi::{self, DepositPublicInputs, ExpectedInputs},
        preflight::{self, Deps},
        prover::{self, ProveRequest},
        prover_files::ProverDir,
        recovery::{self, SearchOutcome},
        retry::{once, transient, until, ONE_READ},
        signals,
        store::{
            blocking_by_sender, DepositInfo, FailReason, FinalizeInfo, OpRecord, OpStage, Store,
            TxClaim,
        },
        ui::{self, StepId, StepState, Ui},
        wallet::{account_check, Wallet},
        DepositSuccess,
    },
    errors::{CliError, CliResult, ExitCode, Stage},
};

/// What the driver needs besides the operation's record: learned by the
/// preflight of a new deposit, or by the checks of a resume.
pub struct RunCx {
    /// Who is expected to anchor the deposit's block.
    pub plan: AnchorPlan,
    /// The Acki Nacki bridge account.
    pub bridge_acc: [u8; 32],
    /// The dapp the Acki Nacki bridge lives in.
    pub bridge_dapp: [u8; 32],
    /// The bridge's light client, if it names one.
    pub light_client: Option<[u8; 32]>,
    /// The prover directory.
    pub prover: ProverDir,
    /// `--rpc-url`, for the prover's fetcher.
    pub rpc_url: String,
    /// The token the EVM bridge takes.
    pub usdc: Address,
}

/// A new deposit that did not succeed: its error, and whether that error
/// is the wallet failing to pair. Then the wallet was asked for nothing,
/// and `--qr-mode both` may offer the EIP-681 codes instead; after the
/// pairing, a failure is the run's answer.
#[derive(Debug)]
pub struct FreshError {
    /// What the run ends with; boxed, as it travels in a `Result` beside a
    /// summary.
    pub error: Box<CliError>,
    /// The wallet never paired.
    pub pairing_failed: bool,
}

impl From<CliError> for FreshError {
    fn from(error: CliError) -> Self {
        FreshError {
            error: Box::new(error),
            pairing_failed: false,
        }
    }
}

/// A deposit outcome other than exit 2, about operation `op`.
fn err(exit: ExitCode, stage: Stage, op: &str, msg: impl Into<String>) -> CliError {
    CliError::deposit(exit, stage, Some(op), msg)
}

/// A record that could not be written before the wallet was asked for the
/// deposit: exit 2.
fn nothing_sent(e: std::io::Error) -> CliError {
    CliError::Preflight {
        reason: format!("cannot write the deposit operation: {e} (nothing was sent)"),
        source: None,
    }
}

/// The stage of the pipeline an error names for an operation at `s`.
fn stage_of(s: OpStage) -> Stage {
    match s {
        OpStage::Reserved => Stage::Preflight,
        OpStage::Requested | OpStage::Signed | OpStage::Abandoned => Stage::Deposit,
        OpStage::Confirmed => Stage::Anchor,
        OpStage::Anchored => Stage::Prove,
        OpStage::Proved | OpStage::Finalizing => Stage::Finalize,
        OpStage::Credited | OpStage::Failed => Stage::Credit,
    }
}

/// `what` went wrong once the wallet may have been asked for the deposit.
/// The exit is the one the operation's stage on disk calls for; exit 2
/// only when the record says the wallet was never asked. A record that
/// cannot be read leaves the stage unknown, and that is exit 34.
fn by_stage_on_disk(store: &Store, op: &str, what: &str) -> CliError {
    match store.load(op) {
        Ok(r) => match signals::exit_for(Some(r.stage)) {
            ExitCode::PreflightRefused => CliError::Preflight {
                reason: format!("{what} (nothing was sent)"),
                source: None,
            },
            exit => err(
                exit,
                stage_of(r.stage),
                op,
                format!("{what}; continue with --resume {op}"),
            ),
        },
        Err(_) => err(
            ExitCode::CreditUnconfirmed,
            Stage::Deposit,
            op,
            format!(
                "{what}; the operation's record cannot be read either, so how far it got is \
                 unknown; continue with --resume {op}"
            ),
        ),
    }
}

/// `e`, raised after the wallet may have been asked for the deposit, as
/// [`by_stage_on_disk`] words it. An error that carries a deposit exit of
/// its own keeps it.
fn after_request(store: &Store, op: &str, e: CliError) -> CliError {
    match e {
        CliError::Deposit {
            ..
        } => e,
        CliError::Preflight {
            reason, ..
        } => by_stage_on_disk(store, op, &reason),
        other => by_stage_on_disk(store, op, &other.to_string()),
    }
}

/// Writes the record. A write that fails is never exit 2 once the wallet
/// may have been asked: its exit follows the stage still on disk.
fn persist(store: &Store, rec: &mut OpRecord) -> CliResult<()> {
    let op = rec.op_id.clone();
    store.write(rec).map_err(|e| {
        by_stage_on_disk(
            store,
            &op,
            &format!("the state of deposit operation {op} could not be written: {e}"),
        )
    })
}

/// Ends the operation as failed at its current stage. The reason is
/// cleaned of secrets like everything the run prints, since a later run
/// prints it again. Whether the record reached the disk.
fn close(store: &Store, rec: &mut OpRecord, reason: FailReason, exit: ExitCode, why: &str) -> bool {
    let at = rec.stage;
    rec.fail(at, reason, exit, ui::redact(why));
    store.write(rec).is_ok()
}

/// A refusal before the deposit was requested: the operation is closed,
/// nothing was sent, and `e` is the answer. This run no longer drives an
/// operation an interrupt could report on.
fn closed_before_request(
    d: &Deps,
    store: &Store,
    rec: &mut OpRecord,
    reason: FailReason,
    e: CliError,
) -> CliError {
    if !close(store, rec, reason, e.exit_code(), &e.to_string()) {
        d.ui.warn(&format!(
            "operation {} could not be closed on disk; the next deposit run closes it",
            rec.op_id
        ));
    }
    *d.current_op.lock().unwrap_or_else(PoisonError::into_inner) = None;
    e
}

/// Once the operation is closed as failed on disk, that failure is the
/// run's answer: an interrupt from here on (while the wallet session is
/// being closed, say) must not report the operation as one to resume.
fn forget_if_closed(d: &Deps, store: &Store, op: &str) {
    if store.load(op).is_ok_and(|r| r.stage == OpStage::Failed) {
        *d.current_op.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// The exits other than 2 that a closed operation can have recorded.
const RECORDED_EXITS: [ExitCode; 11] = [
    ExitCode::DuplicateRefused,
    ExitCode::WalletFailed,
    ExitCode::ApproveFailed,
    ExitCode::DepositReverted,
    ExitCode::DepositOutcomeUnknown,
    ExitCode::AnWaitTimeout,
    ExitCode::DepositProofFailed,
    ExitCode::FinalizeRefused,
    ExitCode::CreditUnconfirmed,
    ExitCode::DepositUnprovable,
    ExitCode::CreditAborted,
];

/// What a run of an operation closed as failed answers: the message and
/// the exit code of the run that closed it (20 for a rejection in the
/// wallet, 21 for a failed approve, 22 for a revert, 35 for a deposit
/// other than the one requested, and so on), never a generic refusal.
fn terminal(rec: &OpRecord) -> CliError {
    let op = &rec.op_id;
    let Some(f) = &rec.failure else {
        return CliError::Preflight {
            reason: format!("operation {op} failed without a recorded reason; nothing to continue"),
            source: None,
        };
    };
    let stage = match f.reason {
        FailReason::Interrupted => Stage::Preflight,
        FailReason::Refused => Stage::Wallet,
        FailReason::ApproveFailed => Stage::Approve,
        FailReason::CreditAborted => Stage::Credit,
        FailReason::Rejected
        | FailReason::NonceConsumed
        | FailReason::Reverted
        | FailReason::Mismatch
        | FailReason::Unprovable => Stage::Deposit,
    };
    let said = format!("operation {op} has ended: {}", f.detail);
    match RECORDED_EXITS.into_iter().find(|c| c.as_i32() == f.exit) {
        Some(code) => err(code, stage, op, said),
        None => CliError::Preflight {
            reason: said,
            source: None,
        },
    }
}

/// `e`, a record of operation `op` that exists and cannot be read: how far
/// the operation got is unknown, which is exit 34, never "nothing was
/// sent".
fn unreadable(op: &str, e: CliError) -> CliError {
    let why = match e {
        CliError::Preflight {
            reason, ..
        } => reason,
        other => other.to_string(),
    };
    err(
        ExitCode::CreditUnconfirmed,
        Stage::Deposit,
        op,
        format!(
            "{why}; how far the operation got is unknown. Restore its record, then run --resume \
             {op} again"
        ),
    )
}

/// A 32-byte id stored in a record as 64 hex digits; `None` for a
/// damaged record.
fn hex32(s: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    hex::decode_to_slice(s.trim_start_matches("0x"), &mut out).ok()?;
    Some(out)
}

/// A record the driver cannot go on with.
fn damaged(store: &Store, op: &str, what: &str) -> CliError {
    by_stage_on_disk(
        store,
        op,
        &format!("the record of operation {op} is damaged: {what}"),
    )
}

/// The step board as the driver walks it. It remembers the step that is
/// running, so that a run ending in an error marks that step failed.
struct Board<'a> {
    /// Where the steps are shown.
    ui: &'a dyn Ui,
    /// The step started last and not finished yet.
    running: Mutex<Option<StepId>>,
}

impl<'a> Board<'a> {
    /// A board with no step running.
    fn new(ui: &'a dyn Ui) -> Self {
        Board {
            ui,
            running: Mutex::new(None),
        }
    }

    /// The running step, even after a panic elsewhere.
    fn running(&self) -> MutexGuard<'_, Option<StepId>> {
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `s` starts.
    fn start(&self, s: StepId, detail: &str) {
        *self.running() = Some(s);
        self.ui.step(s, StepState::Running, detail);
    }

    /// `s` ended in `state`.
    fn end(&self, s: StepId, state: StepState, detail: &str) {
        {
            let mut r = self.running();
            if *r == Some(s) {
                *r = None;
            }
        }
        self.ui.step(s, state, detail);
    }

    /// `s` is done.
    fn done(&self, s: StepId, detail: &str) {
        self.end(s, StepState::Done, detail);
    }

    /// `s` is not needed on this run.
    fn skip(&self, s: StepId, detail: &str) {
        self.end(s, StepState::Skipped, detail);
    }

    /// `s` has to be done again later.
    fn again(&self, s: StepId, detail: &str) {
        self.end(s, StepState::Pending, detail);
    }

    /// The run ends with `e`: the running step failed.
    fn failed(&self, e: &CliError) {
        let running = self.running().take();
        if let Some(s) = running {
            self.ui.step(
                s,
                StepState::Failed,
                &format!("exit {}", e.exit_code().as_i32()),
            );
        }
    }

    /// `r`, after marking the running step failed if it is an error.
    fn answer<T>(&self, r: CliResult<T>) -> CliResult<T> {
        if let Err(e) = &r {
            self.failed(e);
        }
        r
    }
}

/// A new deposit, or a dry run, through `wallet`. See [`run_fresh`].
pub async fn run_with(
    p: &DepositParams,
    d: &Deps,
    wallet: &mut dyn Wallet,
) -> CliResult<DepositSuccess> {
    run_fresh(p, d, wallet).await.map_err(|f| *f.error)
}

/// Steps 1 to 4 of a new deposit, then [`drive`]. The directory lock is
/// held from before the preflight's last check until the deposit is
/// confirmed on chain or the operation is closed; the operation is
/// created, and its own lock taken, before the wallet is asked for
/// anything. A refusal before the deposit is requested closes the
/// operation as refused.
pub async fn run_fresh(
    p: &DepositParams,
    d: &Deps,
    wallet: &mut dyn Wallet,
) -> Result<DepositSuccess, FreshError> {
    let ui = d.ui.as_ref();
    let board = Board::new(ui);
    let (Some(net), Some(amount), Some(to), Some(bridge_acc), Some(rpc_url)) = (
        p.network,
        p.amount,
        p.to,
        p.usdc_bridge_account,
        p.rpc_url.clone(),
    ) else {
        return Err(CliError::Usage {
            reason: "deposit: a new deposit needs --network, --amount, --to, --rpc-url and \
                     --usdc-bridge-account"
                .into(),
        }
        .into());
    };
    let store = Store::open(&p.state_dir)?;
    let Some(dir_lock) = DirLock::try_take(&p.state_dir)? else {
        let busy = store.list().ok().and_then(|recs| {
            recs.into_iter()
                .rev()
                .find(|r| r.is_unresolved() || r.stage == OpStage::Reserved)
                .map(|r| r.op_id)
        });
        return Err(CliError::deposit(
            ExitCode::DuplicateRefused,
            Stage::Preflight,
            busy.as_deref(),
            format!(
                "another deposit run on this machine holds {}{}: one run at a time may be between \
                 its preflight and the on-chain confirmation of its deposit. Wait for it to get \
                 past that point (nothing was sent)",
                p.state_dir.display(),
                busy.as_deref()
                    .map(|b| format!(" (operation {b})"))
                    .unwrap_or_default()
            ),
        )
        .into());
    };

    // Step 1.
    board.start(StepId::Preflight, "");
    let checked = board.answer(preflight::check(p, d, &store, p.from_address).await)?;
    let writer = match checked.plan {
        AnchorPlan::LightClient => "the light client",
        AnchorPlan::Owner {
            ..
        } => "the bridge owner",
    };
    board.done(
        StepId::Preflight,
        &format!("bridge {}, anchored by {writer}", checked.an.version),
    );
    if p.mode == RunMode::DryRun {
        return Ok(preflight::dry_run_summary(p, &checked));
    }

    // The operation, before the wallet is asked for anything.
    let mut rec = OpRecord::new(
        Store::new_op_id(),
        checked.params.clone(),
        hex::encode(checked.an.voucher_code_hash),
    );
    let op = rec.op_id.clone();
    let Some(op_lock) = OpLock::try_take(&p.state_dir, &op)? else {
        return Err(CliError::Preflight {
            reason: format!("operation {op} is locked by another process (nothing was sent)"),
            source: None,
        }
        .into());
    };
    rec.work_dir = Some(p.work_dir.join(&op));
    rec.an_bridge_dapp = Some(hex::encode(checked.an.bridge_dapp));
    store.write(&mut rec).map_err(nothing_sent)?;
    ui.op_id(&op);
    // Only now: an interrupt reports on an operation that is on disk.
    *d.current_op.lock().unwrap_or_else(PoisonError::into_inner) = Some(op.clone());
    let cx = RunCx {
        plan: checked.plan,
        bridge_acc,
        bridge_dapp: checked.an.bridge_dapp,
        light_client: checked.an.light_client,
        prover: checked.prover,
        rpc_url,
        usdc: checked.evm.usdc,
    };

    // Step 2.
    board.start(StepId::Pair, "scan the QR code with your wallet");
    let from = match wallet.connect(ui).await {
        Ok(a) => a,
        Err(e) => {
            let e = closed_before_request(
                d,
                &store,
                &mut rec,
                FailReason::Refused,
                err(
                    ExitCode::WalletFailed,
                    Stage::Wallet,
                    &op,
                    format!("the wallet did not pair: {e} (nothing was sent)"),
                ),
            );
            board.failed(&e);
            return Err(FreshError {
                error: Box::new(e),
                pairing_failed: true,
            });
        },
    };
    let ask = Ask {
        amount: &amount,
        net,
        to: &to,
        from,
    };
    if let Err(e) = request(p, d, &board, &store, &mut rec, &cx, wallet, ask).await {
        forget_if_closed(d, &store, &op);
        wallet.close().await;
        board.failed(&e);
        return Err(e.into());
    }
    drive(
        p,
        d,
        &store,
        &mut rec,
        &cx,
        Some(wallet),
        Some(dir_lock),
        op_lock,
    )
    .await
    .map_err(FreshError::from)
}

/// What a new deposit asks the wallet for, and from whom.
struct Ask<'a> {
    /// The amount as typed, for the account check's message.
    amount: &'a UsdcAmount,
    /// The network.
    net: Network,
    /// The recipient.
    to: &'a AnTarget,
    /// The paired account.
    from: Address,
}

/// Steps 2 to 4 after the pairing: the sender's checks, the allowance and
/// the deposit request, until the wallet's transaction is bound to the
/// operation. A refusal before the request closes the operation; the
/// caller ends the wallet session on any error.
#[allow(clippy::too_many_arguments)]
async fn request(
    p: &DepositParams,
    d: &Deps,
    board: &Board<'_>,
    store: &Store,
    rec: &mut OpRecord,
    cx: &RunCx,
    wallet: &mut dyn Wallet,
    ask: Ask<'_>,
) -> CliResult<()> {
    let ui = d.ui.as_ref();
    let evm = d.evm.as_ref();
    let op = rec.op_id.clone();
    let (from, bridge, units) = (ask.from, rec.params.bridge, rec.params.amount_units);
    // Under the directory lock, like the creation of the operation: no
    // other run can ask the wallet between this check and this request.
    let recs = store.list()?;
    if let Some(b) = blocking_by_sender(store, &recs, rec.params.chain_id, bridge, from, &op)? {
        let e = CliError::deposit(
            ExitCode::DuplicateRefused,
            Stage::Wallet,
            Some(&b.op_id),
            b.to_string(),
        );
        return Err(closed_before_request(d, store, rec, FailReason::Refused, e));
    }
    rec.from = Some(from);
    store.write(rec).map_err(nothing_sent)?;
    let bal = transient(ui, "reading the USDC balance", || {
        read_balance(evm, cx.usdc, from)
    })
    .await;
    if bal < U256::from(units) {
        let e = CliError::Preflight {
            reason: format!(
                "{from} holds {bal} USDC units and the deposit needs {units} (nothing was sent)"
            ),
            source: None,
        };
        return Err(closed_before_request(d, store, rec, FailReason::Refused, e));
    }
    if let Err(e) =
        account_check::check(evm, wallet, ui, from, &op, bridge, ask.amount, ask.net).await
    {
        return Err(closed_before_request(d, store, rec, FailReason::Refused, e));
    }
    board.done(StepId::Pair, &format!("{from}"));

    // Step 3.
    board.start(StepId::Approve, "");
    match ensure_allowance(
        evm,
        wallet,
        ui,
        cx.usdc,
        bridge,
        from,
        units,
        &op,
        p.pair_timeout,
        d.polls.wallet,
    )
    .await
    {
        Ok(ApproveOutcome::Skipped) => {
            board.skip(StepId::Approve, "the allowance already covers the amount")
        },
        Ok(ApproveOutcome::Approved {
            tx,
        }) => board.done(
            StepId::Approve,
            &tx.map(|h| format!("{h:#x}")).unwrap_or_default(),
        ),
        Err(e) => {
            return Err(closed_before_request(
                d,
                store,
                rec,
                FailReason::ApproveFailed,
                e,
            ))
        },
    }

    // Step 4. A deposit the node says would revert, or a bridge paused
    // since the preflight, is refused before the request and closes the
    // operation (evm_steps).
    board.start(StepId::Deposit, "confirm the deposit in your wallet");
    let tx = build_deposit_request(
        evm,
        ui,
        store,
        rec,
        from,
        bridge,
        units,
        ask.to.account_b256(),
    )
    .await?;
    let claim = match request_deposit(evm, wallet, ui, store, rec, &tx).await? {
        RequestOutcome::Signed(c) => c,
        RequestOutcome::Search => {
            bind_by_search(d, store, rec, Some(p.recovery_window), false).await?
        },
    };
    board.done(StepId::Deposit, &format!("{:#x}", claim.tx_hash));
    Ok(())
}

/// Looks for the operation's transaction on chain for at most `window`
/// (`None`: until there is an answer; see `recovery`), and binds what it
/// finds: the record becomes `Signed` with the claim the search itself
/// read. With `relock` the directory lock is taken for the check of the
/// claims and the write; a caller that holds it passes `false`.
pub async fn bind_by_search(
    d: &Deps,
    store: &Store,
    rec: &mut OpRecord,
    window: Option<Duration>,
    relock: bool,
) -> CliResult<TxClaim> {
    let ui = d.ui.as_ref();
    let op = rec.op_id.clone();
    let (chain_id, bridge) = (rec.params.chain_id, rec.params.bridge);
    let listed = || store.list().map_err(|e| after_request(store, &op, e));
    let others = claims_of_others(&listed()?, &op, chain_id, bridge);
    let found = recovery::search(
        d.evm.as_ref(),
        rec,
        &others,
        bridge,
        window,
        ui,
        d.polls.evm,
    )
    .await
    .map_err(|e| {
        err(
            ExitCode::DepositOutcomeUnknown,
            Stage::Deposit,
            &op,
            format!("{e:#}; continue with --resume {op}"),
        )
    })?;
    match found {
        SearchOutcome::Bound {
            tx_hash,
            nonce,
        } => {
            // Claims are read and written under the directory lock.
            let _held = if relock {
                let lock = DirLock::wait(store.dir()).await;
                Some(lock.map_err(|e| after_request(store, &op, e))?)
            } else {
                None
            };
            if claims_of_others(&listed()?, &op, chain_id, bridge)
                .tx_hashes
                .contains(&tx_hash)
            {
                return Err(err(
                    ExitCode::DepositOutcomeUnknown,
                    Stage::Deposit,
                    &op,
                    format!(
                        "{tx_hash:#x} was just claimed by another operation; continue with \
                         --resume {op}"
                    ),
                ));
            }
            // The nonce is the one the search read: reading the
            // transaction again here would wait past the window if it left
            // the mempool meanwhile.
            let claim = TxClaim {
                tx_hash,
                tx_nonce: nonce,
            };
            rec.stage = OpStage::Signed;
            rec.tx = Some(claim);
            persist(store, rec)?;
            Ok(claim)
        },
        SearchOutcome::Ambiguous {
            hashes,
            why,
        } => Err(err(
            ExitCode::DepositOutcomeUnknown,
            Stage::Deposit,
            &op,
            format!(
                "{why}. Candidates: {}. Check them in the wallet and continue with `--resume {op} \
                 --tx-hash <hash>`",
                hashes
                    .iter()
                    .map(|h| format!("{h:#x}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
        SearchOutcome::NotFound => Err(err(
            ExitCode::DepositOutcomeUnknown,
            Stage::Deposit,
            &op,
            format!(
                "the deposit was requested and its transaction is not on chain yet; it may still \
                 be in flight. Continue with --resume {op}"
            ),
        )),
        SearchOutcome::NonceConsumed => {
            let why = "the wallet replaced this deposit's transaction (for example, cancelled it) \
                       in a finalized block; nothing was deposited";
            let at = rec.stage;
            rec.fail(
                at,
                FailReason::NonceConsumed,
                ExitCode::WalletFailed,
                ui::redact(why),
            );
            persist(store, rec)?;
            Err(err(ExitCode::WalletFailed, Stage::Deposit, &op, why))
        },
    }
}

/// The deposit as the bridge's `confirmDeposit` and `DepositFinalized`
/// name it: the proven inputs, the EVM bridge as a number.
fn credit_identity(rec: &OpRecord, dep: &DepositInfo, to: &AnTarget) -> Identity {
    Identity {
        deposit_id: dep.deposit_id,
        contract: U256::from_be_slice(rec.params.bridge.as_slice()),
        chain_id: rec.params.chain_id,
        amount: u128::from(rec.params.amount_units),
        account: to.account_id,
    }
}

/// The Acki Nacki bridge's `isPaused()`. An answer that is not a boolean
/// is a failed read.
async fn bridge_paused(an: &dyn AnRead, bridge: [u8; 32]) -> anyhow::Result<bool> {
    let v = an
        .run_getter(bridge, BRIDGE_ABI, "isPaused", json!({}))
        .await
        .context("could not read isPaused")?;
    v["value0"]
        .as_bool()
        .ok_or_else(|| anyhow!("isPaused answered {v}"))
}

/// The deposit block's time in seconds, which no DepositFinalized of this
/// deposit predates: one read within `deadline`, kept once known for that
/// block. Unknown is 0, and an event search then pages further back.
async fn block_time(
    d: &Deps,
    dep: &DepositInfo,
    deadline: Option<Instant>,
    known: &mut Option<(B256, u64)>,
) -> u64 {
    if let Some((h, t)) = *known {
        if h == dep.block_hash {
            return t;
        }
    }
    let t = once(deadline, d.evm.header_by_hash(dep.block_hash))
        .await
        .flatten()
        .map(|h| h.timestamp);
    *known = t.map(|t| (dep.block_hash, t));
    t.unwrap_or(0)
}

/// Whether somebody finalized the deposit already, and if so whether step
/// 9 confirms it by events (`Some(true)`) or through the voucher
/// (`Some(false)`). The voucher at the stored address shows it while the
/// bridge keeps the voucher code the address was computed with. After a
/// code change the deposit is finalized through another address, and only
/// the bridge's DepositFinalized events show it; they are read then, and
/// whenever the proof is missing (a new proof costs minutes). With a proof
/// on disk and an unchanged code, step 8's own check before every send is
/// enough.
#[allow(clippy::too_many_arguments)]
async fn finalized_already(
    d: &Deps,
    cx: &RunCx,
    stored_code: &str,
    voucher: [u8; 32],
    id: &Identity,
    have_proof: bool,
    not_before: u64,
    look: Option<Instant>,
) -> Option<bool> {
    let ui = d.ui.as_ref();
    let code = once(
        look,
        d.an.run_getter(
            cx.bridge_acc,
            BRIDGE_ABI,
            "getDepositVoucherCodeHash",
            json!({}),
        ),
    )
    .await;
    // Unknown counts as moved: then the events decide, not a stale address.
    let moved = code.and_then(|c| {
        c["value0"]
            .as_str()
            .map(|h| h.trim_start_matches("0x").to_ascii_lowercase())
    }) != Some(stored_code.trim_start_matches("0x").to_ascii_lowercase());
    if !moved {
        let v = once(look, d.an.account(voucher)).await.flatten();
        if v.is_some_and(|a| a.status == AccStatus::Active) {
            return Some(false);
        }
        if have_proof {
            return None;
        }
    }
    // The event is enough to stop proving and sending; step 9 follows the
    // rest of the chain to the delivered transfer.
    match until(
        ui,
        "reading the bridge's DepositFinalized events",
        look,
        || credit::finalized_event(d.an.as_ref(), cx.bridge_acc, id, not_before),
    )
    .await
    {
        Some(Some(_)) => Some(true),
        Some(None) => None,
        None => {
            ui.warn(if have_proof {
                "the bridge's DepositFinalized events could not be read; sending finalizeDeposit"
            } else {
                "the bridge's DepositFinalized events could not be read; building the proof again"
            });
            None
        },
    }
}

/// Step 5's negative verdict, in a finalized block: the operation is
/// closed, and the error says what to give the bridge operator.
async fn negative(d: &Deps, store: &Store, rec: &mut OpRecord, n: Negative) -> CliError {
    let op = rec.op_id.clone();
    let bridge = rec.params.bridge;
    let tx = rec.tx.map(|t| t.tx_hash);
    // Facts for the message only: one bounded read, after the verdict.
    let receipt = match tx {
        Some(h) => once(None, d.evm.receipt(h)).await.flatten(),
        None => None,
    };
    let dep_id = receipt.as_ref().and_then(|r| {
        r.logs
            .iter()
            .filter(|l| l.address == bridge)
            .find_map(parse_deposit_log)
            .map(|e| e.deposit_id.to_string())
    });
    let facts = format!(
        "tx {}, depositId {}, amount {} USDC units, bridge {bridge:#x}, operation {op}",
        tx.map(|h| format!("{h:#x}"))
            .unwrap_or_else(|| "none".into()),
        dep_id.as_deref().unwrap_or("none"),
        rec.params.amount_units,
    );
    let (reason, exit, what) = match &n {
        Negative::Reverted => {
            let why = match (rec.from, &receipt) {
                (Some(from), Some(r)) => {
                    let call = rec
                        .request
                        .as_ref()
                        .map(|q| q.calldata.clone())
                        .or_else(|| {
                            AnTarget::parse(&rec.params.to).ok().map(|t| {
                                deposit_calldata(rec.params.amount_units, t.account_b256())
                            })
                        });
                    match call {
                        Some(c) => once(None, d.evm.revert_reason(from, bridge, c, r.block_number))
                            .await
                            .flatten(),
                        None => None,
                    }
                },
                _ => None,
            };
            (
                FailReason::Reverted,
                ExitCode::DepositReverted,
                format!(
                    "the deposit transaction reverted on the EVM side{}: no USDC was taken, only \
                     gas was spent. Likely causes: InvalidAmount, DepositTooLarge, \
                     InvalidAnAccount, the allowance or the balance changed after the preflight, \
                     TransferAmountMismatch, BridgePaused (the owner paused the bridge after the \
                     check before the request)",
                    why.map(|w| format!(" ({w})")).unwrap_or_default()
                ),
            )
        },
        Negative::NoDeposit => (
            FailReason::Reverted,
            ExitCode::DepositReverted,
            "the transaction succeeded without a Deposit event of the bridge: nothing was \
             deposited and no USDC was taken, only gas was spent. The wallet signed something \
             other than the requested deposit"
                .to_string(),
        ),
        Negative::Mismatch(m) => (
            FailReason::Mismatch,
            ExitCode::DepositUnprovable,
            format!(
                "{m}. The CLI does not finalize a deposit other than the one requested; the USDC \
                 is in the EVM bridge, and its operator can finalize or return it"
            ),
        ),
        Negative::Unprovable(v) => (
            FailReason::Unprovable,
            ExitCode::DepositUnprovable,
            format!(
                "the deposit is on chain but cannot be proven: {v}. The USDC is in the EVM bridge \
                 and only its operator can return it"
            ),
        ),
    };
    let said = format!("{what}.\n  Give the bridge operator: {facts}");
    let recorded = close(store, rec, reason, exit, &said);
    err(
        exit,
        Stage::Deposit,
        &op,
        if recorded {
            said
        } else {
            format!("{said}\n  (the operation record could not be updated)")
        },
    )
}

/// `e` from the anchor wait or the proof. Once a `finalizeDeposit` was sent
/// (the record says Finalizing, and a 224 brought the run back here) that
/// send may have executed: a timeout or a failed proof is exit 34 then.
fn in_doubt(rec: &OpRecord, e: CliError) -> CliError {
    match e.exit_code() {
        ExitCode::AnWaitTimeout | ExitCode::DepositProofFailed
            if rec.stage == OpStage::Finalizing =>
        {
            err(
                ExitCode::CreditUnconfirmed,
                e.stage(),
                &rec.op_id,
                format!(
                    "{e}. A finalizeDeposit of this operation was sent before and its outcome is \
                     not known: it may have executed"
                ),
            )
        },
        _ => e,
    }
}

/// Where [`drive`] is. The record's stage says where the operation stands
/// on disk; the phase is where this run is. The two differ once step 8
/// has sent: after a 224 the run waits for the anchor again while the
/// record stays in Finalizing, so a resume knows the send may have
/// executed.
enum Phase {
    /// Step 5.
    Confirm,
    /// Step 6, entered at `since`, until `deadline` (`--anchor-timeout-s`).
    Anchor {
        /// When this wait began.
        since: Instant,
        /// When it ends with exit 31; `None` waits without end.
        deadline: Option<Instant>,
    },
    /// Step 7.
    Prove,
    /// Step 8, after a look at whether someone finalized the deposit.
    Finalize,
    /// Step 9.
    Credit {
        /// Only the bridge's events can confirm it: the stored voucher
        /// address is stale.
        via_events: bool,
    },
}

impl Phase {
    /// Step 6 from now, with a fresh `--anchor-timeout-s`.
    fn anchor(p: &DepositParams) -> Phase {
        let since = Instant::now();
        Phase::Anchor {
            since,
            deadline: p.anchor_timeout.and_then(|t| since.checked_add(t)),
        }
    }
}

/// Continues the operation from its recorded stage to the credit. The
/// wallet, when there is one, is asked for nothing more: its session is
/// closed once the deposit is confirmed, or when the run ends before that.
/// The directory lock, when held, is let go at the confirmation; the
/// operation's lock is released when this returns.
#[allow(clippy::too_many_arguments)]
pub async fn drive(
    p: &DepositParams,
    d: &Deps,
    store: &Store,
    rec: &mut OpRecord,
    cx: &RunCx,
    mut wallet: Option<&mut dyn Wallet>,
    dir_lock: Option<DirLock>,
    op_lock: OpLock,
) -> CliResult<DepositSuccess> {
    let board = Board::new(d.ui.as_ref());
    let r = walk(p, d, &board, store, rec, cx, &mut wallet, dir_lock).await;
    if r.is_err() {
        forget_if_closed(d, store, &rec.op_id);
    }
    if let Some(w) = wallet.take() {
        w.close().await;
    }
    drop(op_lock);
    board.answer(r)
}

/// The steps of [`drive`], one [`Phase`] at a time.
#[allow(clippy::too_many_arguments)]
async fn walk(
    p: &DepositParams,
    d: &Deps,
    board: &Board<'_>,
    store: &Store,
    rec: &mut OpRecord,
    cx: &RunCx,
    wallet: &mut Option<&mut dyn Wallet>,
    mut dir_lock: Option<DirLock>,
) -> CliResult<DepositSuccess> {
    let ui = d.ui.as_ref();
    let op = rec.op_id.clone();
    let mut phase = match rec.stage {
        // A finished operation is reported from its record alone.
        OpStage::Credited => return Ok(summary(rec, None, (None, None))),
        // A closed operation answers with what the run that closed it said.
        OpStage::Failed => return Err(terminal(rec)),
        OpStage::Reserved => {
            return Err(CliError::Preflight {
                reason: format!(
                    "operation {op} never asked the wallet; there is nothing to continue"
                ),
                source: None,
            })
        },
        OpStage::Requested | OpStage::Signed | OpStage::Abandoned => Phase::Confirm,
        OpStage::Confirmed => Phase::anchor(p),
        OpStage::Anchored => Phase::Prove,
        OpStage::Proved | OpStage::Finalizing => Phase::Finalize,
    };
    let to = AnTarget::parse(&rec.params.to).map_err(|e| after_request(store, &op, e))?;
    let from = rec.from.ok_or_else(|| damaged(store, &op, "no sender"))?;
    let exp = Expect {
        bridge: rec.params.bridge,
        from,
        calldata: rec
            .request
            .as_ref()
            .map(|q| q.calldata.clone())
            .unwrap_or_else(|| deposit_calldata(rec.params.amount_units, to.account_b256())),
        amount: rec.params.amount_units,
        account: to.account_b256(),
        confirmations: p.confirmations,
    };
    // Diagnostic only: one bounded read.
    let balance_before = once(None, d.an.account(to.account_id))
        .await
        .flatten()
        .map(|a| a.ecc3);
    let mut anchor_waited = None;
    let mut block_seen = None;
    loop {
        phase = match phase {
            Phase::Confirm => {
                board.start(StepId::EvmConfirm, "");
                if rec.tx.is_none() {
                    bind_by_search(d, store, rec, None, dir_lock.is_none()).await?;
                }
                let Some(claim) = rec.tx else {
                    return Err(damaged(store, &op, "no transaction is bound to it"));
                };
                let outcome =
                    evm_confirm::confirm(d.evm.as_ref(), claim.tx_hash, &exp, ui, d.polls.evm)
                        .await
                        .map_err(|e| {
                            err(
                                ExitCode::DepositOutcomeUnknown,
                                Stage::Deposit,
                                &op,
                                format!("{e:#}; continue with --resume {op}"),
                            )
                        })?;
                match outcome {
                    Outcome::Confirmed(f) => {
                        let id = DepositIdentity {
                            deposit_id: f.deposit_id,
                            contract: rec.params.bridge,
                            chain_id: rec.params.chain_id,
                        };
                        // Computed offline; a failure here comes after the
                        // deposit, so it is never exit 2.
                        let voucher = voucher_account_id(&offline_context(), &id)
                            .map_err(|e| after_request(store, &op, e))?;
                        rec.deposit = Some(DepositInfo {
                            deposit_id: f.deposit_id,
                            block_number: f.block_number,
                            block_hash: f.block_hash,
                            block_log_index: f.block_log_index,
                            receipt_log_index: f.receipt_log_index,
                            access_list_rlp_len: f.access_list_rlp_len,
                            voucher_account: hex::encode(voucher),
                        });
                        rec.stage = OpStage::Confirmed;
                        persist(store, rec)?;
                        // From here on the operation's own lock is enough,
                        // and the wallet has nothing more to sign.
                        dir_lock = None;
                        if let Some(w) = wallet.take() {
                            w.close().await;
                        }
                        board.done(StepId::EvmConfirm, &format!("depositId {}", f.deposit_id));
                        Phase::anchor(p)
                    },
                    Outcome::Final(n) => return Err(negative(d, store, rec, n).await),
                    Outcome::Lost => {
                        ui.warn(
                            "the deposit transaction left the chain (a reorg or a replacement); \
                             searching for it by its nonce",
                        );
                        bind_by_search(d, store, rec, None, dir_lock.is_none()).await?;
                        Phase::Confirm
                    },
                }
            },
            Phase::Anchor {
                since,
                deadline,
            } => {
                let (Some(dep), Some(claim)) = (rec.deposit.clone(), rec.tx) else {
                    return Err(damaged(store, &op, "no confirmed deposit"));
                };
                let voucher = hex32(&dep.voucher_account)
                    .ok_or_else(|| damaged(store, &op, "the voucher address"))?;
                board.start(StepId::Anchor, "");
                let wc = WaitCtx {
                    tx_hash: claim.tx_hash,
                    block_hash: dep.block_hash,
                    block_number: dep.block_number,
                    chain_id: rec.params.chain_id,
                    bridge: cx.bridge_acc,
                    voucher,
                    light_client: cx.light_client,
                    plan: cx.plan,
                    timeout: deadline.map(|t| t.saturating_duration_since(Instant::now())),
                    grace: p.relayer_grace,
                    poll: d.polls.anchor,
                    op_id: op.clone(),
                };
                let waited = anchor_wait::wait(d.evm.as_ref(), d.an.as_ref(), &wc, ui)
                    .await
                    .map_err(|e| in_doubt(rec, e))?;
                match waited {
                    WaitExit::Proceed => {
                        // The wait read the pause before the relayer's grace
                        // period; the owner may have paused the bridge since.
                        // The read gets at least one try past the deadline:
                        // the grace period is not the anchor wait's time.
                        let look = deadline.map(|t| t.max(Instant::now() + ONE_READ));
                        match until(ui, "reading the bridge pause flag", look, || {
                            bridge_paused(d.an.as_ref(), cx.bridge_acc)
                        })
                        .await
                        {
                            None => {
                                return Err(in_doubt(
                                    rec,
                                    err(
                                        ExitCode::AnWaitTimeout,
                                        Stage::Anchor,
                                        &op,
                                        format!(
                                            "the deposit is on the EVM bridge; whether the Acki \
                                             Nacki bridge is paused could not be read before \
                                             --anchor-timeout-s ran out. Continue with --resume \
                                             {op}"
                                        ),
                                    ),
                                ))
                            },
                            Some(true) => {
                                ui.warn(
                                    "the bridge was paused by its owner after the block was \
                                     anchored; waiting for the pause to be lifted before proving",
                                );
                                Phase::Anchor {
                                    since,
                                    deadline,
                                }
                            },
                            Some(false) => {
                                anchor_waited = Some(since.elapsed());
                                rec.anchor_writer = Some(writer_of(cx.plan).into());
                                // After a send the record stays in Finalizing.
                                if rec.stage != OpStage::Finalizing {
                                    rec.stage = OpStage::Anchored;
                                }
                                persist(store, rec)?;
                                board.done(StepId::Anchor, &format!("{:#x}", dep.block_hash));
                                Phase::Prove
                            },
                        }
                    },
                    WaitExit::VoucherDeployed => {
                        anchor_waited = Some(since.elapsed());
                        rec.anchor_writer = Some(writer_of(cx.plan).into());
                        // Somebody finalized it: a resume goes to the checks
                        // before a send, which find the voucher, and on.
                        rec.stage = OpStage::Finalizing;
                        let info = FinalizeInfo {
                            voucher_code_hash: rec.voucher_code_hash.clone(),
                            voucher_account: dep.voucher_account.clone(),
                            sends: 0,
                        };
                        rec.finalize.get_or_insert(info);
                        persist(store, rec)?;
                        let why = "the operator's relayer finalized it";
                        board.done(StepId::Anchor, &format!("{:#x}", dep.block_hash));
                        board.skip(StepId::Prove, why);
                        board.skip(StepId::Finalize, why);
                        Phase::Credit {
                            via_events: false,
                        }
                    },
                    back @ (WaitExit::BackToConfirm | WaitExit::Search) => {
                        ui.warn(if back == WaitExit::BackToConfirm {
                            "the deposit's transaction moved to another block; confirming it again"
                        } else {
                            "the node no longer shows the deposit's receipt; searching for its \
                             transaction"
                        });
                        // Back to an unknown outcome: under the directory
                        // lock, like the first confirmation.
                        if dir_lock.is_none() {
                            let lock = DirLock::wait(&p.state_dir).await;
                            dir_lock = Some(lock.map_err(|e| after_request(store, &op, e))?);
                        }
                        rec.stage = OpStage::Signed;
                        rec.deposit = None;
                        persist(store, rec)?;
                        board.again(StepId::Anchor, "");
                        Phase::Confirm
                    },
                }
            },
            Phase::Prove => {
                let (Some(dep), Some(claim)) = (rec.deposit.clone(), rec.tx) else {
                    return Err(damaged(store, &op, "no confirmed deposit"));
                };
                board.start(StepId::Prove, "");
                let work = rec.work_dir.clone().unwrap_or_else(|| p.work_dir.join(&op));
                let want = ExpectedInputs {
                    deposit_id: dep.deposit_id,
                    sender: from,
                    amount: rec.params.amount_units,
                    contract: rec.params.bridge,
                    chain_id: rec.params.chain_id,
                    dapp_id: to.dapp_id,
                    account_id: to.account_id,
                    block_hash: dep.block_hash,
                };
                // A proof already on disk for this very deposit (after a 224
                // and a new anchor, or on a resume) is used again: same
                // block, same inputs.
                let reusable = prover::load(&work).is_some_and(|f| {
                    DepositPublicInputs::decode(&f.public_inputs)
                        .is_ok_and(|pi| pi::verify(&pi, &want).is_ok())
                });
                if !reusable {
                    let req = ProveRequest {
                        rpc_url: cx.rpc_url.clone(),
                        tx_hash: claim.tx_hash,
                        bridge: rec.params.bridge,
                        receipt_log_index: dep.receipt_log_index,
                        // Hex of the recipient's dapp bytes, never the text typed.
                        dapp_hex: to.dapp_hex(),
                        chain_id: rec.params.chain_id,
                        work,
                    };
                    // A failure — Ctrl-C reaching the prover too — leaves the
                    // operation where it is: a resume proves again.
                    prover::prove(&cx.prover, &req, &want, p.prover_timeout, ui, &op)
                        .await
                        .map_err(|e| in_doubt(rec, e))?;
                }
                if rec.stage != OpStage::Finalizing {
                    rec.stage = OpStage::Proved;
                    persist(store, rec)?;
                }
                board.done(
                    StepId::Prove,
                    if reusable {
                        "the proof on disk matches the deposit"
                    } else {
                        ""
                    },
                );
                Phase::Finalize
            },
            Phase::Finalize => {
                let Some(dep) = rec.deposit.clone() else {
                    return Err(damaged(store, &op, "no confirmed deposit"));
                };
                let voucher = hex32(&dep.voucher_account)
                    .ok_or_else(|| damaged(store, &op, "the voucher address"))?;
                let stored_code = hex32(&rec.voucher_code_hash)
                    .ok_or_else(|| damaged(store, &op, "the voucher code hash"))?;
                let work = rec.work_dir.clone().unwrap_or_else(|| p.work_dir.join(&op));
                let files = prover::load(&work);
                // With a proof on disk this is step 8, and its deadline
                // starts now: the reads before the send count against it
                // too. Without one, the look below decides whether to prove
                // again, within --credit-timeout-s.
                let look = if files.is_some() {
                    p.anchor_timeout.and_then(|t| Instant::now().checked_add(t))
                } else {
                    Instant::now().checked_add(p.credit_timeout)
                };
                let not_before = block_time(d, &dep, look, &mut block_seen).await;
                let id = credit_identity(rec, &dep, &to);
                // Before the send's own checks: a paused bridge must not hold
                // a deposit that is finalized already, and a missing proof is
                // not built again for one.
                let already = finalized_already(
                    d,
                    cx,
                    &rec.voucher_code_hash,
                    voucher,
                    &id,
                    files.is_some(),
                    not_before,
                    look,
                )
                .await;
                match (already, files) {
                    // Step 9 reads it again and answers for it: exit 0 or 37.
                    (Some(via_events), _) => {
                        board.skip(StepId::Finalize, "the deposit is finalized already");
                        Phase::Credit {
                            via_events,
                        }
                    },
                    (None, None) => Phase::Prove,
                    (None, Some(files)) => {
                        board.start(StepId::Finalize, "");
                        let fc = FinCtx {
                            bridge: cx.bridge_acc,
                            bridge_dapp: cx.bridge_dapp,
                            voucher,
                            stored_code_hash: stored_code,
                            identity: id,
                            not_before,
                            deadline: look,
                            poll: d.polls.anchor,
                        };
                        let an = d.an.as_ref();
                        match finalize::finalize(
                            an,
                            an,
                            store,
                            rec,
                            &fc,
                            &files.proof,
                            &files.public_inputs,
                            ui,
                        )
                        .await?
                        {
                            FinalizeExit::ToCredit {
                                via_events,
                            } => {
                                board.done(StepId::Finalize, "");
                                Phase::Credit {
                                    via_events,
                                }
                            },
                            FinalizeExit::BackToAnchor => {
                                ui.warn(
                                    "the bridge no longer accepts the deposit's block (224); \
                                     waiting for the anchor again",
                                );
                                board.again(StepId::Finalize, "");
                                Phase::anchor(p)
                            },
                        }
                    },
                }
            },
            Phase::Credit {
                via_events,
            } => {
                let Some(dep) = rec.deposit.clone() else {
                    return Err(damaged(store, &op, "no confirmed deposit"));
                };
                let voucher = hex32(&dep.voucher_account)
                    .ok_or_else(|| damaged(store, &op, "the voucher address"))?;
                board.start(StepId::Credit, "");
                let id = credit_identity(rec, &dep, &to);
                // Every read of step 9 is inside --credit-timeout-s, the
                // block time's included.
                let started = Instant::now();
                let not_before = block_time(
                    d,
                    &dep,
                    started.checked_add(p.credit_timeout),
                    &mut block_seen,
                )
                .await;
                let left = p.credit_timeout.saturating_sub(started.elapsed());
                match credit::confirm(
                    d.an.as_ref(),
                    cx.bridge_acc,
                    voucher,
                    &id,
                    via_events,
                    not_before,
                    left,
                    d.polls.credit,
                    &op,
                    ui,
                )
                .await
                {
                    Ok(c) => {
                        let detail = format!("confirmDeposit {}", c.confirm_tx);
                        rec.credit = Some(c);
                        rec.stage = OpStage::Credited;
                        persist(store, rec)?;
                        board.done(StepId::Credit, &detail);
                        // Diagnostic only: one bounded read.
                        let after = once(None, d.an.account(to.account_id))
                            .await
                            .flatten()
                            .map(|a| a.ecc3);
                        return Ok(summary(rec, anchor_waited, (balance_before, after)));
                    },
                    Err(e) if e.exit_code() == ExitCode::CreditAborted => {
                        // Not resumable: the voucher is spent.
                        if close(
                            store,
                            rec,
                            FailReason::CreditAborted,
                            ExitCode::CreditAborted,
                            &e.to_string(),
                        ) {
                            return Err(e);
                        }
                        return Err(err(
                            ExitCode::CreditAborted,
                            Stage::Credit,
                            &op,
                            format!("{e}\n  (the operation record could not be updated)"),
                        ));
                    },
                    Err(e) => return Err(e),
                }
            },
        };
    }
}

/// How the summary names who anchored the block.
fn writer_of(plan: AnchorPlan) -> &'static str {
    match plan {
        AnchorPlan::LightClient => "light-client",
        AnchorPlan::Owner {
            ..
        } => "owner",
    }
}

/// The summary of an operation, from its record alone: a finished
/// operation is reported without reaching either chain. `anchor_waited`
/// and `balance` are what this run saw, if anything.
pub fn summary(
    rec: &OpRecord,
    anchor_waited: Option<Duration>,
    balance: (Option<u128>, Option<u128>),
) -> DepositSuccess {
    let net = Network::from_chain_id(rec.params.chain_id);
    DepositSuccess {
        op_id: Some(rec.op_id.clone()),
        dry_run: false,
        network: net.map(|n| n.name().to_string()).unwrap_or_default(),
        chain_id: rec.params.chain_id,
        amount: UsdcAmount(u128::from(rec.params.amount_units)).display(),
        to: rec.params.to.clone(),
        deposit: rec.deposit.as_ref().map(|d| {
            json!({
                "tx_hash": rec.tx.map(|t| format!("{:#x}", t.tx_hash)),
                "deposit_id": d.deposit_id.to_string(),
                "block_number": d.block_number,
                "block_hash": format!("{:#x}", d.block_hash),
                "block_log_index": d.block_log_index,
                "receipt_log_index": d.receipt_log_index,
            })
        }),
        anchor: Some(json!({
            "writer": rec.anchor_writer,
            "waited_ms": anchor_waited.map(|w| u64::try_from(w.as_millis()).unwrap_or(u64::MAX)),
        })),
        tx: Some(json!({
            "type": 2,
            "calldata_len": DEPOSIT_CALLDATA_LEN,
            "access_list_len": rec.deposit.as_ref().map(|d| d.access_list_rlp_len),
        })),
        confirmation: rec.credit.as_ref().zip(rec.deposit.as_ref()).map(|(c, d)| {
            json!({
                "voucher": format!(
                    "{}::{}",
                    rec.an_bridge_dapp.as_deref().unwrap_or("-"),
                    d.voucher_account
                ),
                "voucher_code_hash": format!("0x{}", rec.voucher_code_hash),
                "confirm_tx": c.confirm_tx,
                "delivery_tx": c.delivery_tx,
                "event": "DepositFinalized",
                "via_events": c.via_events,
            })
        }),
        balance: Some(json!({
            "before": balance.0.map(|b| b.to_string()),
            "after": balance.1.map(|b| b.to_string()),
        })),
        abandoned: false,
    }
}

// ---- --resume, --tx-hash, --abandon ----

/// Refuses a command line that contradicts the operation's record: another
/// EVM network or bridge, another Acki Nacki bridge, or another Acki Nacki
/// network (scheme, host and port of `--gql-endpoint`). Under another
/// profile a resume could send the deposit's `finalizeDeposit` to a second
/// Acki Nacki bridge that trusts the same EVM bridge, and the deposit would
/// be minted twice. Exit 2, before anything is done.
pub fn check_flags_against(p: &DepositParams, rec: &OpRecord) -> CliResult<()> {
    let mut clash = Vec::new();
    if let Some(n) = p.network {
        if n.chain_id() != rec.params.chain_id {
            clash.push(format!(
                "--network is chain {}, the operation is on chain {}",
                n.chain_id(),
                rec.params.chain_id
            ));
        }
    }
    if let Some(b) = p.bridge {
        if b != rec.params.bridge {
            clash.push(format!(
                "--bridge-address is {b}, the operation was made to bridge {}",
                rec.params.bridge
            ));
        }
    }
    if let Some(acc) = p.usdc_bridge_account {
        if !hex::encode(acc).eq_ignore_ascii_case(&rec.params.an_bridge) {
            clash.push(format!(
                "--usdc-bridge-account is {}, the operation was made to Acki Nacki bridge {}",
                hex::encode(acc),
                rec.params.an_bridge
            ));
        }
    }
    if let Some(g) = &p.gql_endpoint {
        let n = an_network_id(g)?;
        if n != rec.params.an_network {
            clash.push(format!(
                "--gql-endpoint is on {n}, the operation was made on Acki Nacki network {}",
                rec.params.an_network
            ));
        }
    }
    if clash.is_empty() {
        return Ok(());
    }
    Err(CliError::Preflight {
        reason: format!(
            "operation {} does not match this command line: {}. Use the profile it was made with \
             (nothing was sent)",
            rec.op_id,
            clash.join("; ")
        ),
        source: None,
    })
}

/// The operation `target` names: an operation id whose record is in the
/// state directory, or the one operation of this bridge (and of
/// `--network`, when given) that recorded the deposit id. Anything else is
/// exit 2: nothing about it is known here.
fn find_op(p: &DepositParams, store: &Store, target: &OpRef) -> CliResult<String> {
    let refuse = |reason: String| CliError::Preflight {
        reason,
        source: None,
    };
    match target {
        OpRef::Op(id) if store.record_path(id).exists() => Ok(id.clone()),
        OpRef::Op(id) => Err(refuse(format!(
            "no deposit operation {id} in {} (nothing was sent)",
            store.dir().display()
        ))),
        OpRef::DepositId(n) => {
            let bridge = p.bridge.ok_or_else(|| CliError::Usage {
                reason: "deposit: --resume <depositId> needs --bridge-address".into(),
            })?;
            let recs = store.list()?;
            let hits: Vec<&OpRecord> = recs
                .iter()
                .filter(|r| {
                    r.params.bridge == bridge
                        && p.network
                            .is_none_or(|net| net.chain_id() == r.params.chain_id)
                })
                .filter(|r| r.deposit.as_ref().is_some_and(|x| x.deposit_id == *n))
                .collect();
            match hits.as_slice() {
                [one] => Ok(one.op_id.clone()),
                [] => Err(refuse(format!(
                    "no operation in {} recorded depositId {n} of bridge {bridge} (nothing was \
                     sent)",
                    store.dir().display()
                ))),
                many => Err(refuse(format!(
                    "depositId {n} is recorded by operations {}; resume one of them by its id",
                    many.iter()
                        .map(|r| r.op_id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))),
            }
        },
    }
}

/// An operation a resume goes on with: locked by this process, its record
/// read and checked against the command line.
struct Opened {
    /// The state directory.
    store: Store,
    /// The operation id.
    op: String,
    /// The operation's own lock, held until the resume ends.
    lock: OpLock,
    /// The record as it stands on disk.
    rec: OpRecord,
}

/// What the record alone says about the operation a resume names. Both
/// are boxed: each is built once, and neither is small.
enum Resumable {
    /// Credited: its summary.
    Finished(Box<DepositSuccess>),
    /// Somewhere between the deposit request and the credit.
    Open(Box<Opened>),
}

/// The part of a resume that asks neither chain: finds and locks the
/// operation, reads its record, checks the command line against it, and
/// answers what the record answers alone. A credited operation answers
/// with its summary, a failed one with its recorded code and message, and
/// a reservation whose run died is closed as interrupted (exit 2: its
/// wallet was never asked). `current_op` names the operation from the lock
/// on, before its record is read, so that an interrupt reports on the
/// record as it stands.
async fn open_for_resume(
    p: &DepositParams,
    target: &OpRef,
    ui: &dyn Ui,
    current_op: &Mutex<Option<String>>,
) -> CliResult<Resumable> {
    let store = Store::open(&p.state_dir)?;
    let op = find_op(p, &store, target)?;
    let Some(lock) = OpLock::try_take(&p.state_dir, &op)? else {
        return Err(CliError::deposit(
            ExitCode::DuplicateRefused,
            Stage::Preflight,
            Some(&op),
            format!(
                "operation {op} is being run by another process (nothing was sent by this run)"
            ),
        ));
    };
    let forget = || *current_op.lock().unwrap_or_else(PoisonError::into_inner) = None;
    *current_op.lock().unwrap_or_else(PoisonError::into_inner) = Some(op.clone());
    ui.op_id(&op);
    let mut rec = store.load(&op).map_err(|e| unreadable(&op, e))?;
    check_flags_against(p, &rec)?;
    match rec.stage {
        // Neither chain is asked for a finished operation.
        OpStage::Credited => {
            return Ok(Resumable::Finished(Box::new(summary(
                &rec,
                None,
                (None, None),
            ))))
        },
        OpStage::Failed => {
            forget();
            return Err(terminal(&rec));
        },
        OpStage::Reserved => {
            // Its lock was free: the run that reserved it is gone, and it
            // never asked the wallet. Closed as a new deposit's preflight
            // closes such an operation, under the directory lock.
            let _dir = DirLock::wait(store.dir()).await?;
            rec.fail(
                OpStage::Reserved,
                FailReason::Interrupted,
                ExitCode::PreflightRefused,
                "the run that reserved it exited before asking the wallet",
            );
            store.write(&mut rec).map_err(nothing_sent)?;
            forget();
            return Err(CliError::Preflight {
                reason: format!(
                    "operation {op} never asked the wallet, so there is nothing to continue; it \
                     is closed now (nothing was sent)"
                ),
                source: None,
            });
        },
        _ => {},
    }
    Ok(Resumable::Open(Box::new(Opened {
        store,
        op,
        lock,
        rec,
    })))
}

/// The steps a run before this one finished, marked done on a resume's
/// board so that it starts where the operation is.
fn done_earlier(board: &Board<'_>, stage: OpStage) {
    let reached = match stage {
        OpStage::Requested | OpStage::Abandoned => StepId::Deposit,
        OpStage::Signed => StepId::EvmConfirm,
        OpStage::Confirmed => StepId::Anchor,
        OpStage::Anchored => StepId::Prove,
        OpStage::Proved | OpStage::Finalizing => StepId::Finalize,
        OpStage::Reserved | OpStage::Credited | OpStage::Failed => return,
    };
    for s in StepId::ALL
        .into_iter()
        .skip(1)
        .take_while(|s| *s != reached)
    {
        board.done(s, "earlier run");
    }
}

/// Binds the transaction the user named with `--tx-hash`. It must be a
/// successful bridge deposit from the operation's sender, made from the
/// block the deposit was requested at, that no other operation has claimed
/// and that no other operation whose outcome is unknown could take by its
/// own rules; the nonce and uniqueness rules of the search are skipped. The
/// chain is read first; the claims are checked and the binding written
/// under the directory lock. A refusal leaves the record as it was, with
/// the exit of an outcome still unknown.
async fn bind_explicit(d: &Deps, store: &Store, rec: &mut OpRecord, h: B256) -> CliResult<()> {
    let op = rec.op_id.clone();
    let refused = |why: String| {
        err(
            ExitCode::DepositOutcomeUnknown,
            Stage::Deposit,
            &op,
            format!("--tx-hash {h:#x}: {why}. Operation {op} is left as it was"),
        )
    };
    if !matches!(
        rec.stage,
        OpStage::Requested | OpStage::Signed | OpStage::Abandoned
    ) {
        // Confirmed on chain already: only its own transaction is accepted.
        return match rec.tx {
            Some(t) if t.tx_hash == h => Ok(()),
            _ => Err(after_request(store, &op, CliError::Preflight {
                reason: format!(
                    "--tx-hash {h:#x}: operation {op} is past its confirmation on chain, bound to \
                     {}",
                    rec.tx
                        .map(|t| format!("{:#x}", t.tx_hash))
                        .unwrap_or_else(|| "its transaction".into())
                ),
                source: None,
            })),
        };
    }
    let ui = d.ui.as_ref();
    let head = transient(ui, "reading the chain head", || async {
        d.evm
            .header(BlockTag::Latest)
            .await?
            .map(|b| b.number)
            .ok_or_else(|| anyhow!("no latest block"))
    })
    .await;
    // A log whose transaction the node does not return right now leaves
    // the list incomplete: it is read again, never judged on.
    let view = rec.clone();
    let cands = transient(ui, "reading the deposits of the operation's sender", || {
        recovery::candidates(d.evm.as_ref(), &view, view.params.bridge, head)
    })
    .await;
    let Some(c) = cands.into_iter().find(|c| c.tx_hash == h) else {
        return Err(refused(format!(
            "it is no Deposit to bridge {} from {} made since block {}",
            rec.params.bridge,
            rec.from
                .map(|f| f.to_string())
                .unwrap_or_else(|| "the sender".into()),
            rec.request.as_ref().map(|q| q.from_block).unwrap_or(0)
        )));
    };
    // Claims are read and written under the directory lock.
    let _dir = DirLock::wait(store.dir())
        .await
        .map_err(|e| after_request(store, &op, e))?;
    let recs = store.list().map_err(|e| after_request(store, &op, e))?;
    let (chain_id, bridge) = (rec.params.chain_id, rec.params.bridge);
    let others = claims_of_others(&recs, &op, chain_id, bridge);
    let unresolved: Vec<&OpRecord> = recs
        .iter()
        .filter(|r| r.op_id != op && r.params.chain_id == chain_id && r.params.bridge == bridge)
        .filter(|r| r.is_unresolved())
        .collect();
    check_explicit(rec, &c, &others, &unresolved).map_err(refused)?;
    rec.stage = OpStage::Signed;
    rec.tx = Some(TxClaim {
        tx_hash: h,
        tx_nonce: c.nonce,
    });
    persist(store, rec)
}

/// `--resume`: continues the operation `target` names from its first
/// unfinished step, with the parameters from its record. A credited
/// operation is reported from its record, a failed one answers with its
/// recorded code and message; otherwise the resume checks only what the
/// recorded stage still needs ([`preflight::context_for_resume`]) and
/// drives the operation on. With `tx_hash` the named transaction is bound
/// first ([`bind_explicit`]); without it, an abandoned operation goes back
/// to the unknown outcome it was released in, and the record says so once
/// a transaction is bound.
pub async fn resume(
    p: &DepositParams,
    d: &Deps,
    target: &OpRef,
    tx_hash: Option<B256>,
) -> CliResult<DepositSuccess> {
    let ui = d.ui.as_ref();
    let Opened {
        store,
        op,
        lock,
        mut rec,
    } = match open_for_resume(p, target, ui, &d.current_op).await? {
        Resumable::Finished(s) => return Ok(*s),
        Resumable::Open(o) => *o,
    };
    let board = Board::new(ui);
    board.start(
        StepId::Preflight,
        &format!("resuming operation {op} at {:?}", rec.stage),
    );
    let cx = board.answer(preflight::context_for_resume(p, d, &rec).await)?;
    board.done(StepId::Preflight, "");
    done_earlier(&board, rec.stage);
    if let Some(h) = tx_hash {
        bind_explicit(d, &store, &mut rec, h).await?;
    } else if rec.stage == OpStage::Abandoned {
        rec.stage = if rec.tx.is_some() {
            OpStage::Signed
        } else {
            OpStage::Requested
        };
    }
    drive(p, d, &store, &mut rec, &cx, None, None, lock).await
}

/// A resume whose chains could not even be set up: `why` is the endpoint
/// its client rejected. What the record answers alone is still answered —
/// a credited operation's summary, a failed one's code. Any other operation
/// may be anywhere between the request and the credit, so `why` takes the
/// exit its stage on disk calls for, never exit 2.
pub async fn resume_without_chains(
    p: &DepositParams,
    target: &OpRef,
    ui: &dyn Ui,
    current_op: &Mutex<Option<String>>,
    why: CliError,
) -> CliResult<DepositSuccess> {
    match open_for_resume(p, target, ui, current_op).await? {
        Resumable::Finished(s) => Ok(*s),
        Resumable::Open(o) => Err(after_request(&o.store, &o.op, why)),
    }
}

/// `--abandon`: releases an operation whose EVM outcome is unknown, after
/// the user checked in the wallet that its transaction does not exist. The
/// operation stops blocking new deposits and becomes `Abandoned`; if its
/// transaction appears after all, `--resume` still picks it up, and it is
/// never again bound by the observed nonce alone. The command line is
/// checked against the record first, and the record changes only after
/// that check.
pub async fn abandon(p: &DepositParams, op: &str, ui: &dyn Ui) -> CliResult<DepositSuccess> {
    let store = Store::open(&p.state_dir)?;
    let op = find_op(p, &store, &OpRef::Op(op.to_string()))?;
    let Some(_lock) = OpLock::try_take(&p.state_dir, &op)? else {
        return Err(CliError::deposit(
            ExitCode::DuplicateRefused,
            Stage::Preflight,
            Some(&op),
            format!(
                "operation {op} is being run by another process; stop it first (nothing was \
                 changed)"
            ),
        ));
    };
    ui.op_id(&op);
    // Read and written under the directory lock, like the claims.
    let _dir = DirLock::wait(&p.state_dir).await?;
    let mut rec = store.load(&op).map_err(|e| unreadable(&op, e))?;
    // Under another profile an abandon would release an operation this
    // command line cannot even see.
    check_flags_against(p, &rec)?;
    match rec.stage {
        OpStage::Requested | OpStage::Signed => {
            rec.stage = OpStage::Abandoned;
            rec.abandoned_ever = true;
            persist(&store, &mut rec)?;
        },
        OpStage::Abandoned => {},
        other => {
            return Err(CliError::Preflight {
                reason: format!(
                    "operation {op} is in stage {other:?}; only an operation whose outcome on the \
                     EVM chain is unknown can be abandoned (nothing was changed)"
                ),
                source: None,
            })
        },
    }
    ui.warn(&format!(
        "operation {op} released. If its transaction appears after all, `ackinacki-bridge deposit \
         --resume {op}` still picks it up"
    ));
    Ok(DepositSuccess {
        op_id: Some(op.clone()),
        dry_run: false,
        network: Network::from_chain_id(rec.params.chain_id)
            .map(|n| n.name().to_string())
            .unwrap_or_default(),
        chain_id: rec.params.chain_id,
        amount: UsdcAmount(u128::from(rec.params.amount_units)).display(),
        to: rec.params.to.clone(),
        deposit: rec.tx.map(|t| {
            json!({
                "tx_hash": format!("{:#x}", t.tx_hash),
                "deposit_id": null,
            })
        }),
        anchor: None,
        tx: None,
        confirmation: None,
        balance: None,
        abandoned: true,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use alloy_primitives::{Address, Bytes, B256};
    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::{
        deposit::{
            args::{OpRef, RunMode},
            locks::DirLock,
            preflight::Polls,
            store::{FailReason, OpParams, OpRecord, OpStage, RequestInfo, TxClaim},
            testkit::*,
            ui::Ui,
            wallet::{TxRequest, Wallet, WalletError, WalletKind},
        },
        errors::ExitCode,
    };

    /// The world's command line and dependencies with polls short enough
    /// for the real clock. A test that reaches the proof runs on it: the
    /// fake prover is a real process, and on a paused clock the prover's
    /// deadline fires as soon as the runtime waits for that process.
    fn on_the_real_clock(w: &World) -> (DepositParams, Deps) {
        let mut p = w.params(RunMode::Fresh);
        p.relayer_grace = Duration::from_millis(10);
        let mut d = w.deps();
        let fast = Duration::from_millis(10);
        d.polls = Polls {
            evm: fast,
            anchor: fast,
            credit: fast,
            wallet: fast,
        };
        (p, d)
    }

    /// A wallet whose pairing never completes.
    struct NeverPairs;

    #[async_trait]
    impl Wallet for NeverPairs {
        fn kind(&self) -> WalletKind {
            WalletKind::WalletConnect
        }

        async fn connect(&mut self, _: &dyn Ui) -> Result<Address, WalletError> {
            Err(WalletError::Timeout)
        }

        async fn personal_sign(&mut self, _: Address, _: &str) -> Result<Bytes, WalletError> {
            Err(WalletError::Timeout)
        }

        async fn capabilities(&mut self, _: Address) -> Option<serde_json::Value> {
            None
        }

        async fn send_transaction(
            &mut self,
            _: &dyn Ui,
            _: &TxRequest,
        ) -> Result<B256, WalletError> {
            Err(WalletError::Timeout)
        }

        async fn close(&mut self) {}
    }

    /// [`FakeWallet`], counting how often its session is closed.
    struct Counted {
        inner: FakeWallet,
        closed: u32,
    }

    #[async_trait]
    impl Wallet for Counted {
        fn kind(&self) -> WalletKind {
            self.inner.kind()
        }

        async fn connect(&mut self, ui: &dyn Ui) -> Result<Address, WalletError> {
            self.inner.connect(ui).await
        }

        async fn personal_sign(&mut self, a: Address, m: &str) -> Result<Bytes, WalletError> {
            self.inner.personal_sign(a, m).await
        }

        async fn capabilities(&mut self, a: Address) -> Option<serde_json::Value> {
            self.inner.capabilities(a).await
        }

        async fn send_transaction(
            &mut self,
            ui: &dyn Ui,
            tx: &TxRequest,
        ) -> Result<B256, WalletError> {
            self.inner.send_transaction(ui, tx).await
        }

        async fn close(&mut self) {
            self.closed += 1;
        }
    }

    /// The world's wallet, taken out of it and counted; the world keeps a
    /// wallet for the same account.
    fn counted(w: &mut World) -> Counted {
        let account = w.wallet.account;
        let inner = std::mem::replace(&mut w.wallet, FakeWallet::eoa());
        w.wallet.account = account;
        Counted {
            inner,
            closed: 0,
        }
    }

    fn record(store: &Store, stage: OpStage) -> OpRecord {
        let mut rec = OpRecord::new(
            Store::new_op_id(),
            OpParams {
                chain_id: 11_155_111,
                bridge: W_BRIDGE,
                to: World::target().extended(),
                amount_units: W_AMOUNT,
                an_bridge: hex::encode(W_BRIDGE_ACC),
                an_network: "http://gql.invalid:80".into(),
            },
            hex::encode(crate::deposit::identity::EXPECTED_VOUCHER_CODE_HASH),
        );
        rec.from = Some(Address::repeat_byte(0xb5));
        if stage != OpStage::Reserved {
            rec.request = Some(RequestInfo {
                nonce_before: 7,
                from_block: 800,
                calldata: crate::deposit::evm::deposit_calldata(W_AMOUNT, B256::from(W_ACC)),
                wallet_hash: None,
            });
            rec.tx = Some(TxClaim {
                tx_hash: B256::repeat_byte(0x55),
                tx_nonce: 7,
            });
        }
        if matches!(
            stage,
            OpStage::Confirmed | OpStage::Anchored | OpStage::Proved
        ) {
            rec.deposit = Some(DepositInfo {
                deposit_id: alloy_primitives::U256::from(6),
                block_number: 900,
                block_hash: B256::repeat_byte(0x90),
                block_log_index: 41,
                receipt_log_index: 1,
                access_list_rlp_len: 1,
                voucher_account: "ab".repeat(32),
            });
        }
        rec.stage = stage;
        store.write(&mut rec).unwrap();
        rec
    }

    #[tokio::test]
    async fn the_happy_path_ends_credited_with_exit_0() {
        let mut w = World::healthy();
        let h = w.mined_deposit(7);
        let a = w.approve_hash();
        w.wallet.send_results.extend([Ok(a), Ok(h)]);
        w.anchor_after(2);
        w.credit_chain_for_mined_deposit();
        let (p, d) = on_the_real_clock(&w);
        let s = run_with(&p, &d, &mut w.wallet).await.unwrap();
        assert!(s.confirmation.is_some());
        let rec = Store::open(&p.state_dir)
            .unwrap()
            .load(s.op_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(rec.stage, OpStage::Credited);
        assert_eq!(w.prover_runs(), 1);
        // The world shows DepositFinalized from the start: only the count
        // tells that step 8 really sent.
        assert_eq!(*w.an.sent.lock().unwrap(), 1, "finalizeDeposit was sent");
        assert_eq!(*d.current_op.lock().unwrap(), s.op_id);
        let j = serde_json::to_value(&s).unwrap();
        assert_eq!(j["deposit"]["tx_hash"], format!("{h:#x}"));
        assert_eq!(j["anchor"]["writer"], "owner");
        assert_eq!(j["tx"]["type"], 2);
        assert_eq!(j["tx"]["calldata_len"], 100);
        assert_eq!(j["confirmation"]["confirm_tx"], "btx");
        assert_eq!(j["confirmation"]["delivery_tx"], "rtx");
        assert_eq!(j["confirmation"]["via_events"], false);
        assert!(DirLock::try_take(&p.state_dir).unwrap().is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn a_second_run_while_the_first_holds_the_directory_is_exit_3() {
        let w = World::healthy();
        let p = w.params(RunMode::Fresh);
        let _first = crate::deposit::locks::DirLock::try_take(&p.state_dir)
            .unwrap()
            .unwrap();
        let mut wallet = FakeWallet::eoa();
        let d = w.deps();
        let e = run_with(&p, &d, &mut wallet).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::DuplicateRefused);
        assert!(
            wallet.sent.is_empty(),
            "the second run never reached the wallet"
        );
        assert_eq!(
            *d.current_op.lock().unwrap(),
            None,
            "no operation was created"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_killed_after_requested_blocks_the_same_sender() {
        let mut w = World::healthy();
        let from = w.wallet.account;
        w.left_requested_operation_from(from);
        let p = w.params_with_other_amount(RunMode::Fresh);
        let d = w.deps();
        let e = run_with(&p, &d, &mut w.wallet).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::DuplicateRefused);
        assert!(e.to_string().contains("--resume"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_reverted_deposit_is_exit_22_and_frees_the_directory() {
        let mut w = World::healthy();
        let h = w.reverted_deposit_finalized(7);
        let a = w.approve_hash();
        w.wallet.send_results.extend([Ok(a), Ok(h)]);
        let p = w.params(RunMode::Fresh);
        let d = w.deps();
        let e = run_with(&p, &d, &mut w.wallet).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::DepositReverted);
        assert!(
            e.to_string().contains("InvalidAmount"),
            "names the likely causes: {e}"
        );
        assert!(
            e.to_string().contains("BridgePaused"),
            "a pause after the last check is one of them: {e}"
        );
        assert!(crate::deposit::locks::DirLock::try_take(&p.state_dir)
            .unwrap()
            .is_some());
        let rec = Store::open(&p.state_dir)
            .unwrap()
            .load(e.op_id().unwrap())
            .unwrap();
        assert_eq!(rec.failure.unwrap().reason, FailReason::Reverted);
        assert_eq!(
            *d.current_op.lock().unwrap(),
            None,
            "the operation is closed: an interrupt now must not offer --resume"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_legacy_transaction_is_exit_35_with_what_to_give_the_operator() {
        let mut w = World::healthy();
        let h = w.mined_deposit_of_type(7, 0);
        let a = w.approve_hash();
        w.wallet.send_results.extend([Ok(a), Ok(h)]);
        let p = w.params(RunMode::Fresh);
        let d = w.deps();
        let e = run_with(&p, &d, &mut w.wallet).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::DepositUnprovable);
        let msg = e.to_string();
        assert!(
            msg.contains(&format!("{h:#x}"))
                && msg.contains("depositId")
                && msg.contains("operator"),
            "{msg}"
        );
    }

    #[tokio::test]
    async fn a_reorg_during_the_anchor_wait_updates_the_deposit_and_goes_on() {
        let mut w = World::healthy();
        // The directory lock is let go at the confirmation and taken again
        // after the reorg.
        let _relocking = crate::test_forks::relocking();
        let h = w.mined_deposit(7);
        let a = w.approve_hash();
        w.wallet.send_results.extend([Ok(a), Ok(h)]);
        w.reorg_after_confirmation(h, 2); // new block, depositId 8; only the new block gets anchored
        w.credit_chain_for_mined_deposit();
        let (p, d) = on_the_real_clock(&w);
        let s = run_with(&p, &d, &mut w.wallet).await.unwrap();
        assert_eq!(s.deposit.unwrap()["deposit_id"], "8");
    }

    #[tokio::test(start_paused = true)]
    async fn a_bound_search_does_not_outlive_the_recovery_window() {
        // The search finds the transaction, then the node stops showing it.
        // The claim is written from what the search read; nothing waits for
        // the node again past the window.
        let mut w = World::healthy();
        let op = w.left_requested_operation_from(w.wallet.account);
        w.mined_deposit(7);
        *w.evm.vanish_tx_after.lock().unwrap() = Some(1); // the search's one read, then gone
        let p = w.params(RunMode::Fresh);
        let store = Store::open(&p.state_dir).unwrap();
        let mut rec = store.load(&op).unwrap();
        let t0 = tokio::time::Instant::now();
        let claim = tokio::time::timeout(
            Duration::from_secs(3600),
            bind_by_search(
                &w.deps(),
                &store,
                &mut rec,
                Some(Duration::from_secs(1)),
                false,
            ),
        )
        .await
        .expect("the window must end the search")
        .unwrap();
        assert_eq!(claim.tx_nonce, 7);
        assert!(t0.elapsed() <= Duration::from_secs(1), "{:?}", t0.elapsed());
        assert_eq!(store.load(&op).unwrap().stage, OpStage::Signed);
    }

    #[test]
    fn the_temporary_entry_point_is_gone_and_nothing_here_aborts() {
        let m = include_str!("mod.rs");
        assert!(
            !m.contains("the pipeline is not assembled"),
            "Task A2's stub must be replaced"
        );
        assert!(
            !m.contains("not available in this build yet"),
            "--resume and --abandon reach the driver"
        );
        let run = crate::source_guard::production_source("run.rs", include_str!("run.rs"));
        for bad in ["unimplemented!", "todo!", "panic!(", ".unwrap()"] {
            assert!(
                !run.contains(bad),
                "{bad} in the deposit driver: refuse with an exit code instead"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_wallet_that_never_pairs_is_exit_20_and_a_failed_pairing() {
        let w = World::healthy();
        let p = w.params(RunMode::Fresh);
        let d = w.deps();
        let f = run_fresh(&p, &d, &mut NeverPairs).await.unwrap_err();
        assert_eq!(f.error.exit_code(), ExitCode::WalletFailed);
        assert!(f.pairing_failed, "--qr-mode both may offer EIP-681 now");
        let op = f.error.op_id().unwrap().to_string();
        let rec = Store::open(&p.state_dir).unwrap().load(&op).unwrap();
        assert_eq!(rec.stage, OpStage::Failed);
        assert_eq!(rec.failure.unwrap().reason, FailReason::Refused);
        assert_eq!(
            *d.current_op.lock().unwrap(),
            None,
            "nothing was requested, so an interrupt now is exit 2"
        );
        assert!(DirLock::try_take(&p.state_dir).unwrap().is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn a_deposit_rejected_in_the_wallet_is_not_a_failed_pairing() {
        let mut w = World::healthy();
        w.mined_deposit(7); // the sender's nonce as the node shows it
        let a = w.approve_hash();
        w.wallet
            .send_results
            .extend([Ok(a), Err(WalletError::Rejected)]);
        let p = w.params(RunMode::Fresh);
        let d = w.deps();
        let f = run_fresh(&p, &d, &mut w.wallet).await.unwrap_err();
        assert_eq!(f.error.exit_code(), ExitCode::WalletFailed);
        assert!(
            !f.pairing_failed,
            "the wallet paired and said no: EIP-681 must not ask again"
        );
        let rec = Store::open(&p.state_dir)
            .unwrap()
            .load(f.error.op_id().unwrap())
            .unwrap();
        assert_eq!(rec.failure.unwrap().reason, FailReason::Rejected);
        assert_eq!(
            *d.current_op.lock().unwrap(),
            None,
            "closed as rejected before the wallet session is closed"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_wallet_session_is_closed_after_the_confirmation_and_on_every_way_out() {
        // Closed when the deposit leaves step 5, here with a verdict.
        let mut w = World::healthy();
        let h = w.reverted_deposit_finalized(7);
        let a = w.approve_hash();
        w.wallet.send_results.extend([Ok(a), Ok(h)]);
        let mut wallet = counted(&mut w);
        let p = w.params(RunMode::Fresh);
        run_with(&p, &w.deps(), &mut wallet).await.unwrap_err();
        assert_eq!(wallet.closed, 1);

        // Closed on a refusal after the pairing.
        let mut w = World::healthy();
        w.left_requested_operation_from(w.wallet.account);
        let mut wallet = counted(&mut w);
        let p = w.params_with_other_amount(RunMode::Fresh);
        let e = run_with(&p, &w.deps(), &mut wallet).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::DuplicateRefused);
        assert_eq!(wallet.closed, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_bridge_paused_after_the_grace_period_is_waited_for_not_proven() {
        let mut w = World::healthy();
        let h = w.mined_deposit(7);
        let a = w.approve_hash();
        w.wallet.send_results.extend([Ok(a), Ok(h)]);
        w.anchor_after(0);
        // Preflight, then the anchor wait: not paused. The check before the
        // proof: paused, and it stays so.
        w.an.getter(W_BRIDGE_ACC, "isPaused", vec![
            json!({ "value0": false }),
            json!({ "value0": false }),
            json!({ "value0": true }),
        ]);
        let mut p = w.params(RunMode::Fresh);
        p.anchor_timeout = Some(Duration::from_secs(600));
        let d = w.deps();
        let e = run_with(&p, &d, &mut w.wallet).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::AnWaitTimeout, "{e}");
        assert_eq!(
            w.prover_runs(),
            0,
            "nothing is proven while the bridge is paused"
        );
        let rec = Store::open(&p.state_dir)
            .unwrap()
            .load(e.op_id().unwrap())
            .unwrap();
        assert_eq!(rec.stage, OpStage::Confirmed);
    }

    #[tokio::test]
    async fn a_proof_that_fails_leaves_the_operation_anchored_for_resume() {
        let mut w = World::healthy();
        // As when Ctrl-C reaches the prover too: it dies, the run does not
        // close the operation.
        w.prover = fake_prover_dir("exit 3");
        let h = w.mined_deposit(7);
        let a = w.approve_hash();
        w.wallet.send_results.extend([Ok(a), Ok(h)]);
        w.anchor_after(0);
        let (p, d) = on_the_real_clock(&w);
        let e = run_with(&p, &d, &mut w.wallet).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::DepositProofFailed, "{e}");
        let rec = Store::open(&p.state_dir)
            .unwrap()
            .load(e.op_id().unwrap())
            .unwrap();
        assert_eq!(rec.stage, OpStage::Anchored);
    }

    #[tokio::test]
    async fn an_anchor_lost_after_a_send_that_does_not_come_back_is_exit_34() {
        let mut w = World::healthy();
        let h = w.mined_deposit(7);
        let a = w.approve_hash();
        w.wallet.send_results.extend([Ok(a), Ok(h)]);
        let block = w.mined.as_ref().unwrap().block.hash;
        // Accepted for the wait, gone for good when the send hits 224.
        w.an.accepted_seq
            .lock()
            .unwrap()
            .insert(block, Script::new([true, false]));
        w.first_finalize_hits_224();
        let (mut p, d) = on_the_real_clock(&w);
        p.anchor_timeout = Some(Duration::from_millis(500));
        let e = run_with(&p, &d, &mut w.wallet).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::CreditUnconfirmed, "{e}");
        assert_eq!(*w.an.sent.lock().unwrap(), 1);
        let rec = Store::open(&p.state_dir)
            .unwrap()
            .load(e.op_id().unwrap())
            .unwrap();
        assert_eq!(
            rec.stage,
            OpStage::Finalizing,
            "a resume must know a send may have executed"
        );
    }

    #[test]
    fn a_state_write_that_fails_exits_by_the_stage_still_on_disk() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return; // root writes into a read-only directory anyway
        }
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let read_only = |yes: bool| {
            let mode = if yes { 0o500 } else { 0o700 };
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).unwrap();
        };

        let mut signed = record(&store, OpStage::Signed);
        signed.fail(
            OpStage::Signed,
            FailReason::NonceConsumed,
            ExitCode::WalletFailed,
            "replaced",
        );
        read_only(true);
        let e = persist(&store, &mut signed).unwrap_err();
        read_only(false);
        assert_eq!(e.exit_code(), ExitCode::DepositOutcomeUnknown, "{e}");
        assert!(e.to_string().contains("--resume"), "{e}");

        let mut anchored = record(&store, OpStage::Anchored);
        anchored.stage = OpStage::Proved;
        read_only(true);
        let e = persist(&store, &mut anchored).unwrap_err();
        read_only(false);
        assert_eq!(e.exit_code(), ExitCode::DepositProofFailed, "{e}");

        let mut reserved = record(&store, OpStage::Reserved);
        reserved.stage = OpStage::Requested;
        read_only(true);
        let e = persist(&store, &mut reserved).unwrap_err();
        read_only(false);
        assert_eq!(e.exit_code(), ExitCode::PreflightRefused, "{e}");
        assert!(e.to_string().contains("nothing was sent"), "{e}");
    }

    // ---- --resume, --tx-hash, --abandon ----

    /// [`on_the_real_clock`] for a resume of `target`.
    fn resuming_on_the_real_clock(w: &World, target: &OpRef) -> (DepositParams, Deps) {
        let (mut p, d) = on_the_real_clock(w);
        p.mode = RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        };
        (p, d)
    }

    #[tokio::test]
    async fn resume_finds_a_transaction_mined_after_the_window() {
        let mut w = World::healthy();
        let from = w.wallet.account;
        let op = w.left_requested_operation_from(from);
        w.mined_deposit_for_operation(&op, 7);
        w.anchor_after(1);
        w.credit_chain_for_mined_deposit();
        let target = OpRef::Op(op);
        let (p, d) = resuming_on_the_real_clock(&w, &target);
        let s = resume(&p, &d, &target, None).await.unwrap();
        assert!(s.confirmation.is_some());
    }

    #[tokio::test]
    async fn abandon_then_resume_still_works_when_the_transaction_appears() {
        let mut w = World::healthy();
        let from = w.wallet.account;
        let op = w.left_signed_operation_from(from, 7);
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let p = w.params(RunMode::Abandon(op.clone()));
        assert!(abandon(&p, &op, &ui).await.unwrap().abandoned);
        assert!(
            ui.warnings().iter().any(|w| w.contains("--resume")),
            "{:?}",
            ui.warnings()
        );
        let rec = Store::open(&p.state_dir).unwrap().load(&op).unwrap();
        assert_eq!(rec.stage, OpStage::Abandoned);
        assert!(!rec.is_unresolved(), "a new deposit is no longer blocked");
        w.mined_deposit_for_operation(&op, 7);
        w.anchor_after(1);
        w.credit_chain_for_mined_deposit();
        let target = OpRef::Op(op);
        let (p, d) = resuming_on_the_real_clock(&w, &target);
        assert!(
            resume(&p, &d, &target, None).await.is_ok(),
            "tx_nonce was known before the abandon"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn resume_refuses_flags_that_contradict_the_record() {
        let w = World::healthy();
        let op = w.left_signed_operation_from(w.wallet.account, 7);
        let target = OpRef::Op(op);
        let mut p = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        p.bridge = Some(alloy_primitives::address!(
            "8545129b215b248944a3ae40f711f34cab458644"
        ));
        let e = resume(&p, &w.deps(), &target, None).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::PreflightRefused);
        assert!(e.to_string().contains("bridge"), "{e}");
    }

    #[tokio::test(start_paused = true)]
    async fn resume_refuses_another_acki_nacki_bridge_or_network() {
        let w = World::healthy();
        let op = w.left_signed_operation_from(w.wallet.account, 7);
        let target = OpRef::Op(op);
        let mut other_bridge = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        other_bridge.usdc_bridge_account = Some([0x20; 32]);
        let mut other_network = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        other_network.gql_endpoint = Some("https://mainnet.ackinacki.org/graphql".into());
        // The same host on another port is another network.
        let mut other_port = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        other_port.gql_endpoint = Some("http://gql.invalid:8700/graphql".into());
        for (p, needle) in [
            (other_bridge, "Acki Nacki bridge"),
            (other_network, "Acki Nacki network"),
            (other_port, "Acki Nacki network"),
        ] {
            let e = resume(&p, &w.deps(), &target, None).await.unwrap_err();
            assert_eq!(e.exit_code(), ExitCode::PreflightRefused);
            assert!(e.to_string().contains(needle), "{e}");
        }
        assert_eq!(*w.an.sent.lock().unwrap(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn resume_of_a_credited_operation_reads_only_its_record() {
        let mut w = World::healthy();
        let op = w.credited_operation();
        let target = OpRef::Op(op);
        let p = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        let mut d = w.deps();
        // Neither chain answers anything.
        d.evm = std::sync::Arc::new(FakeEvm::default());
        d.an = std::sync::Arc::new(FakeAn::default());
        let s = resume(&p, &d, &target, None).await.unwrap();
        assert_eq!(s.confirmation.unwrap()["confirm_tx"], "btx");
    }

    #[tokio::test(start_paused = true)]
    async fn resume_of_finalizing_on_a_paused_bridge_still_reads_the_credit() {
        let mut w = World::healthy();
        let op = w.finalizing_operation();
        w.paused(true);
        w.voucher_deployed();
        w.credit_chain_for_mined_deposit();
        let target = OpRef::Op(op);
        let mut p = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        p.prover_dir = Some("/nonexistent/prover".into()); // not needed: the voucher is there
        let s = resume(&p, &w.deps(), &target, None).await.unwrap();
        assert!(s.confirmation.is_some());
        assert_eq!(w.prover_runs(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_second_resume_of_one_operation_is_exit_3() {
        let w = World::healthy();
        let op = w.left_signed_operation_from(w.wallet.account, 7);
        let target = OpRef::Op(op.clone());
        let p = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        let _first = crate::deposit::locks::OpLock::try_take(&p.state_dir, &op)
            .unwrap()
            .unwrap();
        let e = resume(&p, &w.deps(), &target, None).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::DuplicateRefused);
        // Nor can the operation be released while another process runs it.
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let e = abandon(&p, &op, &ui).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::DuplicateRefused);
        let rec = Store::open(&p.state_dir).unwrap().load(&op).unwrap();
        assert_eq!(rec.stage, OpStage::Signed);
    }

    #[tokio::test(start_paused = true)]
    async fn an_abandoned_operation_without_identity_needs_tx_hash_and_refuses_anothers() {
        let mut w = World::healthy();
        // A: Requested at nonce n, abandoned. B: Requested at nonce n, died
        // before its hash. T: B's transaction.
        let (a, b, t) = w.two_operations_one_slot(7);
        let target = OpRef::Op(a.clone());
        let p = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        let e = resume(&p, &w.deps(), &target, None).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::DepositOutcomeUnknown);
        assert!(e.to_string().contains("--tx-hash"));
        let e = resume(&p, &w.deps(), &target, Some(t)).await.unwrap_err();
        assert!(e.to_string().contains(&b), "must point at B: {e}");
        let rec = Store::open(&p.state_dir).unwrap().load(&a).unwrap();
        assert_eq!(rec.stage, OpStage::Abandoned, "A is left as it was");
        assert!(rec.tx.is_none());
        // B binds it by its own nonce_before. Nothing anchors the block, so
        // the run stops in the anchor wait with the deposit confirmed.
        let target = OpRef::Op(b.clone());
        let mut p = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        p.anchor_timeout = Some(Duration::from_secs(1));
        let e = resume(&p, &w.deps(), &target, None).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::AnWaitTimeout, "{e}");
        let rec = Store::open(&p.state_dir).unwrap().load(&b).unwrap();
        assert_eq!(rec.stage, OpStage::Confirmed);
        assert_eq!(rec.tx.map(|c| c.tx_hash), Some(t));
    }

    #[tokio::test(start_paused = true)]
    async fn an_explicit_hash_binds_an_abandoned_operation_without_identity() {
        let mut w = World::healthy();
        let from = w.wallet.account;
        let op = w.left_requested_operation_from(from);
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let p = w.params(RunMode::Abandon(op.clone()));
        abandon(&p, &op, &ui).await.unwrap();
        let t = w.mined_deposit(7);
        let target = OpRef::Op(op.clone());
        let mut p = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: Some(t),
        });
        p.anchor_timeout = Some(Duration::from_secs(1));
        let e = resume(&p, &w.deps(), &target, Some(t)).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::AnWaitTimeout, "{e}");
        let rec = Store::open(&p.state_dir).unwrap().load(&op).unwrap();
        assert_eq!(rec.stage, OpStage::Confirmed);
        assert_eq!(
            rec.tx,
            Some(TxClaim {
                tx_hash: t,
                tx_nonce: 7
            })
        );
        assert!(rec.abandoned_ever);
    }

    #[tokio::test(start_paused = true)]
    async fn a_tx_hash_that_is_no_deposit_of_the_operation_changes_nothing() {
        let w = World::healthy();
        let op = w.left_requested_operation_from(w.wallet.account);
        let a = w.approve_hash(); // mined, but no deposit
        let target = OpRef::Op(op.clone());
        let p = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: Some(a),
        });
        let e = resume(&p, &w.deps(), &target, Some(a)).await.unwrap_err();
        assert_eq!(
            e.exit_code(),
            ExitCode::DepositOutcomeUnknown,
            "the deposit may be in flight: never exit 2: {e}"
        );
        assert!(e.to_string().contains(&format!("{a:#x}")), "{e}");
        let rec = Store::open(&p.state_dir).unwrap().load(&op).unwrap();
        assert_eq!(rec.stage, OpStage::Requested);
        assert!(rec.tx.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn a_crash_between_the_wallet_hash_and_its_nonce_resumes_to_exit_22() {
        let mut w = World::healthy();
        let h = w.reverted_deposit_finalized(7);
        let from = w.wallet.account;
        let op = w.left_requested_operation_with_wallet_hash(from, h);
        let target = OpRef::Op(op);
        let p = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        let e = resume(&p, &w.deps(), &target, None).await.unwrap_err();
        assert_eq!(
            e.exit_code(),
            ExitCode::DepositReverted,
            "not an endless search for a Deposit log"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_replacement_that_deposited_another_amount_is_exit_35_not_nothing_deposited() {
        let mut w = World::healthy();
        let from = w.wallet.account;
        let op = w.left_signed_operation_from(from, 7);
        w.mined_deposit_of_amount(7, W_AMOUNT / 2);
        let target = OpRef::Op(op);
        let p = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        let d = w.deps();
        let e = resume(&p, &d, &target, None).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::DepositUnprovable);
        assert!(e.to_string().contains("differs from the request"), "{e}");
        assert_eq!(
            *d.current_op.lock().unwrap(),
            None,
            "closed for the operator: an interrupt now must not offer --resume"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn abandon_refuses_a_profile_the_operation_was_not_made_with() {
        let w = World::healthy();
        let op = w.left_signed_operation_from(w.wallet.account, 7);
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let mut p = w.params(RunMode::Abandon(op.clone()));
        p.gql_endpoint = Some("https://mainnet.ackinacki.org/graphql".into());
        let e = abandon(&p, &op, &ui).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::PreflightRefused);
        assert!(e.to_string().contains("Acki Nacki network"), "{e}");
        let rec = Store::open(&p.state_dir).unwrap().load(&op).unwrap();
        assert_eq!(rec.stage, OpStage::Signed, "the record is untouched");
        assert!(!rec.abandoned_ever);
    }

    #[tokio::test(start_paused = true)]
    async fn a_resume_of_a_failed_operation_repeats_its_exit_code_and_message() {
        let w = World::healthy();
        for (reason, code) in [
            (FailReason::Rejected, ExitCode::WalletFailed),
            (FailReason::NonceConsumed, ExitCode::WalletFailed),
            (FailReason::ApproveFailed, ExitCode::ApproveFailed),
            (FailReason::Refused, ExitCode::WalletFailed),
            (FailReason::Refused, ExitCode::PreflightRefused),
            (FailReason::Refused, ExitCode::DuplicateRefused),
            (FailReason::Interrupted, ExitCode::PreflightRefused),
            (FailReason::Reverted, ExitCode::DepositReverted),
            (FailReason::Mismatch, ExitCode::DepositUnprovable),
            (FailReason::Unprovable, ExitCode::DepositUnprovable),
            (FailReason::CreditAborted, ExitCode::CreditAborted),
        ] {
            let op = w.left_signed_operation_from(w.wallet.account, 7);
            let target = OpRef::Op(op.clone());
            let p = w.params(RunMode::Resume {
                target: target.clone(),
                tx_hash: None,
            });
            let store = Store::open(&p.state_dir).unwrap();
            let mut rec = store.load(&op).unwrap();
            rec.fail(
                OpStage::Signed,
                reason,
                code,
                format!("recorded for {reason:?}"),
            );
            store.write(&mut rec).unwrap();
            let e = resume(&p, &w.deps(), &target, None).await.unwrap_err();
            assert_eq!(e.exit_code(), code, "{reason:?}");
            assert!(
                e.to_string().contains(&format!("recorded for {reason:?}")),
                "{e}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_record_given_to_the_driver_answers_with_its_own_code() {
        let w = World::healthy();
        let store = Store::open(w.state.path()).unwrap();
        let mut rec = record(&store, OpStage::Signed);
        rec.fail(
            OpStage::Signed,
            FailReason::Mismatch,
            ExitCode::DepositUnprovable,
            "another amount",
        );
        store.write(&mut rec).unwrap();
        let op_lock = OpLock::try_take(store.dir(), &rec.op_id).unwrap().unwrap();
        let cx = RunCx {
            plan: AnchorPlan::Owner {
                lc_ready: false,
            },
            bridge_acc: W_BRIDGE_ACC,
            bridge_dapp: [0; 32],
            light_client: None,
            prover: w.prover.1.clone(),
            rpc_url: "http://rpc.invalid".into(),
            usdc: W_USDC,
        };
        let p = w.params(RunMode::Fresh);
        let e = drive(&p, &w.deps(), &store, &mut rec, &cx, None, None, op_lock)
            .await
            .unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::DepositUnprovable, "{e}");
        assert!(e.to_string().contains("another amount"), "{e}");
    }

    #[tokio::test(start_paused = true)]
    async fn only_an_unknown_outcome_can_be_abandoned() {
        let mut w = World::healthy();
        let op = w.credited_operation();
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let p = w.params(RunMode::Abandon(op.clone()));
        assert_eq!(
            abandon(&p, &op, &ui).await.unwrap_err().exit_code(),
            ExitCode::PreflightRefused
        );
        let rec = Store::open(&p.state_dir).unwrap().load(&op).unwrap();
        assert_eq!(rec.stage, OpStage::Credited);
    }

    #[tokio::test(start_paused = true)]
    async fn an_operation_that_is_not_there_is_refused_without_a_trace() {
        let w = World::healthy();
        let op = "01J0000000000000000000000A".to_string();
        let target = OpRef::Op(op.clone());
        let p = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        let e = resume(&p, &w.deps(), &target, None).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::PreflightRefused, "{e}");
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let e = abandon(&p, &op, &ui).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::PreflightRefused, "{e}");
        assert!(
            !p.state_dir.join(format!("{op}.lock")).exists(),
            "no lock file for an operation that does not exist"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn resume_by_deposit_id_finds_the_operation_that_made_it() {
        let mut w = World::healthy();
        let op = w.credited_operation();
        let id = Store::open(w.state.path())
            .unwrap()
            .load(&op)
            .unwrap()
            .deposit
            .unwrap()
            .deposit_id;
        let target = OpRef::DepositId(id);
        let mut p = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        p.network = None; // optional for a lookup by deposit id
        let s = resume(&p, &w.deps(), &target, None).await.unwrap();
        assert_eq!(s.op_id.as_deref(), Some(op.as_str()));
        let unknown = OpRef::DepositId(id + alloy_primitives::U256::from(1));
        let e = resume(&p, &w.deps(), &unknown, None).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::PreflightRefused, "{e}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_reservation_whose_run_died_is_closed_as_interrupted() {
        let w = World::healthy();
        let store = Store::open(w.state.path()).unwrap();
        let rec = record(&store, OpStage::Reserved);
        let target = OpRef::Op(rec.op_id.clone());
        let p = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        let e = resume(&p, &w.deps(), &target, None).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::PreflightRefused, "{e}");
        assert!(e.to_string().contains("nothing was sent"), "{e}");
        let back = store.load(&rec.op_id).unwrap();
        assert_eq!(back.failure.unwrap().reason, FailReason::Interrupted);
    }

    #[tokio::test(start_paused = true)]
    async fn a_resume_refuses_a_bridge_below_the_minimum_version_by_the_stage() {
        let w = World::healthy();
        let op = w.left_signed_operation_from(w.wallet.account, 7);
        let target = OpRef::Op(op.clone());
        let p = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        let mut d = w.deps();
        d.min_bridge = Some(crate::deposit::an_preflight::BridgeVersion(1, 7, 0));
        let e = resume(&p, &d, &target, None).await.unwrap_err();
        assert!(e.to_string().contains("1.7.0"), "{e}");
        assert_eq!(
            e.exit_code(),
            ExitCode::DepositOutcomeUnknown,
            "the deposit may be in flight: never exit 2: {e}"
        );
        let rec = Store::open(&p.state_dir).unwrap().load(&op).unwrap();
        assert_eq!(rec.stage, OpStage::Signed);
    }

    #[tokio::test(start_paused = true)]
    async fn a_resume_whose_chains_cannot_be_set_up_answers_from_the_record() {
        let mut w = World::healthy();
        let signed = w.left_signed_operation_from(w.wallet.account, 7);
        let credited = w.credited_operation();
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let current = std::sync::Mutex::new(None);
        let why = || CliError::ArgInvalid {
            flag: "rpc-url",
            expected: "an http(s) URL".into(),
            got: crate::args::redact("not a url"),
        };
        let t = OpRef::Op(signed);
        let p = w.params(RunMode::Resume {
            target: t.clone(),
            tx_hash: None,
        });
        let e = resume_without_chains(&p, &t, &ui, &current, why())
            .await
            .unwrap_err();
        assert_eq!(
            e.exit_code(),
            ExitCode::DepositOutcomeUnknown,
            "the wallet was asked: never exit 2: {e}"
        );
        assert!(e.to_string().contains("--rpc-url"), "{e}");
        let t = OpRef::Op(credited);
        let s = resume_without_chains(&p, &t, &ui, &current, why())
            .await
            .unwrap();
        assert!(
            s.confirmation.is_some(),
            "a credited operation needs no chain"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_resume_after_a_voucher_code_change_confirms_by_events() {
        let mut w = World::healthy();
        let op = w.proved_operation();
        w.voucher_code_moved();
        w.credit_chain_for_mined_deposit();
        let target = OpRef::Op(op);
        let p = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        let s = resume(&p, &w.deps(), &target, None).await.unwrap();
        assert_eq!(s.confirmation.unwrap()["via_events"], true);
    }

    #[tokio::test(start_paused = true)]
    async fn a_resume_without_the_proof_after_a_voucher_code_change_needs_no_prover() {
        // Finalized through the new voucher code: the stored voucher address
        // stays empty, and only the bridge's events show the deposit is done.
        let mut w = World::healthy();
        let op = w.finalizing_operation();
        w.voucher_code_moved();
        w.credit_chain_for_mined_deposit();
        let target = OpRef::Op(op);
        let mut p = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        p.prover_dir = Some("/nonexistent/prover".into());
        let s = resume(&p, &w.deps(), &target, None).await.unwrap();
        assert_eq!(s.confirmation.unwrap()["via_events"], true);
        assert_eq!(w.prover_runs(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn the_reads_before_the_send_count_against_the_anchor_timeout() {
        // The block time for the event search never comes; with a 1 s step 8
        // the run must not wait a whole read cap on it first.
        let mut w = World::healthy();
        let op = w.proved_operation();
        w.paused(true);
        w.evm
            .hang_headers_by_hash
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let target = OpRef::Op(op);
        let mut p = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        p.anchor_timeout = Some(Duration::from_secs(1));
        let t0 = tokio::time::Instant::now();
        let e = resume(&p, &w.deps(), &target, None).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::AnWaitTimeout);
        assert!(t0.elapsed() <= Duration::from_secs(1), "{:?}", t0.elapsed());
    }

    #[tokio::test(start_paused = true)]
    async fn a_proof_on_disk_does_not_hide_a_credit_through_the_new_voucher_code() {
        // Credited through the new voucher code while the bridge is paused:
        // with the proof on disk, step 8 would wait for the pause to lift
        // before anything looked at the events.
        let mut w = World::healthy();
        let op = w.proved_operation();
        w.voucher_code_moved();
        w.paused(true);
        w.credit_chain_for_mined_deposit();
        let target = OpRef::Op(op);
        let p = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        let s = resume(&p, &w.deps(), &target, None).await.unwrap();
        assert_eq!(s.confirmation.unwrap()["via_events"], true);
        assert_eq!(
            *w.an.sent.lock().unwrap(),
            0,
            "nothing to send: the deposit is credited"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_recipient_that_spent_the_funds_before_the_check_is_still_exit_0() {
        // The credit is the delivered transfer, not the balance: the
        // recipient may have spent it all before step 9 looked.
        let mut w = World::healthy();
        let op = w.proved_operation();
        w.credit_chain_for_mined_deposit();
        w.recipient_spends_everything_after_the_send(7_000_000);
        let target = OpRef::Op(op);
        let p = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        let s = resume(&p, &w.deps(), &target, None).await.unwrap();
        assert_eq!(*w.an.sent.lock().unwrap(), 1, "finalizeDeposit was sent");
        assert!(s.confirmation.is_some());
        let b = s.balance.unwrap();
        assert_eq!(b["before"], "7000000");
        assert_eq!(b["after"], "0", "less than before, and still exit 0");
    }
}
