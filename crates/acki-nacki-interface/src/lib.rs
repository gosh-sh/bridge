//! Acki Nacki blockchain interface
//!
//! This crate provides an abstract interface for interacting with the Acki Nacki blockchain.
//! The actual implementation will be provided by the Acki Nacki team.

pub mod types;
pub mod traits;
pub mod mock;
pub mod error;

pub use types::{AckiNackiTransaction, TransactionStatus, TransactionReceipt};
pub use traits::{IAckiNacki, TransactionSender};
pub use mock::{MockAckiNacki, MockTransactionSender};
pub use error::{AckiNackiError, Result};

#[cfg(test)]
mod tests;

