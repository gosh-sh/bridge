//! Error taxonomy for the deposit relayer.
//!
//! As in the AN→ETH relayer, recoverable conditions (a deposit isn't
//! confirmed deeply enough yet, the AN side rejected a submission so a
//! retry may help) are distinguished from terminal conditions (a source
//! returned the wrong deposit id — a logic bug — and unexpected I/O).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RelayerError {
    /// No `Deposit` event for this `deposit_id` is visible on Ethereum yet
    /// (not emitted, or not buried under enough confirmations). Recoverable:
    /// the relayer waits and retries.
    #[error("no confirmed deposit yet for depositId={0}")]
    DepositNotYetAvailable(u64),

    /// The source returned a deposit whose id doesn't match the target.
    /// Terminal — a source-side bug, never retried.
    #[error("deposit source returned wrong id: requested {requested}, got {got}")]
    DepositIdMismatch { requested: u64, got: u64 },

    /// Building the `finalizeDeposit` arguments failed: an RPC error, a
    /// receipt or block the node does not have, receipts that do not rebuild
    /// the header's `receiptsRoot`. Retried with backoff.
    #[error("building the receipt proof failed: {0}")]
    ProofGeneration(String),

    /// The deposit cannot be finalized from its receipt: its transaction
    /// failed, the log is not the bridge's `Deposit` in the one shape the AN
    /// bridge reads, the transaction or log index does not fit a `uint16`, or
    /// no message can carry its proof (over one transaction's gas, or over
    /// one message). Final for that deposit: the relayer records it in
    /// `unprovable_deposit_ids` at once.
    #[error("the deposit cannot be finalized from its receipt: {0}")]
    Unprovable(String),

    /// The bridge checks the deposit's receipt proof with more gas than an
    /// external message may spend before it is accepted. The relayer sends
    /// only external messages; the deposit's user sends this one from their
    /// own Acki Nacki wallet, which attaches the gas. The relayer records it
    /// in `wallet_deposit_ids` at once.
    #[error(
        "the deposit's receipt proof needs more gas than an external message may spend ({0}); its \
         user finalizes it from their Acki Nacki wallet: ackinacki-bridge deposit --resume \
         <op-id> --an-wallet <dapp_id::account_id> --an-wallet-keys <keys.json>"
    )]
    NeedsWallet(String),

    /// The AN side rejected the finalize submission (the receipt proof was
    /// refused, or the deposit's voucher already exists). The
    /// relayer logs and moves on without double-submitting.
    #[error("acki nacki rejected finalizeDeposit: {0}")]
    AnRejected(String),

    /// Any I/O error while reading or writing `state.json`. Terminal —
    /// without persistence we can't safely resume after a crash.
    #[error("relayer state I/O error: {0}")]
    StateIo(#[from] std::io::Error),

    /// Failure to (de)serialise `state.json`.
    #[error("relayer state serde error: {0}")]
    StateSerde(#[from] serde_json::Error),

    /// An error surfacing from `acki-nacki-interface` (the AN client /
    /// transaction sender). Recoverable — the caller decides whether to
    /// retry.
    #[error("acki-nacki interface error: {0}")]
    AckiNacki(String),

    /// An Ethereum RPC / log-decode failure. Recoverable.
    #[error("ethereum error: {0}")]
    Eth(String),

    /// An unexpected condition without a finer-grained variant. Use
    /// sparingly.
    #[error("relayer error: {0}")]
    Other(String),
}

impl From<acki_nacki_interface::AckiNackiError> for RelayerError {
    fn from(e: acki_nacki_interface::AckiNackiError) -> Self {
        Self::AckiNacki(e.to_string())
    }
}

impl RelayerError {
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }

    pub fn eth(msg: impl Into<String>) -> Self {
        Self::Eth(msg.into())
    }
}
