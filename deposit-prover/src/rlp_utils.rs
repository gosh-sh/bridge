use alloy_rlp::{Encodable, RlpEncodable};
use anyhow::{anyhow, Result};
use ethers::types::{Block, Log, TransactionReceipt, U256, U64};

/// Wrapper for U256 that implements Encodable
struct RlpU256<'a>(&'a U256);

impl Encodable for RlpU256<'_> {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        let mut bytes = [0u8; 32];
        self.0.to_big_endian(&mut bytes);
        // Remove leading zeros
        let start = bytes.iter().position(|&b| b != 0).unwrap_or(32);
        bytes[start..].encode(out);
    }
}

/// Wrapper for U64 that implements Encodable
struct RlpU64<'a>(&'a U64);

impl Encodable for RlpU64<'_> {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        let mut bytes = [0u8; 8];
        self.0.to_big_endian(&mut bytes);
        // Remove leading zeros
        let start = bytes.iter().position(|&b| b != 0).unwrap_or(8);
        bytes[start..].encode(out);
    }
}

/// Wrapper for ethers Log that implements RlpEncodable
#[derive(RlpEncodable)]
struct RlpLog<'a> {
    address: &'a [u8; 20],
    topics: Vec<&'a [u8; 32]>,
    data: &'a [u8],
}

impl<'a> From<&'a Log> for RlpLog<'a> {
    fn from(log: &'a Log) -> Self {
        RlpLog {
            address: log.address.as_fixed_bytes(),
            topics: log.topics.iter().map(|t| t.as_fixed_bytes()).collect(),
            data: log.data.as_ref(),
        }
    }
}

/// RLP encode a transaction receipt
///
/// Receipt structure (EIP-2718):
/// - For legacy transactions (type 0): RLP([status, cumulativeGasUsed,
///   logsBloom, logs])
/// - For typed transactions: type || RLP([status, cumulativeGasUsed, logsBloom,
///   logs])
pub fn encode_receipt(receipt: &TransactionReceipt) -> Result<Vec<u8>> {
    // Status (1 for success, 0 for failure)
    let status = receipt
        .status
        .ok_or_else(|| anyhow!("Receipt status not found"))?;

    // Convert logs to RlpLog wrappers
    let rlp_logs: Vec<RlpLog> = receipt.logs.iter().map(|log| log.into()).collect();

    // Create a struct that represents the receipt for RLP encoding
    #[derive(RlpEncodable)]
    struct RlpReceipt<'a> {
        status: RlpU64<'a>,
        cumulative_gas_used: RlpU256<'a>,
        logs_bloom: &'a [u8],
        logs: Vec<RlpLog<'a>>,
    }

    let rlp_receipt = RlpReceipt {
        status: RlpU64(&status),
        cumulative_gas_used: RlpU256(&receipt.cumulative_gas_used),
        logs_bloom: receipt.logs_bloom.0.as_ref(),
        logs: rlp_logs,
    };

    // Encode using the derive macro
    let mut buf = Vec::new();
    rlp_receipt.encode(&mut buf);

    // Check if this is a typed transaction (EIP-2718)
    // Type 0 (legacy) = no prefix
    // Type 1 (EIP-2930) = 0x01 prefix
    // Type 2 (EIP-1559) = 0x02 prefix
    if let Some(tx_type) = receipt.transaction_type {
        if tx_type.as_u64() > 0 {
            // Prepend transaction type byte
            let mut typed_receipt = vec![tx_type.as_u64() as u8];
            typed_receipt.extend_from_slice(&buf);
            return Ok(typed_receipt);
        }
    }

    Ok(buf)
}

/// RLP encode a block header
///
/// Block header structure:
/// [parentHash, ommersHash, beneficiary, stateRoot, transactionsRoot,
/// receiptsRoot,  logsBloom, difficulty, number, gasLimit, gasUsed, timestamp,
/// extraData,  mixHash, nonce, baseFeePerGas (EIP-1559), withdrawalsRoot
/// (EIP-4895)]
pub fn encode_block_header<T>(block: &Block<T>) -> Result<Vec<u8>> {
    let number = block
        .number
        .ok_or_else(|| anyhow!("Block number not found"))?;

    let nonce_bytes = block
        .nonce
        .unwrap_or_default()
        .to_low_u64_be()
        .to_be_bytes();

    // For block headers with optional fields, we need to manually encode
    // because the derive macro doesn't handle optional fields well
    let mut buf = Vec::new();

    // Encode all fields in order
    block.parent_hash.as_bytes().encode(&mut buf);
    block.uncles_hash.as_bytes().encode(&mut buf);
    block.author.unwrap_or_default().as_bytes().encode(&mut buf);
    block.state_root.as_bytes().encode(&mut buf);
    block.transactions_root.as_bytes().encode(&mut buf);
    block.receipts_root.as_bytes().encode(&mut buf);
    block
        .logs_bloom
        .unwrap_or_default()
        .0
        .as_ref()
        .encode(&mut buf);
    RlpU256(&block.difficulty).encode(&mut buf);
    RlpU64(&number).encode(&mut buf);
    RlpU256(&block.gas_limit).encode(&mut buf);
    RlpU256(&block.gas_used).encode(&mut buf);
    RlpU256(&block.timestamp).encode(&mut buf);
    block.extra_data.as_ref().encode(&mut buf);
    block
        .mix_hash
        .unwrap_or_default()
        .as_bytes()
        .encode(&mut buf);
    nonce_bytes.as_ref().encode(&mut buf);

    // Optional fields
    if let Some(ref base_fee) = block.base_fee_per_gas {
        RlpU256(base_fee).encode(&mut buf);
    }
    if let Some(ref withdrawals_root) = block.withdrawals_root {
        withdrawals_root.as_bytes().encode(&mut buf);
    }

    // Wrap in list header
    let payload_len = buf.len();
    let mut result = Vec::new();
    alloy_rlp::Header {
        list: true,
        payload_length: payload_len,
    }
    .encode(&mut result);
    result.extend_from_slice(&buf);

    Ok(result)
}

/// Encode transaction index for use as trie key
///
/// In Ethereum's receipt trie, the key is RLP(transaction_index)
pub fn encode_tx_index(index: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    index.encode(&mut buf);
    buf
}

#[cfg(test)]
mod tests {
    use ethers::types::{Address, Bytes, H256};

    use super::*;

    #[test]
    fn test_encode_tx_index() {
        // Index 0 should encode as 0x80 (empty string)
        let encoded = encode_tx_index(0);
        assert_eq!(encoded, vec![0x80]);
        assert_eq!(encoded.len(), 1);

        // Index 1 should encode as 0x01
        let encoded = encode_tx_index(1);
        assert_eq!(encoded, vec![0x01]);
        assert_eq!(encoded.len(), 1);

        // Index 127 should encode as 0x7f
        let encoded = encode_tx_index(127);
        assert_eq!(encoded, vec![0x7f]);
        assert_eq!(encoded.len(), 1);

        // Index 128 should encode as 0x81, 0x80
        let encoded = encode_tx_index(128);
        assert_eq!(encoded, vec![0x81, 0x80]);
        assert_eq!(encoded.len(), 2);

        // Index 255 should encode as 0x81, 0xff
        let encoded = encode_tx_index(255);
        assert_eq!(encoded, vec![0x81, 0xff]);
        assert_eq!(encoded.len(), 2);

        // Index 256 should encode as 0x82, 0x01, 0x00 (3 bytes)
        let encoded = encode_tx_index(256);
        assert_eq!(encoded, vec![0x82, 0x01, 0x00]);
        assert_eq!(encoded.len(), 3);

        // Index 65535 (max 2-byte value) should encode as 0x82, 0xff, 0xff (3 bytes)
        let encoded = encode_tx_index(65535);
        assert_eq!(encoded, vec![0x82, 0xff, 0xff]);
        assert_eq!(encoded.len(), 3);

        // Index 65536 should encode as 0x83, 0x01, 0x00, 0x00 (4 bytes)
        let encoded = encode_tx_index(65536);
        assert_eq!(encoded, vec![0x83, 0x01, 0x00, 0x00]);
        assert_eq!(encoded.len(), 4);
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

        let rlp_log: RlpLog = (&log).into();
        let mut buf = Vec::new();
        rlp_log.encode(&mut buf);

        // Should be a list of 3 items: [address, topics, data]
        assert!(buf.len() > 0);
        assert_eq!(buf[0] & 0xc0, 0xc0); // List marker
    }
}
