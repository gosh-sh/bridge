//! Error taxonomy for the Ethereum beacon light-client relayer.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RelayerError {
    #[error("beacon source error: {0}")]
    Beacon(String),

    #[error("proof generation failed: {0}")]
    ProofGeneration(String),

    #[error("acki nacki rejected light-client submit: {0}")]
    AnRejected(String),

    #[error("relayer state I/O error: {0}")]
    StateIo(#[from] std::io::Error),

    #[error("relayer state serde error: {0}")]
    StateSerde(#[from] serde_json::Error),

    #[error("acki-nacki interface error: {0}")]
    AckiNacki(String),

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

    pub fn beacon(msg: impl Into<String>) -> Self {
        Self::Beacon(msg.into())
    }
}
