//! Error taxonomy for the deposit relayer.
//!
//! As in the AN→ETH relayer, recoverable conditions (a deposit isn't
//! confirmed deeply enough yet, the AN side rejected a submission so a
//! re-prove may help) are distinguished from terminal conditions (a source
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

    /// The proof-generation stage failed (witness fetch, circuit error,
    /// malformed operand). The caller decides whether to retry (transient
    /// RPC blip) or treat as terminal (witness mismatch).
    #[error("proof generation failed: {0}")]
    ProofGeneration(String),

    /// The AN side rejected the finalize submission (proof verification
    /// failed, or the `depositId` nullifier was already consumed). The
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
