//! `ackinacki-bridge deposit`: EVM → Acki Nacki.
//!
//! The wallet signs `approve` and `deposit`; everything after the
//! broadcast — the anchor wait, the proof, `finalizeDeposit` and the
//! credit check — runs here, keyed by an operation id that survives a
//! crash.

pub mod args;
pub mod binding;
pub mod evm;
pub mod identity;
pub mod limits;
pub mod locks;
pub mod log_index;
pub mod pi;
pub mod prover_files;
pub mod refusals;
pub mod retry;
pub mod store;
#[cfg(test)]
pub mod testkit;
pub mod ui;

/// Replaced by the driver in `run.rs`.
pub async fn run(_params: args::DepositParams) -> crate::errors::CliResult<DepositSuccess> {
    Err(crate::errors::CliError::Preflight {
        reason: "deposit: the pipeline is not assembled in this build".into(),
        source: None,
    })
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
