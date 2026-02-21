//! Acki Nacki blockchain interface
//!
//! This crate provides an abstract interface for interacting with the Acki
//! Nacki blockchain. The actual implementation will be provided by the Acki
//! Nacki team.

pub mod error;
pub mod mock;
pub mod traits;
pub mod types;

pub use error::{AckiNackiError, Result};
pub use mock::{MockAckiNacki, MockTransactionSender};
pub use traits::{IAckiNacki, TransactionSender};
pub use types::{AckiNackiTransaction, TransactionReceipt, TransactionStatus};

#[cfg(test)]
mod tests;
