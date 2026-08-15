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
    types::{ReceiptProof, TransactionProof},
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
    let block = minimal_block_header(H256::from(receipt_root), H256::zero());
    let block_header_rlp = crate::rlp_utils::encode_block_header(&block)?;

    Ok(ReceiptProof {
        receipt_rlp,
        proof_nodes,
        receipt_root,
        block_header_rlp,
    })
}

fn minimal_block_header(receipts_root: H256, transactions_root: H256) -> Block<Transaction> {
    Block {
        parent_hash: H256::zero(),
        uncles_hash: H256::from(EMPTY_UNCLES_HASH),
        author: Some(Address::zero()),
        state_root: H256::zero(),
        transactions_root,
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

    // RLP encode the block header, asserting it reproduces the canonical hash —
    // a silently truncated header would still hash to something and become the
    // circuit's `blockHash` public input.
    let block_header_rlp = crate::rlp_utils::verify_block_header_rlp(&block)?;
    println!("✅ Block header RLP reproduces the canonical block hash");

    Ok(ReceiptProof {
        receipt_rlp,
        proof_nodes: proof,
        receipt_root: receipts_root,
        block_header_rlp,
    })
}

/// Generate a Merkle-Patricia Trie proof for the enclosing transaction.
///
/// Reconstructs the block's transactions trie (same pattern as receipts —
/// there is no `eth_getProof` for the tx trie) and returns the leaf + path for
/// `rlp(tx_index)`. The leaf value is the typed-tx wire encoding (EIP-1559
/// starts with `0x02`), which the circuit RLP-decodes to bind `chain_id`.
pub async fn generate_transaction_proof(
    provider: Arc<Provider<Http>>,
    block_number: u64,
    tx_index: u64,
) -> Result<TransactionProof> {
    use axiom_eth::providers::transaction::get_tx_key_from_index;
    use cita_trie::{MemoryDB, PatriciaTrie, Trie};
    use hasher::HasherKeccak;

    println!(
        "🔍 Fetching block {} with full transactions (for tx MPT proof)...",
        block_number
    );
    let block = provider
        .get_block_with_txs(block_number)
        .await
        .context("Failed to fetch block with txs")?
        .ok_or_else(|| anyhow!("Block {} not found", block_number))?;

    let tx_count = block.transactions.len();
    if (tx_index as usize) >= tx_count {
        return Err(anyhow!(
            "tx_index {} out of range (block has {} txs)",
            tx_index,
            tx_count
        ));
    }

    let transactions_root: [u8; 32] = block.transactions_root.into();

    // Build the transactions trie and verify against the header root.
    let memdb = Arc::new(MemoryDB::new(true));
    let hasher = Arc::new(HasherKeccak::new());
    let mut trie = PatriciaTrie::new(Arc::clone(&memdb), Arc::clone(&hasher));
    let mut target_tx_bytes = None;
    for (idx, tx) in block.transactions.iter().enumerate() {
        let key = get_tx_key_from_index(idx);
        // Use the node's canonical signed-tx wire bytes as the trie leaf. This is
        // the exact encoding the block's `transactionsRoot` commits to for EVERY
        // tx type (legacy / 2930 / 1559 / 4844-blob / 7702-setcode). Re-encoding
        // from the JSON tx object via a type-specific RLP encoder silently
        // mis-handles blob (type 3, sidecar-stripped envelope) and setcode
        // (type 4, `authorization_list`) transactions, which makes the
        // reconstructed root diverge from the header whenever a congested live
        // block contains one of those — even though our own deposit tx is 1559.
        let raw: ethers::types::Bytes = provider
            .request("eth_getRawTransactionByHash", [tx.hash])
            .await
            .context(format!("Failed to fetch raw tx {} ({:?})", idx, tx.hash))?;
        let tx_rlp = raw.to_vec();
        if idx == tx_index as usize {
            target_tx_bytes = Some(tx_rlp.clone());
        }
        trie.insert(key, tx_rlp)
            .context(format!("Failed to insert tx {} into trie", idx))?;
    }
    let computed_root = trie.root()?;
    if computed_root.as_slice() != transactions_root {
        return Err(anyhow!(
            "Transactions trie root mismatch! Computed: {:?}, Expected: {:?}",
            computed_root,
            transactions_root
        ));
    }

    let tx_bytes = target_tx_bytes.ok_or_else(|| anyhow!("target tx bytes missing"))?;
    // Reject non-EIP-1559 early so MockProver / prove fail with a clear error
    // rather than an opaque RLP constraint failure.
    if tx_bytes.first() != Some(&crate::rlp_utils::EIP1559_TX_TYPE) {
        return Err(anyhow!(
            "deposit enclosing tx must be EIP-1559 (type 0x02); got first byte {:#x}. \
             The circuit binds chain_id from the typed-tx RLP, which only type 0x02 \
             exposes, so this deposit cannot be proven as-is — the depositor must \
             re-send with an EIP-1559 transaction.",
            tx_bytes.first().copied().unwrap_or(0)
        ));
    }

    let key = get_tx_key_from_index(tx_index as usize);
    let proof = trie.get_proof(&key)?;
    println!(
        "✅ Generated tx MPT proof ({} nodes, {} wire bytes)",
        proof.len(),
        tx_bytes.len()
    );

    Ok(TransactionProof {
        tx_bytes,
        proof_nodes: proof,
        transactions_root,
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

/// Build a transactions trie proof for a single typed-tx leaf (synthetic / keygen).
pub fn transaction_proof_from_wire_bytes(tx_bytes: Vec<u8>, tx_index: u64) -> Result<TransactionProof> {
    use axiom_eth::providers::transaction::get_tx_key_from_index;

    if tx_bytes.first() != Some(&crate::rlp_utils::EIP1559_TX_TYPE) {
        return Err(anyhow!(
            "synthetic tx proof requires EIP-1559 wire bytes (0x02 prefix)"
        ));
    }

    let memdb = Arc::new(MemoryDB::new(true));
    let hasher = Arc::new(HasherKeccak::new());
    let mut trie = PatriciaTrie::new(Arc::clone(&memdb), Arc::clone(&hasher));
    let key = get_tx_key_from_index(tx_index as usize);
    trie.insert(key.clone(), tx_bytes.clone()).context("insert tx leaf")?;
    let transactions_root: [u8; 32] = trie
        .root()?
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("transactions trie root must be 32 bytes"))?;
    let proof_nodes = trie.get_proof(&key)?;

    Ok(TransactionProof {
        tx_bytes,
        proof_nodes,
        transactions_root,
    })
}

/// Re-encode `block_header_rlp` so field 4/5 match the trie roots in the witness.
pub fn align_block_header_roots(
    receipt_proof: &mut ReceiptProof,
    transactions_root: [u8; 32],
) -> Result<()> {
    let block = minimal_block_header(
        H256::from_slice(&receipt_proof.receipt_root),
        H256::from_slice(&transactions_root),
    );
    receipt_proof.block_header_rlp = crate::rlp_utils::encode_block_header(&block)?;
    Ok(())
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
