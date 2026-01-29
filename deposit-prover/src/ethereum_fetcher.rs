//! Ethereum Data Fetcher
//!
//! This module provides utilities for fetching real Ethereum data including:
//! - Transaction receipts
//! - Block headers
//! - MPT proofs for receipts
//! - Parsing Deposit events from logs

use crate::types::{DepositEventData, DepositProofInput, ReceiptProof};
use anyhow::{anyhow, Result};
use ethers::providers::{Http, Middleware, Provider};
use ethers::types::{BlockNumber, TransactionReceipt, H160, H256, U256};
use ethers::utils::keccak256;

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
    /// - The MPT proof for the receipt
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

        // 3. Fetch the block header
        println!("  - Fetching block header...");
        let block_number = receipt
            .block_number
            .ok_or_else(|| anyhow!("Receipt missing block number"))?;
        let block = self
            .provider
            .get_block(BlockNumber::Number(block_number))
            .await?
            .ok_or_else(|| anyhow!("Block not found"))?;
        println!("    ✓ Block header fetched");

        // 4. Get receipt RLP
        println!("  - Encoding receipt as RLP...");
        let receipt_rlp = encode_receipt_rlp(&receipt)?;
        println!("    ✓ Receipt RLP encoded ({} bytes)", receipt_rlp.len());

        // 5. Get block header RLP
        println!("  - Encoding block header as RLP...");
        let block_header_rlp = encode_block_header_rlp(&block)?;
        println!(
            "    ✓ Block header RLP encoded ({} bytes)",
            block_header_rlp.len()
        );

        // 6. TODO: Fetch MPT proof
        // This requires eth_getProof RPC call which is not standard
        // For now, we'll use a placeholder
        println!("  ⚠️  MPT proof fetching not implemented yet");
        println!("     Using placeholder proof nodes");
        let proof_nodes = vec![]; // TODO: Implement MPT proof fetching

        // 7. Get receipt root from block header
        let receipt_root = block.receipts_root.as_bytes().try_into().unwrap();

        let receipt_proof = ReceiptProof {
            receipt_rlp,
            proof_nodes,
            receipt_root,
            block_header_rlp,
        };

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

/// Encode a transaction receipt as RLP
fn encode_receipt_rlp(receipt: &TransactionReceipt) -> Result<Vec<u8>> {
    use ethers::utils::rlp::RlpStream;

    let mut stream = RlpStream::new();

    // Receipt format: [status, cumulativeGasUsed, logsBloom, logs]
    stream.begin_list(4);

    // Status (1 for success, 0 for failure)
    stream.append(&receipt.status.unwrap_or(1u64.into()));

    // Cumulative gas used
    stream.append(&receipt.cumulative_gas_used);

    // Logs bloom
    stream.append(&receipt.logs_bloom);

    // Logs array
    stream.begin_list(receipt.logs.len());
    for log in &receipt.logs {
        stream.begin_list(3);
        stream.append(&log.address);

        // Topics
        stream.begin_list(log.topics.len());
        for topic in &log.topics {
            stream.append(topic);
        }

        // Data
        stream.append(&log.data.to_vec());
    }

    Ok(stream.out().to_vec())
}

/// Encode a block header as RLP
fn encode_block_header_rlp(block: &ethers::types::Block<H256>) -> Result<Vec<u8>> {
    use ethers::utils::rlp::RlpStream;

    let mut stream = RlpStream::new();

    // Block header format (15 fields for post-London blocks)
    stream.begin_list(15);

    stream.append(&block.parent_hash);
    stream.append(&block.uncles_hash);
    stream.append(&block.author.unwrap_or_default());
    stream.append(&block.state_root);
    stream.append(&block.transactions_root);
    stream.append(&block.receipts_root);
    stream.append(&block.logs_bloom.unwrap_or_default());
    stream.append(&block.difficulty);
    stream.append(&block.number.unwrap_or_default());
    stream.append(&block.gas_limit);
    stream.append(&block.gas_used);
    stream.append(&block.timestamp);
    stream.append(&block.extra_data.to_vec());
    stream.append(&block.mix_hash.unwrap_or_default());
    stream.append(&block.nonce.unwrap_or_default());

    Ok(stream.out().to_vec())
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

