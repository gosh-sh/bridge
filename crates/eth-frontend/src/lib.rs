//! Ethereum Frontend for the Bridge
//!
//! This crate implements the Ethereum side of the bridge, handling:
//! - Deposit flow: Generate secrets, create commitments, submit to Ethereum contract
//! - Withdrawal flow: Generate proofs, verify burns, submit to Ethereum contract
//! - Async transaction management with retry logic
//! - Status tracking for cross-chain operations

pub mod deposit;
pub mod withdrawal;
pub mod contract;
pub mod status;
pub mod error;

pub use deposit::DepositManager;
pub use withdrawal::WithdrawalManager;
pub use contract::EthereumContract;
pub use status::{BridgeStatus, TransactionTracker};
pub use error::{BridgeError, Result};

#[cfg(test)]
mod tests;

