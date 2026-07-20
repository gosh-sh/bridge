//! Types for Acki Nacki transactions and data structures

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::address::ExtendedAddress;

/// Transaction hash type (32 bytes)
pub type TxHash = [u8; 32];

/// ABI-encoded contract call routed through tvm_client 3.0 (`dapp_id` on the
/// wire).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ContractCallRequest {
    /// Sender in extended `dapp_id::account_id` form.
    pub from: ExtendedAddress,
    /// Destination contract in extended `dapp_id::account_id` form.
    pub to: ExtendedAddress,
    /// Contract function name (e.g. `finalizeDeposit`).
    pub function: String,
    /// JSON function arguments matching the contract ABI.
    pub params: Value,
}

/// Transaction on Acki Nacki blockchain
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AckiNackiTransaction {
    /// Transaction hash
    pub tx_hash: TxHash,
    /// Sender address
    pub from: String,
    /// Recipient address (contract)
    pub to: String,
    /// Transaction data (encoded function call)
    pub data: Vec<u8>,
    /// Gas limit
    pub gas_limit: u64,
    /// Nonce
    pub nonce: u64,
}

impl AckiNackiTransaction {
    /// Create a new transaction
    pub fn new(
        tx_hash: TxHash,
        from: String,
        to: String,
        data: Vec<u8>,
        gas_limit: u64,
        nonce: u64,
    ) -> Self {
        Self {
            tx_hash,
            from,
            to,
            data,
            gas_limit,
            nonce,
        }
    }

    /// Get transaction hash
    pub fn hash(&self) -> &TxHash {
        &self.tx_hash
    }
}

/// Status of a transaction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransactionStatus {
    /// Transaction is pending
    Pending,
    /// Transaction is included in a block
    Included,
    /// Transaction is confirmed
    Confirmed,
    /// Transaction failed
    Failed,
    /// Transaction was reverted
    Reverted,
}

impl TransactionStatus {
    /// Check if transaction is finalized (confirmed or failed)
    pub fn is_finalized(&self) -> bool {
        matches!(
            self,
            TransactionStatus::Confirmed | TransactionStatus::Failed | TransactionStatus::Reverted
        )
    }

    /// Check if transaction succeeded
    pub fn is_success(&self) -> bool {
        matches!(self, TransactionStatus::Confirmed)
    }
}

/// Receipt for a transaction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionReceipt {
    /// Transaction hash
    pub tx_hash: TxHash,
    /// Status
    pub status: TransactionStatus,
    /// Block number
    pub block_number: Option<u64>,
    /// Gas used
    pub gas_used: u64,
    /// Logs/events
    pub logs: Vec<Log>,
    /// TVM compute-phase exit code (`compute.exit_code`), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Whether the transaction was aborted.
    #[serde(default)]
    pub aborted: bool,
}

impl TransactionReceipt {
    /// Create a new receipt (successful / unknown exit code).
    pub fn new(
        tx_hash: TxHash,
        status: TransactionStatus,
        block_number: Option<u64>,
        gas_used: u64,
        logs: Vec<Log>,
    ) -> Self {
        Self {
            tx_hash,
            status,
            block_number,
            gas_used,
            logs,
            exit_code: None,
            aborted: false,
        }
    }

    /// Create a receipt with compute-phase details.
    pub fn with_compute(
        tx_hash: TxHash,
        status: TransactionStatus,
        block_number: Option<u64>,
        gas_used: u64,
        exit_code: Option<i32>,
        aborted: bool,
    ) -> Self {
        Self {
            tx_hash,
            status,
            block_number,
            gas_used,
            logs: vec![],
            exit_code,
            aborted,
        }
    }

    /// Check if transaction succeeded
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }
}

/// Event log from a transaction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Log {
    /// Contract address that emitted the log
    pub address: String,
    /// Topics (indexed parameters)
    pub topics: Vec<TxHash>,
    /// Data (non-indexed parameters)
    pub data: Vec<u8>,
}

impl Log {
    /// Create a new log
    pub fn new(address: String, topics: Vec<TxHash>, data: Vec<u8>) -> Self {
        Self {
            address,
            topics,
            data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transaction_creation() {
        let tx = AckiNackiTransaction::new(
            [1u8; 32],
            "sender".to_string(),
            "recipient".to_string(),
            vec![1, 2, 3],
            100000,
            1,
        );

        assert_eq!(tx.from, "sender");
        assert_eq!(tx.to, "recipient");
        assert_eq!(tx.gas_limit, 100000);
    }

    #[test]
    fn test_transaction_status() {
        assert!(TransactionStatus::Confirmed.is_finalized());
        assert!(TransactionStatus::Failed.is_finalized());
        assert!(!TransactionStatus::Pending.is_finalized());

        assert!(TransactionStatus::Confirmed.is_success());
        assert!(!TransactionStatus::Failed.is_success());
    }

    #[test]
    fn test_receipt_creation() {
        let receipt = TransactionReceipt::new(
            [1u8; 32],
            TransactionStatus::Confirmed,
            Some(12345),
            50000,
            vec![],
        );

        assert!(receipt.is_success());
        assert_eq!(receipt.block_number, Some(12345));
    }
}
