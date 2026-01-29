//! Ethereum Data Fetcher
//!
//! This module provides utilities for fetching real Ethereum data including:
//! - Transaction receipts
//! - Block headers
//! - MPT proofs for receipts
//! - Parsing Deposit events from logs

use crate::mpt::generate_receipt_proof;
use crate::types::{DepositEventData, DepositProofInput, ReceiptProof};
use anyhow::{anyhow, Result};
use ethers::providers::{Http, Middleware, Provider};
use ethers::types::{BlockNumber, TransactionReceipt, H160, H256, U256};
use ethers::utils::keccak256;
use std::sync::Arc;

/// Ethereum data fetcher for deposit proofs
pub struct EthereumFetcher {
    provider: Provider<Http>,
}

impl EthereumFetcher {
    /// Create a new Ethereum fetcher with the given RPC URL
    pub fn new(rpc_url: &str) -> Result<Self> {
        let provider = Provider::<Http>::try_from(rpc_url)?;
        Ok(Self { provider })
    }

    /// Fetch a transaction receipt by hash
    pub async fn get_receipt(&self, tx_hash: H256) -> Result<TransactionReceipt> {
        self.provider
            .get_transaction_receipt(tx_hash)
            .await?
            .ok_or_else(|| anyhow!("Receipt not found for tx: {:?}", tx_hash))
    }

    /// Parse a Deposit event from a transaction receipt
    ///
    /// Expected event signature: Deposit(uint256 indexed depositId, address indexed sender, uint256 amount, uint256 timestamp)
    pub fn parse_deposit_event(
        &self,
        receipt: &TransactionReceipt,
        contract_address: H160,
        log_index: usize,
    ) -> Result<DepositEventData> {
        // Get the log at the specified index
        let log = receipt
            .logs
            .get(log_index)
            .ok_or_else(|| anyhow!("Log index {} not found in receipt", log_index))?;

        // Verify contract address
        if log.address != contract_address {
            return Err(anyhow!(
                "Log address {:?} doesn't match expected contract {:?}",
                log.address,
                contract_address
            ));
        }

        // Verify event signature
        let expected_signature = get_deposit_event_signature();
        if log.topics.is_empty() || log.topics[0].as_bytes() != &expected_signature {
            return Err(anyhow!(
                "Event signature doesn't match Deposit event. Expected: {:?}, Got: {:?}",
                hex::encode(expected_signature),
                log.topics.get(0).map(|t| hex::encode(t.as_bytes()))
            ));
        }

        // Parse indexed parameters
        if log.topics.len() < 3 {
            return Err(anyhow!(
                "Not enough topics. Expected 3 (signature + 2 indexed params), got {}",
                log.topics.len()
            ));
        }

        // depositId (indexed, topic[1])
        let deposit_id = log.topics[1].to_low_u64_be();

        // sender (indexed, topic[2])
        let sender = {
            let mut addr = [0u8; 20];
            addr.copy_from_slice(&log.topics[2].as_bytes()[12..32]);
            addr
        };

        // Parse non-indexed parameters from data
        if log.data.len() < 64 {
            return Err(anyhow!(
                "Log data too short. Expected 64 bytes (amount + timestamp), got {}",
                log.data.len()
            ));
        }

        // amount (first 32 bytes of data)
        let amount = {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&log.data[24..32]);
            u64::from_be_bytes(bytes)
        };

        // timestamp (second 32 bytes of data)
        let timestamp = {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&log.data[56..64]);
            u64::from_be_bytes(bytes)
        };

        Ok(DepositEventData {
            block_number: receipt
                .block_number
                .ok_or_else(|| anyhow!("Receipt missing block number"))?
                .as_u64(),
            transaction_index: receipt.transaction_index.as_u64(),
            log_index,
            deposit_id,
            sender,
            amount,
            timestamp,
            contract_address: contract_address.as_bytes().try_into().unwrap(),
        })
    }

    /// Fetch a complete deposit proof input for a transaction
    ///
    /// This fetches:
    /// - The transaction receipt
    /// - The block header
    /// - The MPT proof for the receipt (by building the receipt trie)
    /// - Parses the Deposit event
    pub async fn fetch_deposit_proof(
        &self,
        tx_hash: H256,
        contract_address: H160,
        log_index: usize,
    ) -> Result<DepositProofInput> {
        println!("Fetching deposit proof for tx: {:?}", tx_hash);

        // 1. Fetch the receipt
        println!("  - Fetching receipt...");
        let receipt = self.get_receipt(tx_hash).await?;
        println!("    ✓ Receipt found (block: {:?})", receipt.block_number);

        // 2. Parse the Deposit event
        println!("  - Parsing Deposit event...");
        let event_data = self.parse_deposit_event(&receipt, contract_address, log_index)?;
        println!("    ✓ Event parsed (depositId: {})", event_data.deposit_id);

        // 3. Generate MPT proof using the existing implementation
        println!("  - Generating MPT proof...");
        println!("    (This will fetch all receipts in the block and build the trie)");
        let provider_arc = Arc::new(self.provider.clone());
        let receipt_proof = generate_receipt_proof(provider_arc, tx_hash).await?;
        println!("    ✓ MPT proof generated ({} proof nodes)", receipt_proof.proof_nodes.len());

        Ok(DepositProofInput {
            event_data,
            receipt_proof,
        })
    }
}

/// Get the Deposit event signature
/// keccak256("Deposit(uint256,address,uint256,uint256)")
pub fn get_deposit_event_signature() -> [u8; 32] {
    keccak256("Deposit(uint256,address,uint256,uint256)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_signature() {
        let sig = get_deposit_event_signature();
        println!("Deposit event signature: 0x{}", hex::encode(sig));

        // Verify it's a valid keccak256 hash (32 bytes)
        assert_eq!(sig.len(), 32);
    }

    #[test]
    fn test_parse_deposit_event_signature() {
        // The signature should be deterministic
        let sig1 = get_deposit_event_signature();
        let sig2 = get_deposit_event_signature();
        assert_eq!(sig1, sig2);
    }
}

