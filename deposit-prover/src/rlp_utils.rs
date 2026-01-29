use anyhow::{anyhow, Result};
use ethers::types::{Block, Log, TransactionReceipt};
use rlp::RlpStream;

/// RLP encode a transaction receipt
/// 
/// Receipt structure (EIP-2718):
/// - For legacy transactions (type 0): RLP([status, cumulativeGasUsed, logsBloom, logs])
/// - For typed transactions: type || RLP([status, cumulativeGasUsed, logsBloom, logs])
pub fn encode_receipt(receipt: &TransactionReceipt) -> Result<Vec<u8>> {
    let mut stream = RlpStream::new_list(4);
    
    // Status (1 for success, 0 for failure)
    let status = receipt.status
        .ok_or_else(|| anyhow!("Receipt status not found"))?;
    stream.append(&status);
    
    // Cumulative gas used
    stream.append(&receipt.cumulative_gas_used);
    
    // Logs bloom
    stream.append(&receipt.logs_bloom.0.as_ref());
    
    // Logs array
    stream.begin_list(receipt.logs.len());
    for log in &receipt.logs {
        encode_log_into_stream(&mut stream, log)?;
    }
    
    let encoded = stream.out().to_vec();
    
    // Check if this is a typed transaction (EIP-2718)
    // Type 0 (legacy) = no prefix
    // Type 1 (EIP-2930) = 0x01 prefix
    // Type 2 (EIP-1559) = 0x02 prefix
    if let Some(tx_type) = receipt.transaction_type {
        if tx_type.as_u64() > 0 {
            // Prepend transaction type byte
            let mut typed_receipt = vec![tx_type.as_u64() as u8];
            typed_receipt.extend_from_slice(&encoded);
            return Ok(typed_receipt);
        }
    }
    
    Ok(encoded)
}

/// Encode a log into an RLP stream
fn encode_log_into_stream(stream: &mut RlpStream, log: &Log) -> Result<()> {
    stream.begin_list(3);
    
    // Address
    stream.append(&log.address.as_bytes());
    
    // Topics
    stream.begin_list(log.topics.len());
    for topic in &log.topics {
        stream.append(&topic.as_bytes());
    }
    
    // Data
    stream.append(&log.data.to_vec());
    
    Ok(())
}

/// RLP encode a block header
/// 
/// Block header structure:
/// [parentHash, ommersHash, beneficiary, stateRoot, transactionsRoot, receiptsRoot,
///  logsBloom, difficulty, number, gasLimit, gasUsed, timestamp, extraData,
///  mixHash, nonce, baseFeePerGas (EIP-1559), withdrawalsRoot (EIP-4895)]
pub fn encode_block_header<T>(block: &Block<T>) -> Result<Vec<u8>> {
    let mut stream = RlpStream::new();
    
    // Determine list length based on block type
    let has_base_fee = block.base_fee_per_gas.is_some();
    let has_withdrawals_root = block.withdrawals_root.is_some();
    
    let list_len = if has_withdrawals_root {
        17 // Post-Shanghai (EIP-4895)
    } else if has_base_fee {
        16 // Post-London (EIP-1559)
    } else {
        15 // Pre-London
    };
    
    stream.begin_list(list_len);
    
    // 1. Parent hash
    stream.append(&block.parent_hash.as_bytes());
    
    // 2. Ommers hash (uncles hash)
    stream.append(&block.uncles_hash.as_bytes());
    
    // 3. Beneficiary (miner/author)
    stream.append(&block.author.unwrap_or_default().as_bytes());
    
    // 4. State root
    stream.append(&block.state_root.as_bytes());
    
    // 5. Transactions root
    stream.append(&block.transactions_root.as_bytes());
    
    // 6. Receipts root
    stream.append(&block.receipts_root.as_bytes());
    
    // 7. Logs bloom
    stream.append(&block.logs_bloom.unwrap_or_default().0.as_ref());
    
    // 8. Difficulty
    stream.append(&block.difficulty);
    
    // 9. Number
    stream.append(&block.number.ok_or_else(|| anyhow!("Block number not found"))?);
    
    // 10. Gas limit
    stream.append(&block.gas_limit);
    
    // 11. Gas used
    stream.append(&block.gas_used);
    
    // 12. Timestamp
    stream.append(&block.timestamp);
    
    // 13. Extra data
    stream.append(&block.extra_data.to_vec());
    
    // 14. Mix hash
    stream.append(&block.mix_hash.unwrap_or_default().as_bytes());
    
    // 15. Nonce
    let nonce_bytes = block.nonce.unwrap_or_default().to_low_u64_be().to_be_bytes();
    stream.append(&nonce_bytes.as_ref());
    
    // 16. Base fee per gas (EIP-1559, post-London)
    if has_base_fee {
        stream.append(&block.base_fee_per_gas.unwrap());
    }
    
    // 17. Withdrawals root (EIP-4895, post-Shanghai)
    if has_withdrawals_root {
        stream.append(&block.withdrawals_root.unwrap().as_bytes());
    }
    
    Ok(stream.out().to_vec())
}

/// Encode transaction index for use as trie key
/// 
/// In Ethereum's receipt trie, the key is RLP(transaction_index)
pub fn encode_tx_index(index: u64) -> Vec<u8> {
    let mut stream = RlpStream::new();
    stream.append(&index);
    stream.out().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethers::types::{Address, Bytes, H256};
    
    #[test]
    fn test_encode_tx_index() {
        // Index 0 should encode as 0x80 (empty string)
        let encoded = encode_tx_index(0);
        assert_eq!(encoded, vec![0x80]);
        
        // Index 1 should encode as 0x01
        let encoded = encode_tx_index(1);
        assert_eq!(encoded, vec![0x01]);
        
        // Index 127 should encode as 0x7f
        let encoded = encode_tx_index(127);
        assert_eq!(encoded, vec![0x7f]);
        
        // Index 128 should encode as 0x81, 0x80
        let encoded = encode_tx_index(128);
        assert_eq!(encoded, vec![0x81, 0x80]);
    }
    
    #[test]
    fn test_encode_log() {
        let log = Log {
            address: Address::zero(),
            topics: vec![H256::zero()],
            data: Bytes::from(vec![1, 2, 3]),
            block_hash: None,
            block_number: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            transaction_log_index: None,
            log_type: None,
            removed: None,
        };
        
        let mut stream = RlpStream::new();
        encode_log_into_stream(&mut stream, &log).unwrap();
        let encoded = stream.out().to_vec();
        
        // Should be a list of 3 items: [address, topics, data]
        assert!(encoded.len() > 0);
        assert_eq!(encoded[0] & 0xc0, 0xc0); // List marker
    }
}

