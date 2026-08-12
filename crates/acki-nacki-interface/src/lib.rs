//! Acki Nacki blockchain interface
//!
//! This crate provides an abstract interface for interacting with the Acki
//! Nacki blockchain. The actual implementation will be provided by the Acki
//! Nacki team.

pub mod address;
pub mod error;
pub mod mock;
pub mod traits;
#[cfg(feature = "tvm-sdk")]
pub mod tvm_client;
pub mod types;

pub use address::ExtendedAddress;
pub use error::{AckiNackiError, Result};
pub use mock::{MockAckiNacki, MockTransactionSender};
pub use traits::{IAckiNacki, TransactionSender};
#[cfg(feature = "tvm-sdk")]
pub use tvm_client::{
    parse_exit_code_from_message, TvmAckiNacki, TvmClientConfig, EXIT_CONSTRUCTOR_ALREADY_CALLED,
    EXIT_INVALID_ZKPROOF, HD_PATH_V3,
};
pub use types::{AckiNackiTransaction, ContractCallRequest, TransactionReceipt, TransactionStatus};

#[cfg(test)]
mod tests;
