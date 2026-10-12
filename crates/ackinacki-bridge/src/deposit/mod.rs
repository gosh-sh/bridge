//! `ackinacki-bridge deposit`: EVM → Acki Nacki.
//!
//! The wallet signs `approve` and `deposit`; everything after the
//! broadcast — the anchor wait, the proof, `finalizeDeposit` and the
//! credit check — runs here, keyed by an operation id that survives a
//! crash.

pub mod an;
pub mod an_preflight;
pub mod anchor_wait;
pub mod args;
pub mod binding;
pub mod credit;
#[cfg(all(test, feature = "e2e"))]
mod e2e;
pub mod evm;
pub mod evm_confirm;
pub mod evm_preflight;
pub mod evm_steps;
pub mod finalize;
pub mod identity;
#[cfg(test)]
mod it_anvil;
pub mod lc_readiness;
pub mod limits;
pub mod locks;
pub mod log_index;
pub mod pi;
pub mod preflight;
pub mod prover;
pub mod prover_files;
pub mod qr;
pub mod qr_display;
pub mod recovery;
pub mod refusals;
pub mod retry;
pub mod run;
pub mod signals;
pub mod store;
#[cfg(test)]
pub mod testkit;
pub mod ui;
pub mod wallet;
pub mod wc;

use std::sync::{Arc, Mutex, PoisonError};

use crate::errors::{CliError, CliResult};

/// Runs `ackinacki-bridge deposit` against the live chains: a new deposit,
/// a dry run, `--resume` or `--abandon`. SIGINT, SIGTERM and SIGHUP end the
/// run cleanly: the prover is killed, the locks are released, and the exit
/// code follows the operation's recorded stage. The progress display is
/// gone when this returns, so the caller prints the summary or the error on
/// a quiet terminal.
pub async fn run(p: args::DepositParams) -> CliResult<DepositSuccess> {
    let current_op: Arc<Mutex<Option<String>>> = Arc::default();
    // The step board moves into `dispatch` and goes with it, before the
    // summary or the error is printed.
    let ui = ui::pick(&p.globals, p.uri_only, p.qr_invert, p.qr_display);
    match signals::until_signal(dispatch(&p, ui, current_op.clone())).await {
        Ok(r) => r,
        Err(sig) => {
            let op = current_op
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            signals::interrupted(sig, &p.state_dir, op)
        },
    }
}

/// The run `p.mode` asks for, printing its progress to `ui`.
async fn dispatch(
    p: &args::DepositParams,
    ui: Arc<dyn ui::Ui>,
    current_op: Arc<Mutex<Option<String>>>,
) -> CliResult<DepositSuccess> {
    match &p.mode {
        // No chain is asked: the record is all there is to change.
        args::RunMode::Abandon(op) => {
            // An interrupt reports on the operation as its record stands.
            *current_op.lock().unwrap_or_else(PoisonError::into_inner) = Some(op.clone());
            run::abandon(p, op, ui.as_ref()).await
        },
        args::RunMode::Resume {
            target,
            tx_hash,
        } => match live_deps(p, ui.clone(), current_op.clone()) {
            Ok(d) => run::resume(p, &d, target, *tx_hash).await,
            // The wallet may have been asked already: the refusal takes the
            // exit of the operation's stage, and a finished operation is
            // still answered from its record.
            Err(e) => run::resume_without_chains(p, target, ui.as_ref(), &current_op, e).await,
        },
        args::RunMode::Fresh | args::RunMode::DryRun => {
            let d = live_deps(p, ui, current_op)?;
            start(p, &d).await
        },
    }
}

/// The live chains behind `--rpc-url` and `--gql-endpoint`, the progress
/// display, and the slot the signal handler reads the operation from.
/// Nothing is sent or read yet. A resume may come without either endpoint;
/// the refusal names each one missing.
fn live_deps(
    p: &args::DepositParams,
    ui: Arc<dyn ui::Ui>,
    current_op: Arc<Mutex<Option<String>>>,
) -> CliResult<preflight::Deps> {
    let (Some(rpc), Some(gql)) = (&p.rpc_url, &p.gql_endpoint) else {
        let missing: Vec<&str> = [
            (p.rpc_url.is_none(), "--rpc-url (RPC_URL)"),
            (
                p.gql_endpoint.is_none(),
                "--gql-endpoint (BRIDGE_GQL_ENDPOINT)",
            ),
        ]
        .into_iter()
        .filter_map(|(absent, flag)| absent.then_some(flag))
        .collect();
        return Err(CliError::Usage {
            reason: format!("deposit: missing {}", missing.join(", ")),
        });
    };
    Ok(preflight::Deps {
        evm: Arc::new(evm::AlloyEvm::connect(rpc)?),
        an: Arc::new(an::LiveAn::connect(gql)?),
        ui,
        polls: preflight::Polls::live(),
        min_bridge: an_preflight::MIN_BRIDGE_VERSION,
        current_op,
    })
}

/// A new deposit, or a dry run, through the wallet `--qr-mode` picks. With
/// `both`, a WalletConnect pairing that fails falls back to the EIP-681
/// codes; a failure after the pairing is the run's answer.
async fn start(p: &args::DepositParams, d: &preflight::Deps) -> CliResult<DepositSuccess> {
    let usage = |what: &str| CliError::Usage {
        reason: format!("deposit: {what} missing"),
    };
    let net = p.network.ok_or_else(|| usage("--network"))?;
    let eip681 = |from| wallet::eip681::Eip681Wallet::new(from, net.chain_id());
    match p.qr_mode {
        args::QrMode::Eip681 => {
            let from = p.from_address.ok_or_else(|| usage("--from-address"))?;
            run::run_with(p, d, &mut eip681(from)).await
        },
        args::QrMode::Walletconnect | args::QrMode::Both => {
            let mut w = wc::WalletConnectWallet::new(wc::WcConfig {
                relay_url: p.wc_relay_url.clone(),
                project_id: p.wc_project_id.clone().unwrap_or_default(),
                network: net,
                expect_from: p.from_address,
                pair_timeout: p.pair_timeout,
                request_timeout: std::time::Duration::from_secs(300),
                qr_out: p.qr_out.clone(),
            });
            match run::run_fresh(p, d, &mut w).await {
                Ok(s) => Ok(s),
                Err(f) if p.qr_mode == args::QrMode::Both && f.pairing_failed => {
                    let Some(from) = p.from_address else {
                        return Err(*f.error);
                    };
                    // That operation ended before the wallet was asked for
                    // anything; an interrupt from here on is about the next.
                    *d.current_op.lock().unwrap_or_else(PoisonError::into_inner) = None;
                    d.ui.warn(&format!("{}; showing the EIP-681 codes instead", f.error));
                    run::run_with(p, d, &mut eip681(from)).await
                },
                Err(f) => Err(*f.error),
            }
        },
    }
}

/// The final summary of a deposit run, printed by `output`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DepositSuccess {
    /// The operation id; absent for a dry run that never reserved one.
    pub op_id: Option<String>,
    /// Nothing was sent.
    pub dry_run: bool,
    /// The network as `--network` takes it, e.g. `sepolia`.
    pub network: String,
    /// EVM chain id of `network`.
    pub chain_id: u64,
    /// The deposit amount in USDC, as typed.
    pub amount: String,
    /// The Acki Nacki recipient.
    pub to: String,
    /// The deposit transaction: `tx_hash`, `deposit_id`, ...
    pub deposit: Option<serde_json::Value>,
    /// The anchor that covered the deposit block: `writer`, ...
    pub anchor: Option<serde_json::Value>,
    /// The `finalizeDeposit` message.
    pub tx: Option<serde_json::Value>,
    /// The credit confirmation: `confirm_tx`, `delivery_tx`, ...
    pub confirmation: Option<serde_json::Value>,
    /// Balance before and after, diagnostic only.
    pub balance: Option<serde_json::Value>,
    /// The operation was released instead of completed.
    pub abandoned: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        deposit::{
            args::{DepositParams, GlobalFlags, OpRef, RunMode},
            store::{FailReason, OpStage, Store},
            testkit::{deposit_args, World},
            ui::RecordingUi,
        },
        errors::ExitCode,
    };

    /// The world's EVM node, as `--rpc-url` names it.
    const RPC: &str = "http://rpc.invalid/key";
    /// The world's Acki Nacki node, as `--gql-endpoint` names it.
    const GQL: &str = "http://gql.invalid/graphql";

    /// `deposit --resume <op> <argv>` with the world's state and work
    /// directories, validated as `main` validates it. Nothing else is set.
    fn resume_line(w: &World, op: &str, argv: &[&str]) -> DepositParams {
        let state = w.state.path().to_str().unwrap();
        let work = w.work.path().to_str().unwrap();
        let mut line = vec!["--resume", op, "--state-dir", state, "--work-dir", work];
        line.extend_from_slice(argv);
        deposit_args(&line)
            .validate(&GlobalFlags::default())
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// `p` run as `main` runs it, without the signal handler.
    async fn dispatched(p: &DepositParams) -> CliResult<DepositSuccess> {
        dispatch(p, Arc::new(RecordingUi::new(true)), Arc::default()).await
    }

    /// Closes `op` as failed with `exit` and `detail`.
    fn close(w: &World, op: &str, reason: FailReason, exit: ExitCode, detail: &str) {
        let store = Store::open(w.state.path()).unwrap();
        let mut rec = store.load(op).unwrap();
        rec.fail(rec.stage, reason, exit, detail);
        store.write(&mut rec).unwrap();
    }

    /// `op`'s stage on disk.
    fn stage(w: &World, op: &str) -> OpStage {
        Store::open(w.state.path()).unwrap().load(op).unwrap().stage
    }

    /// Moves `op`'s record to stage `at`.
    fn restage(w: &World, op: &str, at: OpStage) {
        let store = Store::open(w.state.path()).unwrap();
        let mut rec = store.load(op).unwrap();
        rec.stage = at;
        store.write(&mut rec).unwrap();
    }

    /// `deposit <argv>` validated with `HOME` unset, as under systemd or
    /// cron. Nothing else is set.
    fn without_home(argv: &[&str]) -> DepositParams {
        deposit_args(argv)
            .validate_with_home(&GlobalFlags::default(), None)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    #[tokio::test(start_paused = true)]
    async fn without_home_a_resume_and_an_abandon_need_only_the_state_directory() {
        let w = World::healthy();
        let state = w.state.path().to_str().unwrap();
        let failed = w.left_signed_operation_from(w.wallet.account, 7);
        close(
            &w,
            &failed,
            FailReason::Reverted,
            ExitCode::DepositReverted,
            "the deposit reverted in block 900",
        );
        let e = dispatched(&without_home(&["--resume", &failed, "--state-dir", state]))
            .await
            .unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::DepositReverted, "{e}");
        assert!(
            e.to_string().contains("the deposit reverted in block 900"),
            "{e}"
        );
        let open = w.left_signed_operation_from(w.wallet.account, 8);
        let e = dispatched(&without_home(&["--resume", &open, "--state-dir", state]))
            .await
            .unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::DepositOutcomeUnknown, "{e}");
        for flag in [
            "--rpc-url (RPC_URL)",
            "--gql-endpoint (BRIDGE_GQL_ENDPOINT)",
        ] {
            assert!(e.to_string().contains(flag), "{flag}: {e}");
        }
        let s = dispatched(&without_home(&["--abandon", &open, "--state-dir", state]))
            .await
            .unwrap();
        assert!(s.abandoned);
        assert_eq!(stage(&w, &open), OpStage::Abandoned);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_operation_repeats_its_exit_given_only_the_state_directory() {
        let w = World::healthy();
        let op = w.left_signed_operation_from(w.wallet.account, 7);
        close(
            &w,
            &op,
            FailReason::Reverted,
            ExitCode::DepositReverted,
            "the deposit reverted in block 900",
        );
        let e = dispatched(&resume_line(&w, &op, &[])).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::DepositReverted, "{e}");
        assert!(
            e.to_string().contains("the deposit reverted in block 900"),
            "{e}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_credited_operation_answers_its_summary_given_only_the_state_directory() {
        let mut w = World::healthy();
        let op = w.credited_operation();
        let s = dispatched(&resume_line(&w, &op, &[])).await.unwrap();
        assert_eq!(s.op_id.as_deref(), Some(op.as_str()));
        assert_eq!(s.confirmation.unwrap()["confirm_tx"], "btx");
    }

    #[tokio::test(start_paused = true)]
    async fn an_open_operation_without_the_chains_takes_its_stage_exit_naming_them() {
        let w = World::healthy();
        let op = w.left_signed_operation_from(w.wallet.account, 7);
        let e = dispatched(&resume_line(&w, &op, &[])).await.unwrap_err();
        assert_eq!(
            e.exit_code(),
            ExitCode::DepositOutcomeUnknown,
            "the wallet was asked: never exit 2: {e}"
        );
        for flag in [
            "--rpc-url (RPC_URL)",
            "--gql-endpoint (BRIDGE_GQL_ENDPOINT)",
        ] {
            assert!(e.to_string().contains(flag), "{flag}: {e}");
        }
        assert!(e.to_string().contains(&format!("--resume {op}")), "{e}");
        assert_eq!(stage(&w, &op), OpStage::Signed);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stage_before_the_proof_without_the_prover_takes_its_exit_naming_it() {
        for (at, exit) in [
            (OpStage::Confirmed, ExitCode::AnWaitTimeout),
            (OpStage::Anchored, ExitCode::DepositProofFailed),
        ] {
            let mut w = World::healthy();
            let op = w.proved_operation();
            restage(&w, &op, at);
            let p = resume_line(&w, &op, &["--rpc-url", RPC, "--gql-endpoint", GQL]);
            assert!(p.prover_dir.is_none());
            let e = run::resume(&p, &w.deps(), &OpRef::Op(op.clone()), None)
                .await
                .unwrap_err();
            assert_eq!(e.exit_code(), exit, "{at:?}: {e}");
            assert!(
                e.to_string()
                    .contains("--deposit-prover-dir (BRIDGE_DEPOSIT_PROVER_DIR)"),
                "{at:?}: {e}"
            );
            assert_eq!(stage(&w, &op), at, "the record is unchanged");
            assert_eq!(*w.an.sent.lock().unwrap(), 0);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_missing_prover_is_named_before_a_chain_is_read() {
        // A flag and a local directory: nothing waits for an RPC that is
        // down before saying the prover is missing.
        let mut w = World::healthy();
        let op = w.proved_operation();
        restage(&w, &op, OpStage::Confirmed);
        w.evm
            .fail_chain_id
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let p = resume_line(&w, &op, &["--rpc-url", RPC, "--gql-endpoint", GQL]);
        let e = tokio::time::timeout(
            std::time::Duration::from_secs(600),
            run::resume(&p, &w.deps(), &OpRef::Op(op.clone()), None),
        )
        .await
        .expect("the missing prover is named without reading the EVM chain")
        .unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::AnWaitTimeout, "{e}");
        assert!(
            e.to_string()
                .contains("--deposit-prover-dir (BRIDGE_DEPOSIT_PROVER_DIR)"),
            "{e}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_proved_operation_goes_on_without_the_prover() {
        let mut w = World::healthy();
        let op = w.proved_operation();
        w.credit_chain_for_mined_deposit();
        let p = resume_line(&w, &op, &["--rpc-url", RPC, "--gql-endpoint", GQL]);
        assert!(p.prover_dir.is_none());
        let s = run::resume(&p, &w.deps(), &OpRef::Op(op), None)
            .await
            .unwrap();
        assert!(s.confirmation.is_some());
        assert_eq!(*w.an.sent.lock().unwrap(), 1, "finalizeDeposit was sent");
        assert_eq!(w.prover_runs(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_proved_operation_without_its_proof_or_the_prover_is_exit_32_naming_it() {
        // The proof is gone and has to be built again: that step fails, and
        // nothing was sent yet.
        let mut w = World::healthy();
        let op = w.proved_operation();
        let work = w.work.path().join(&op);
        std::fs::remove_file(work.join("proof.bin")).unwrap();
        std::fs::remove_file(work.join("public_inputs.bin")).unwrap();
        let p = resume_line(&w, &op, &["--rpc-url", RPC, "--gql-endpoint", GQL]);
        let e = run::resume(&p, &w.deps(), &OpRef::Op(op.clone()), None)
            .await
            .unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::DepositProofFailed, "{e}");
        assert!(
            e.to_string()
                .contains("--deposit-prover-dir (BRIDGE_DEPOSIT_PROVER_DIR)"),
            "{e}"
        );
        assert_eq!(stage(&w, &op), OpStage::Proved);
        assert_eq!(*w.an.sent.lock().unwrap(), 0);
        assert_eq!(w.prover_runs(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_proof_to_build_again_without_a_work_directory_names_it() {
        // The record names no work directory, and none is given: the proof
        // cannot be written anywhere.
        let mut w = World::healthy();
        let op = w.finalizing_operation();
        let target = OpRef::Op(op.clone());
        let mut p = w.params(RunMode::Resume {
            target: target.clone(),
            tx_hash: None,
        });
        p.work_dir = None;
        let e = run::resume(&p, &w.deps(), &target, None).await.unwrap_err();
        assert_eq!(
            e.exit_code(),
            ExitCode::CreditUnconfirmed,
            "a send may have executed: {e}"
        );
        assert!(
            e.to_string().contains("--work-dir (BRIDGE_WORK_DIR)"),
            "{e}"
        );
        assert_eq!(stage(&w, &op), OpStage::Finalizing);
        assert_eq!(w.prover_runs(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_proof_to_build_again_without_the_prover_names_it() {
        // Sent before, the proof gone, nothing finalized: the proof is
        // built again, and that needs the prover.
        let mut w = World::healthy();
        let op = w.finalizing_operation();
        let p = resume_line(&w, &op, &["--rpc-url", RPC, "--gql-endpoint", GQL]);
        let e = run::resume(&p, &w.deps(), &OpRef::Op(op.clone()), None)
            .await
            .unwrap_err();
        assert_eq!(
            e.exit_code(),
            ExitCode::CreditUnconfirmed,
            "a send may have executed: {e}"
        );
        assert!(
            e.to_string()
                .contains("--deposit-prover-dir (BRIDGE_DEPOSIT_PROVER_DIR)"),
            "{e}"
        );
        assert_eq!(stage(&w, &op), OpStage::Finalizing);
        assert_eq!(w.prover_runs(), 0);
    }
}
