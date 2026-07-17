use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use cita_trie::{MemoryDB, PatriciaTrie, Trie};
use ethers::{
    providers::{Http, Middleware, Provider},
    types::{Address, Block, Bloom, H256, H64, Transaction, TransactionReceipt, U256, U64},
};
use hasher::HasherKeccak;

use crate::{
    rlp_utils::{encode_receipt, encode_tx_index},
    types::ReceiptProof,
};

/// Keccak256 hash of RLP([]) — canonical `ommersHash` for a block with no uncles.
const EMPTY_UNCLES_HASH: [u8; 32] = [
    0x1d, 0xcc, 0x4d, 0xe8, 0xde, 0xc7, 0x5d, 0x7a, 0xab, 0x85, 0xb5, 0x67, 0xb6, 0xcc, 0xd4,
    0x1a, 0xd3, 0x12, 0x45, 0x1b, 0x94, 0x8a, 0x74, 0x13, 0xf0, 0xa1, 0x42, 0xfd, 0x40, 0xd4,
    0x93, 0x47,
];

/// Build a consistent [`ReceiptProof`] for a single-receipt trie (synthetic / keygen).
pub fn receipt_proof_from_receipt(receipt: &TransactionReceipt) -> Result<ReceiptProof> {
    let tx_index = receipt.transaction_index.as_u64();
    let mut trie = build_receipt_trie(&[receipt.clone()])?;
    let root = trie.root()?;
    let receipt_root: [u8; 32] = root
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("receipt trie root must be 32 bytes"))?;

    let key = encode_tx_index(tx_index);
    let proof_nodes = trie.get_proof(&key)?;
    let receipt_rlp = encode_receipt(receipt)?;
    let block = minimal_block_header(H256::from(receipt_root));
    let block_header_rlp = crate::rlp_utils::encode_block_header(&block)?;

    Ok(ReceiptProof {
        receipt_rlp,
        proof_nodes,
        receipt_root,
        block_header_rlp,
    })
}

fn minimal_block_header(receipts_root: H256) -> Block<Transaction> {
    Block {
        parent_hash: H256::zero(),
        uncles_hash: H256::from(EMPTY_UNCLES_HASH),
        author: Some(Address::zero()),
        state_root: H256::zero(),
        transactions_root: H256::zero(),
        receipts_root,
        logs_bloom: Some(Bloom::default()),
        difficulty: U256::zero(),
        number: Some(U64::from(1)),
        gas_limit: U256::from(30_000_000),
        gas_used: U256::zero(),
        timestamp: U256::from(1_700_000_000),
        extra_data: Default::default(),
        mix_hash: Some(H256::zero()),
        nonce: Some(H64::zero()),
        transactions: vec![],
        ..Default::default()
    }
}

/// Generate a Merkle-Patricia Trie proof for a transaction receipt
///
/// This function:
/// 1. Fetches all receipts in the block
/// 2. Builds the receipt trie from scratch
/// 3. Extracts the proof path for the specific transaction
/// 4. Returns the proof along with the receipt and block header
pub async fn generate_receipt_proof(
    provider: Arc<Provider<Http>>,
    tx_hash: H256,
) -> Result<ReceiptProof> {
    println!("🔍 Fetching transaction receipt...");

    // Fetch the target receipt
    let receipt = provider
        .get_transaction_receipt(tx_hash)
        .await
        .context("Failed to fetch transaction receipt")?
        .ok_or_else(|| anyhow!("Transaction receipt not found"))?;

    let block_number = receipt
        .block_number
        .ok_or_else(|| anyhow!("Block number not found"))?;

    let tx_index = receipt.transaction_index.as_u64();

    println!(
        "📦 Block: {}, Transaction index: {}",
        block_number, tx_index
    );

    // Fetch the block
    println!("🔍 Fetching block {}...", block_number);
    let block = provider
        .get_block(block_number)
        .await
        .context("Failed to fetch block")?
        .ok_or_else(|| anyhow!("Block not found"))?;

    let receipts_root: [u8; 32] = block.receipts_root.into();
    let tx_count = block.transactions.len();

    println!("📊 Block has {} transactions", tx_count);

    // Fetch all receipts in the block
    println!("🔍 Fetching all {} receipts...", tx_count);
    let mut receipts = Vec::new();
    for (i, tx_hash) in block.transactions.iter().enumerate() {
        if i % 10 == 0 {
            println!("   Progress: {}/{}", i, tx_count);
        }

        let receipt = provider
            .get_transaction_receipt(*tx_hash)
            .await
            .context(format!("Failed to fetch receipt for tx {}", i))?
            .ok_or_else(|| anyhow!("Receipt not found for tx {}", i))?;

        receipts.push(receipt);
    }

    println!("✅ Fetched all receipts");

    // Build the receipt trie
    println!("🌳 Building receipt trie...");
    let mut trie = build_receipt_trie(&receipts)?;

    // Verify the trie root matches the block's receipts root
    let computed_root = trie.root()?;
    if computed_root.as_slice() != receipts_root {
        return Err(anyhow!(
            "Trie root mismatch! Computed: {:?}, Expected: {:?}",
            computed_root,
            receipts_root
        ));
    }

    println!("✅ Trie root verified: 0x{}", hex::encode(&receipts_root));

    // Generate proof for our transaction
    println!("🔐 Generating MPT proof for tx index {}...", tx_index);
    let key = encode_tx_index(tx_index);
    let proof = trie.get_proof(&key)?;

    println!("✅ Generated proof with {} nodes", proof.len());

    // RLP encode the receipt
    let receipt_rlp = encode_receipt(&receipt)?;

    // RLP encode the block header
    let block_header_rlp = crate::rlp_utils::encode_block_header(&block)?;

    Ok(ReceiptProof {
        receipt_rlp,
        proof_nodes: proof,
        receipt_root: receipts_root,
        block_header_rlp,
    })
}

/// Build a receipt trie from a list of receipts
///
/// The trie uses:
/// - Key: RLP(transaction_index)
/// - Value: RLP(receipt)
fn build_receipt_trie(
    receipts: &[TransactionReceipt],
) -> Result<PatriciaTrie<MemoryDB, HasherKeccak>> {
    let memdb = Arc::new(MemoryDB::new(true));
    let hasher = Arc::new(HasherKeccak::new());
    let mut trie = PatriciaTrie::new(Arc::clone(&memdb), Arc::clone(&hasher));

    for (i, receipt) in receipts.iter().enumerate() {
        // Encode the key (transaction index)
        let key = encode_tx_index(i as u64);

        // Encode the value (receipt)
        let value = encode_receipt(receipt).context(format!("Failed to encode receipt {}", i))?;

        // Insert into trie
        trie.insert(key, value)
            .context(format!("Failed to insert receipt {} into trie", i))?;
    }

    Ok(trie)
}

#[cfg(test)]
mod tests {
    use ethers::types::{Address, Bloom, U256, U64};

    use super::*;

    #[test]
    fn test_build_receipt_trie() {
        // Create a simple receipt
        let receipt = TransactionReceipt {
            transaction_hash: H256::zero(),
            transaction_index: U64::from(0),
            block_hash: Some(H256::zero()),
            block_number: Some(U64::from(1)),
            from: Address::zero(),
            to: Some(Address::zero()),
            cumulative_gas_used: U256::from(21000),
            gas_used: Some(U256::from(21000)),
            contract_address: None,
            logs: vec![],
            status: Some(U64::from(1)),
            root: None,
            logs_bloom: Bloom::default(),
            transaction_type: None,
            effective_gas_price: None,
            other: Default::default(),
        };

        // Build trie with single receipt
        let trie = build_receipt_trie(&[receipt]).unwrap();

        // Verify we can get the receipt back
        let key = encode_tx_index(0);
        let value = trie.get(&key).unwrap();
        assert!(value.is_some());
    }

    #[test]
    fn test_build_receipt_trie_multiple() {
        let mut receipts = vec![];

        for i in 0..10 {
            let receipt = TransactionReceipt {
                transaction_hash: H256::zero(),
                transaction_index: U64::from(i),
                block_hash: Some(H256::zero()),
                block_number: Some(U64::from(1)),
                from: Address::zero(),
                to: Some(Address::zero()),
                cumulative_gas_used: U256::from(21000 * (i + 1)),
                gas_used: Some(U256::from(21000)),
                contract_address: None,
                logs: vec![],
                status: Some(U64::from(1)),
                root: None,
                logs_bloom: Bloom::default(),
                transaction_type: None,
                effective_gas_price: None,
                other: Default::default(),
            };
            receipts.push(receipt);
        }

        // Build trie
        let trie = build_receipt_trie(&receipts).unwrap();

        // Verify all receipts are in the trie
        for i in 0..10 {
            let key = encode_tx_index(i);
            let value = trie.get(&key).unwrap();
            assert!(value.is_some(), "Receipt {} not found", i);
        }
    }
}
