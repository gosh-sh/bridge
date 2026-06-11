//! Acki Nacki blockchain interface
//!
//! This crate provides an abstract interface for interacting with the Acki
//! Nacki blockchain. The actual implementation will be provided by the Acki
//! Nacki team.

pub mod address;
pub mod bk_set_client;
pub mod bk_set_tracker;
pub mod error;
pub mod mock;
pub mod traits;
#[cfg(feature = "tvm-sdk")]
pub mod tvm_client;
pub mod types;

pub use address::ExtendedAddress;
pub use bk_set_client::{
    BkEntry, BkSetClient, BkSetResponse, BkSetUpdateResponse, BkUpdateEntry, BLS_PUBKEY_LEN,
    ID32_LEN,
};
pub use bk_set_tracker::{BkSetChange, BkSetSnapshot, BkSetTracker, MembershipDelta};
pub use error::{AckiNackiError, Result};
pub use mock::{MockAckiNacki, MockTransactionSender};
pub use traits::{IAckiNacki, TransactionSender};
#[cfg(feature = "tvm-sdk")]
pub use tvm_client::{TvmAckiNacki, TvmClientConfig, HD_PATH_V3};
pub use types::{AckiNackiTransaction, ContractCallRequest, TransactionReceipt, TransactionStatus};

#[cfg(test)]
mod tests;
