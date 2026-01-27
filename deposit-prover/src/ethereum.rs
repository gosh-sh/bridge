//! Ethereum client for fetching deposit events and generating receipt proofs

use anyhow::{anyhow, Context, Result};
use ethers::prelude::*;
use ethers::providers::{Http, Provider};

use crate::types::{DepositEventData, ReceiptProof};

/// Ethereum client for fetching deposit event data
pub struct EthereumClient {
    provider: Provider<Http>,
}

impl EthereumClient {
    /// Create a new Ethereum client
    pub fn new(provider: Provider<Http>) -> Self {
        Self { provider }
    }

    /// Fetch deposit event from transaction receipt
    pub async fn fetch_deposit_event(
        &self,
        tx_hash: &str,
        contract_address: &str,
    ) -> Result<DepositEventData> {
        // Parse transaction hash
        let tx_hash: H256 = tx_hash.parse()
            .context("Invalid transaction hash")?;

        // Parse contract address
        let contract_address: Address = contract_address.parse()
            .context("Invalid contract address")?;

        // Fetch transaction receipt
        let receipt = self.provider
            .get_transaction_receipt(tx_hash)
            .await
            .context("Failed to fetch transaction receipt")?
            .ok_or_else(|| anyhow!("Transaction receipt not found"))?;

        // Find Deposit event
        // Event signature: Deposit(bytes32 indexed depositHash, address indexed sender, uint256 amount, uint256 timestamp)
        let deposit_event_signature = ethers::core::utils::keccak256(
            "Deposit(bytes32,address,uint256,uint256)"
        );

        let log = receipt.logs.iter()
            .enumerate()
            .find(|(_, log)| {
                log.address == contract_address &&
                !log.topics.is_empty() &&
                log.topics[0].as_bytes() == deposit_event_signature
            })
            .ok_or_else(|| anyhow!("Deposit event not found in transaction"))?;

        let log_index = log.0;
        let log = log.1;

        // Parse event data
        if log.topics.len() < 3 {
            return Err(anyhow!("Invalid Deposit event: not enough topics"));
        }

        let deposit_hash: [u8; 32] = log.topics[1].into();
        let sender_bytes: [u8; 32] = log.topics[2].into();
        let sender: [u8; 20] = sender_bytes[12..32].try_into().unwrap();

        // Decode amount and timestamp from data
        if log.data.len() < 64 {
            return Err(anyhow!("Invalid Deposit event: data too short"));
        }

        let amount = U256::from_big_endian(&log.data[0..32]);
        let timestamp = U256::from_big_endian(&log.data[32..64]);

        Ok(DepositEventData {
            block_number: receipt.block_number
                .ok_or_else(|| anyhow!("Block number not found"))?
                .as_u64(),
            transaction_index: receipt.transaction_index.as_u64(),
            log_index,
            deposit_hash,
            sender,
            amount: amount.as_u64(),
            timestamp: timestamp.as_u64(),
            contract_address: contract_address.into(),
        })
    }

    /// Generate Merkle-Patricia Trie proof for transaction receipt
    pub async fn generate_receipt_proof(&self, tx_hash: &str) -> Result<ReceiptProof> {
        // Parse transaction hash
        let tx_hash: H256 = tx_hash.parse()
            .context("Invalid transaction hash")?;

        // Fetch transaction receipt
        let receipt = self.provider
            .get_transaction_receipt(tx_hash)
            .await
            .context("Failed to fetch transaction receipt")?
            .ok_or_else(|| anyhow!("Transaction receipt not found"))?;

        let block_number = receipt.block_number
            .ok_or_else(|| anyhow!("Block number not found"))?;

        // Fetch block header
        let block = self.provider
            .get_block(block_number)
            .await
            .context("Failed to fetch block")?
            .ok_or_else(|| anyhow!("Block not found"))?;

        let receipt_root: [u8; 32] = block.receipts_root.into();

        // Generate MPT proof for receipt
        //
        // Ethereum receipts are stored in a Merkle-Patricia Trie where:
        // - Key: RLP(transaction_index)
        // - Value: RLP(receipt)
        // - Root: receipts_root in block header
        //
        // To prove a receipt exists, we need:
        // 1. RLP-encoded receipt
        // 2. MPT proof (list of trie nodes from root to leaf)
        // 3. Block header (contains receipts_root)
        //
        // Options for generating the proof:
        // A) Use eth_getProof (not available for receipts, only for storage)
        // B) Fetch all receipts and build trie manually
        // C) Use a third-party service (e.g., Axiom, Herodotus)
        //
        // For now, we'll implement option B (manual trie building)
        // This is the most decentralized approach

        println!("⚠️  MPT proof generation not yet implemented");
        println!("    This requires:");
        println!("    1. Fetching all receipts in block {}", block_number);
        println!("    2. Building receipt trie from scratch");
        println!("    3. Generating proof path for tx index {}", receipt.transaction_index);
        println!("    4. RLP encoding receipt and block header");

        // Placeholder implementation
        let receipt_rlp = vec![]; // TODO: RLP encode receipt
        let proof_nodes = vec![]; // TODO: Build trie and extract proof
        let block_header_rlp = vec![]; // TODO: RLP encode block header

        Ok(ReceiptProof {
            receipt_rlp,
            proof_nodes,
            receipt_root,
            block_header_rlp,
        })
    }
}

