//! Ethereum Frontend for the Bridge
//!
//! This crate implements the Ethereum side of the bridge, handling:
//! - Deposit flow: Generate secrets, create commitments, submit to Ethereum
//!   contract
//! - Withdrawal flow: Generate proofs, verify burns, submit to Ethereum
//!   contract
//! - Async transaction management with retry logic
//! - Status tracking for cross-chain operations

pub mod contract;
pub mod deposit;
pub mod error;
pub mod status;

pub use contract::EthereumContract;
pub use deposit::DepositManager;
pub use error::{BridgeError, Result};
pub use status::{BridgeStatus, TransactionTracker};

#[cfg(test)]
mod tests;
