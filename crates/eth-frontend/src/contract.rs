//! Ethereum contract interface

use crate::error::Result;

/// Ethereum contract interface
pub struct EthereumContract {}

impl EthereumContract {
    /// Create a new contract interface
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for EthereumContract {
    fn default() -> Self {
        Self::new()
    }
}

