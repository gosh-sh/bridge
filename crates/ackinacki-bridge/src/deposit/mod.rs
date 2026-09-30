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
    // The step board lives inside `dispatch` and goes with it, before the
    // summary or the error is printed.
    match signals::until_signal(dispatch(&p, current_op.clone())).await {
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

/// The run `p.mode` asks for, with the progress display it prints to.
async fn dispatch(
    p: &args::DepositParams,
    current_op: Arc<Mutex<Option<String>>>,
) -> CliResult<DepositSuccess> {
    let ui = ui::pick(&p.globals, p.uri_only, p.qr_invert);
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
/// Nothing is sent or read yet.
fn live_deps(
    p: &args::DepositParams,
    ui: Arc<dyn ui::Ui>,
    current_op: Arc<Mutex<Option<String>>>,
) -> CliResult<preflight::Deps> {
    let usage = |what: &str| CliError::Usage {
        reason: format!("deposit: {what} missing"),
    };
    let rpc = p.rpc_url.clone().ok_or_else(|| usage("--rpc-url"))?;
    let gql = p
        .gql_endpoint
        .clone()
        .ok_or_else(|| usage("--gql-endpoint"))?;
    Ok(preflight::Deps {
        evm: Arc::new(evm::AlloyEvm::connect(&rpc)?),
        an: Arc::new(an::LiveAn::connect(&gql)?),
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
    /// Network name, e.g. `sepolia`.
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
