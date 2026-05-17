//! Acki Nacki blockchain interface
//!
//! This crate provides an abstract interface for interacting with the Acki
//! Nacki blockchain. The actual implementation will be provided by the Acki
//! Nacki team.

pub mod bk_set_client;
pub mod bk_set_tracker;
pub mod error;
pub mod mock;
pub mod traits;
pub mod types;

pub use bk_set_client::{
    BkEntry, BkSetClient, BkSetResponse, BkSetUpdateResponse, BkUpdateEntry, BLS_PUBKEY_LEN,
    ID32_LEN,
};
pub use bk_set_tracker::{BkSetChange, BkSetSnapshot, BkSetTracker, MembershipDelta};
pub use error::{AckiNackiError, Result};
pub use mock::{MockAckiNacki, MockTransactionSender};
pub use traits::{IAckiNacki, TransactionSender};
pub use types::{AckiNackiTransaction, TransactionReceipt, TransactionStatus};

#[cfg(test)]
mod tests;
