//! Type definitions for deposit event proofs

use serde::{Deserialize, Serialize};

/// Deposit event data from Ethereum
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepositEventData {
    /// Block number where deposit occurred
    pub block_number: u64,
    /// Transaction index in block
    pub transaction_index: u64,
    /// Log index in transaction receipt
    pub log_index: usize,
    /// Deposit hash (topics[1])
    pub deposit_hash: [u8; 32],
    /// Sender address (topics[2])
    pub sender: [u8; 20],
    /// Amount (from log data)
    pub amount: u64,
    /// Timestamp (from log data)
    pub timestamp: u64,
    /// Contract address that emitted the event
    pub contract_address: [u8; 20],
}

/// Receipt proof data for Merkle-Patricia Trie verification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptProof {
    /// RLP-encoded receipt
    pub receipt_rlp: Vec<u8>,
    /// Merkle-Patricia Trie proof (list of trie nodes)
    pub proof_nodes: Vec<Vec<u8>>,
    /// Receipt root from block header
    pub receipt_root: [u8; 32],
    /// Block header RLP
    pub block_header_rlp: Vec<u8>,
}

/// Input for deposit proof generation
#[derive(Debug, Clone)]
pub struct DepositProofInput {
    /// Withdrawal hash (secret)
    pub withdrawal_hash: [u8; 32],
    /// Nullifier preimage (secret)
    pub nullifier_preimage: [u8; 32],
    /// Deposit event data
    pub event_data: DepositEventData,
    /// Receipt proof
    pub receipt_proof: ReceiptProof,
}

/// Output of deposit proof generation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepositProofOutput {
    /// The ZK proof bytes
    pub proof: Vec<u8>,
    /// Public inputs
    pub nullifier: [u8; 32],
    pub recipient: [u8; 20],
    pub amount: u64,
    pub contract_address: [u8; 20],
}

