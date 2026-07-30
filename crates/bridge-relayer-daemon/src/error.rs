//! Error taxonomy for the relayer.
//!
//! The relayer distinguishes recoverable conditions (a block isn't ready
//! yet, the bridge rejected the proof so the next attempt will refetch a
//! fresher one) from terminal conditions (calldata that violates the
//! contract's structural invariants — a logic bug — and unexpected
//! Ethereum RPC failures).

use thiserror::Error;

use crate::types::ShapeError;

#[derive(Debug, Error)]
pub enum RelayerError {
    /// The configured BlockSource doesn't have data for this seqno yet.
    /// Recoverable: the relayer waits and retries.
    #[error("block source has no data yet for seqNo={0}")]
    BlockNotYetAvailable(u64),

    /// BlockSource returned a payload whose seqNo doesn't match the
    /// target. Treated as a terminal source-side bug — never retried by
    /// the relayer.
    #[error("block source returned wrong seqNo: requested {requested}, got {got}")]
    SeqNoMismatch { requested: u64, got: u64 },

    /// The fetched block payload violates the contract's shape rules.
    /// Terminal: the source is buggy.
    #[error("block source returned invalid shape: {0}")]
    InvalidShape(#[from] ShapeError),

    /// The bridge reverted while verifying our proofs. Recoverable in
    /// the sense that the relayer logs and moves on (a re-prove may
    /// succeed once the off-chain data is corrected); we never silently
    /// double-submit.
    #[error("bridge rejected verifyBlock: {0}")]
    BridgeRejected(String),

    /// Any I/O error while reading or writing `state.json`. Terminal —
    /// without persistence we can't safely resume after a crash.
    #[error("relayer state I/O error: {0}")]
    StateIo(#[from] std::io::Error),

    /// Failure to (de)serialise `state.json`.
    #[error("relayer state serde error: {0}")]
    StateSerde(#[from] serde_json::Error),

    /// A failure surfacing from `acki-nacki-interface` (address parse,
    /// contract-call encoding, tvm_client submission). Wrapped by string
    /// so the dependency type doesn't leak into `RelayerError`'s public API.
    #[error("acki-nacki interface error: {0}")]
    AckiNacki(String),

    /// An unexpected condition we don't have a finer-grained variant
    /// for. Use sparingly.
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
}
