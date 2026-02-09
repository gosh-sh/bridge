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
    /// Unique deposit ID (prevents double-spending)
    pub deposit_id: u64,
    /// Sender address (topics[2])
    pub sender: [u8; 20],
    /// Amount (from log data) - full uint256 (32 bytes big-endian)
    /// FIX BC-TYPES-001: Changed from u64 to [u8; 32] to support amounts >
    /// 18.44 ETH
    pub amount: [u8; 32],
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepositProofInput {
    /// Deposit event data (public)
    pub event_data: DepositEventData,
    /// Receipt proof (private - proves event exists in Ethereum state)
    pub receipt_proof: ReceiptProof,
}

/// Output of deposit proof generation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepositProofOutput {
    /// The ZK proof bytes (binary)
    pub proof: Vec<u8>,
    /// The ZK proof bytes (hex-encoded for easy use in scripts)
    pub proof_bytes: String,
    /// Public inputs (verified by the circuit)
    pub deposit_id: u64,
    pub sender: [u8; 20],
    /// Amount - full uint256 (32 bytes big-endian)
    /// FIX BC-TYPES-001: Changed from u64 to [u8; 32] to support amounts >
    /// 18.44 ETH
    pub amount: [u8; 32],
    pub contract_address: [u8; 20],
    /// Block hash (32 bytes) - proves the deposit is from a real Ethereum block
    pub block_hash: [u8; 32],
}

impl DepositProofOutput {
    /// Create a new proof output with hex-encoded proof bytes
    pub fn new(
        proof: Vec<u8>,
        deposit_id: u64,
        sender: [u8; 20],
        amount: [u8; 32],
        contract_address: [u8; 20],
        block_hash: [u8; 32],
    ) -> Self {
        let proof_bytes = format!("0x{}", hex::encode(&proof));
        Self {
            proof,
            proof_bytes,
            deposit_id,
            sender,
            amount,
            contract_address,
            block_hash,
        }
    }
}
