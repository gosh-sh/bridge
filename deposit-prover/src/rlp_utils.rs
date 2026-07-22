use alloy_rlp::{Encodable, RlpEncodable};
use anyhow::{anyhow, Result};
use ethers::types::{Block, Log, TransactionReceipt, U256, U64};
use serde::Deserialize;

/// EIP-2718 / OP Stack deposit transaction type (`DepositTxType` = 0x7e).
pub const DEPOSIT_TX_TYPE: u64 = 0x7e;

/// OP Stack deposit-receipt extras carried in RPC `other` fields.
///
/// Matches axiom-eth `OptimismTransactionReceiptFields` (Canyon layout):
/// <https://specs.optimism.io/protocol/deposits.html#deposit-receipt>
#[derive(Clone, Copy, Default, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OptimismDepositReceiptFields {
    deposit_nonce: Option<U64>,
    deposit_receipt_version: Option<U64>,
}

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
///
/// OP Stack deposit txs (`type == 0x7e`, Canyon+): when the RPC provides
/// `depositReceiptVersion`, the payload is
/// RLP([status, cumulativeGasUsed, logsBloom, logs, depositNonce,
/// depositReceiptVersion]) — matching axiom-eth `get_receipt_rlp`. Pre-Canyon
/// deposit receipts omit those two trailing fields even if `depositNonce` is
/// present in the RPC response. User bridge deposits remain EIP-1559 `0x02`;
/// this path exists so blocks that also contain sequencer deposit txs still
/// rebuild `receiptsRoot`.
pub fn encode_receipt(receipt: &TransactionReceipt) -> Result<Vec<u8>> {
    // Status (1 for success, 0 for failure) — prefer status; fall back to
    // pre-EIP-658 state root (same as axiom-eth).
    let status_or_root_encoded = {
        let mut tmp = Vec::new();
        if let Some(ref root) = receipt.root {
            root.as_bytes().encode(&mut tmp);
        } else {
            let status = receipt
                .status
                .ok_or_else(|| anyhow!("Receipt status not found"))?;
            RlpU64(&status).encode(&mut tmp);
        }
        tmp
    };

    // Convert logs to RlpLog wrappers
    let rlp_logs: Vec<RlpLog> = receipt.logs.iter().map(|log| log.into()).collect();

    // Manually assemble the list payload so we can optionally append Canyon
    // deposit fields (derive macro can't express a variable-length list).
    let mut payload = Vec::new();
    payload.extend_from_slice(&status_or_root_encoded);
    RlpU256(&receipt.cumulative_gas_used).encode(&mut payload);
    receipt.logs_bloom.0.as_ref().encode(&mut payload);
    {
        // Encode logs as an RLP list
        let mut logs_payload = Vec::new();
        for log in &rlp_logs {
            log.encode(&mut logs_payload);
        }
        let logs_len = logs_payload.len();
        alloy_rlp::Header {
            list: true,
            payload_length: logs_len,
        }
        .encode(&mut payload);
        payload.extend_from_slice(&logs_payload);
    }

    // OP Stack Canyon deposit receipt extras
    // https://specs.optimism.io/protocol/deposits.html#deposit-receipt
    // https://github.com/axiom-crypto/axiom-eth (providers/receipt.rs::get_receipt_rlp)
    if receipt.transaction_type == Some(U64::from(DEPOSIT_TX_TYPE)) {
        let op_fields: OptimismDepositReceiptFields = receipt
            .other
            .clone()
            .deserialize_into()
            .map_err(|e| anyhow!("Failed to parse OP deposit receipt fields: {e}"))?;
        if let Some(deposit_receipt_version) = op_fields.deposit_receipt_version {
            // RPC providers often surface depositNonce even before Canyon; the
            // version field is the indicator that both must be in the RLP.
            let deposit_nonce = op_fields
                .deposit_nonce
                .ok_or_else(|| anyhow!("Canyon deposit receipt without depositNonce"))?;
            RlpU64(&deposit_nonce).encode(&mut payload);
            RlpU64(&deposit_receipt_version).encode(&mut payload);
        }
    }

    let mut buf = Vec::new();
    let payload_len = payload.len();
    alloy_rlp::Header {
        list: true,
        payload_length: payload_len,
    }
    .encode(&mut buf);
    buf.extend_from_slice(&payload);

    // Check if this is a typed transaction (EIP-2718)
    // Type 0 (legacy) = no prefix
    // Type 1 (EIP-2930) = 0x01 prefix
    // Type 2 (EIP-1559) = 0x02 prefix
    // Type 0x7e (OP deposit) = 0x7e prefix
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
/// Block header structure (through Cancun / OP Stack Ecotone):
/// [parentHash, ommersHash, beneficiary, stateRoot, transactionsRoot,
/// receiptsRoot, logsBloom, difficulty, number, gasLimit, gasUsed, timestamp,
/// extraData, mixHash, nonce, baseFeePerGas?, withdrawalsRoot?, blobGasUsed?,
/// excessBlobGas?, parentBeaconBlockRoot?]
///
/// Optional fields are appended only when present so `keccak(header) ==
/// block.hash` on both L1 Cancun and OP Stack Ecotone. Encoding matches
/// axiom-eth `providers/block.rs::get_block_rlp` (ethers `rlp` crate).
pub fn encode_block_header<T>(block: &Block<T>) -> Result<Vec<u8>> {
    use rlp::RlpStream;

    let base_fee = block.base_fee_per_gas;
    let withdrawals_root = block.withdrawals_root;
    let blob_gas_used = block.blob_gas_used;
    let excess_blob_gas = block.excess_blob_gas;
    let parent_beacon_block_root = block.parent_beacon_block_root;

    let mut rlp_len = 15;
    for opt in [
        base_fee.is_some(),
        withdrawals_root.is_some(),
        blob_gas_used.is_some(),
        excess_blob_gas.is_some(),
        parent_beacon_block_root.is_some(),
    ] {
        rlp_len += opt as usize;
    }
    let mut rlp = RlpStream::new_list(rlp_len);
    rlp.append(&block.parent_hash);
    rlp.append(&block.uncles_hash);
    rlp.append(
        &block
            .author
            .ok_or_else(|| anyhow!("Block author (beneficiary) not found"))?,
    );
    rlp.append(&block.state_root);
    rlp.append(&block.transactions_root);
    rlp.append(&block.receipts_root);
    rlp.append(
        &block
            .logs_bloom
            .ok_or_else(|| anyhow!("Block logsBloom not found"))?,
    );
    rlp.append(&block.difficulty);
    rlp.append(
        &block
            .number
            .ok_or_else(|| anyhow!("Block number not found"))?,
    );
    rlp.append(&block.gas_limit);
    rlp.append(&block.gas_used);
    rlp.append(&block.timestamp);
    rlp.append(&block.extra_data.to_vec());
    rlp.append(
        &block
            .mix_hash
            .ok_or_else(|| anyhow!("Block mixHash not found"))?,
    );
    rlp.append(
        &block
            .nonce
            .ok_or_else(|| anyhow!("Block nonce not found"))?,
    );
    if let Some(base_fee) = base_fee {
        rlp.append(&base_fee);
    }
    if let Some(withdrawals_root) = withdrawals_root {
        rlp.append(&withdrawals_root);
    }
    if let Some(blob_gas_used) = blob_gas_used {
        rlp.append(&blob_gas_used);
    }
    if let Some(excess_blob_gas) = excess_blob_gas {
        rlp.append(&excess_blob_gas);
    }
    if let Some(parent_beacon_block_root) = parent_beacon_block_root {
        rlp.append(&parent_beacon_block_root);
    }
    Ok(rlp.out().into())
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
    use ethers::types::{Address, Bloom, Bytes, OtherFields, H256, H64};
    use ethers::utils::keccak256;

    use super::*;

    /// Build an `OtherFields` map with the given camelCase JSON keys.
    fn other_fields_from_json(pairs: &[(&str, serde_json::Value)]) -> OtherFields {
        let mut other = OtherFields::default();
        for &(k, ref v) in pairs {
            other.insert(k.to_string(), v.clone());
        }
        other
    }

    fn sample_block() -> Block<H256> {
        Block {
            hash: Some(H256::zero()),
            parent_hash: H256::from_low_u64_be(1),
            uncles_hash: H256::from_low_u64_be(2),
            author: Some(Address::zero()),
            state_root: H256::from_low_u64_be(3),
            transactions_root: H256::from_low_u64_be(4),
            receipts_root: H256::from_low_u64_be(5),
            number: Some(U64::from(100)),
            gas_used: U256::from(21_000u64),
            gas_limit: U256::from(30_000_000u64),
            extra_data: Bytes::default(),
            logs_bloom: Some(Bloom::default()),
            timestamp: U256::from(1_700_000_000u64),
            difficulty: U256::zero(),
            total_difficulty: None,
            seal_fields: vec![],
            uncles: vec![],
            transactions: vec![],
            size: None,
            mix_hash: Some(H256::from_low_u64_be(6)),
            nonce: Some(H64::zero()),
            base_fee_per_gas: Some(U256::from(7u64)),
            withdrawals_root: Some(H256::from_low_u64_be(8)),
            withdrawals: None,
            blob_gas_used: None,
            excess_blob_gas: None,
            parent_beacon_block_root: None,
            other: Default::default(),
        }
    }

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

    /// Cancun/Ecotone: when blob fields are present they must be appended so the
    /// header RLP commits to them (otherwise `keccak(header) ≠ block.hash` on
    /// OP Stack Ecotone / L1 Cancun).
    #[test]
    fn test_encode_block_header_includes_blob_fields() {
        let mut block = sample_block();
        let without_blob = encode_block_header(&block).unwrap();

        block.blob_gas_used = Some(U256::from(0x20000u64)); // 131072
        block.excess_blob_gas = Some(U256::from(0u64));
        block.parent_beacon_block_root = Some(H256::from_low_u64_be(0xdead));

        let with_blob = encode_block_header(&block).unwrap();

        // Cancun fields lengthen the header; both encodings stay under the
        // circuit's MAX_BLOCK_HEADER_BYTES (668).
        assert!(
            with_blob.len() > without_blob.len(),
            "blob fields must extend header RLP ({} vs {})",
            with_blob.len(),
            without_blob.len()
        );
        assert!(
            with_blob.len() <= crate::circuit_v2::MAX_BLOCK_HEADER_BYTES,
            "encoded header {} exceeds MAX_BLOCK_HEADER_BYTES",
            with_blob.len()
        );

        // Optional Cancun fields are appended (never reordered); hashes differ.
        assert_ne!(without_blob, with_blob);
        assert_ne!(keccak256(&without_blob), keccak256(&with_blob));

        // Explicit RLP of the three Cancun scalars/hash must appear at the tail
        // of the list payload.
        let mut expected_tail = Vec::new();
        RlpU256(&U256::from(0x20000u64)).encode(&mut expected_tail);
        RlpU256(&U256::from(0u64)).encode(&mut expected_tail);
        H256::from_low_u64_be(0xdead)
            .as_bytes()
            .encode(&mut expected_tail);
        assert!(
            with_blob.ends_with(&expected_tail),
            "header must end with blobGasUsed || excessBlobGas || parentBeaconBlockRoot"
        );
    }

    fn sample_receipt(tx_type: Option<u64>, other: OtherFields) -> TransactionReceipt {
        TransactionReceipt {
            transaction_hash: H256::zero(),
            transaction_index: U64::from(0),
            block_hash: Some(H256::zero()),
            block_number: Some(U64::from(1)),
            from: Address::zero(),
            to: Some(Address::zero()),
            cumulative_gas_used: U256::from(21_000u64),
            gas_used: Some(U256::from(21_000u64)),
            contract_address: None,
            logs: vec![],
            status: Some(U64::from(1)),
            root: None,
            logs_bloom: Bloom::default(),
            transaction_type: tx_type.map(U64::from),
            effective_gas_price: None,
            other,
        }
    }

    /// Canyon deposit receipt (`type == 0x7e` + `depositReceiptVersion`):
    /// RLP payload gains `depositNonce` and `depositReceiptVersion` after logs.
    #[test]
    fn test_encode_receipt_deposit_tx_canyon() {
        // 4012991 = 0x3d3bbf (matches ethers-rs deposit-receipt fixture).
        // ethers `U64` deserializes from hex strings only (not JSON integers).
        let other = other_fields_from_json(&[
            ("depositNonce", serde_json::json!("0x3d3bbf")),
            ("depositReceiptVersion", serde_json::json!("0x1")),
        ]);
        let receipt = sample_receipt(Some(DEPOSIT_TX_TYPE), other);
        let encoded = encode_receipt(&receipt).unwrap();

        assert_eq!(encoded[0], DEPOSIT_TX_TYPE as u8, "must be typed 0x7e");

        // Tail must be RLP(depositNonce) || RLP(depositReceiptVersion)
        let mut expected_tail = Vec::new();
        RlpU64(&U64::from(4012991u64)).encode(&mut expected_tail);
        RlpU64(&U64::from(1u64)).encode(&mut expected_tail);
        assert!(
            encoded.ends_with(&expected_tail),
            "Canyon deposit receipt must append depositNonce + depositReceiptVersion; got {}",
            hex::encode(&encoded)
        );

        // Same receipt without Canyon version → no deposit fields in RLP
        // (Regolith / pre-Canyon behaviour; depositNonce alone is ignored).
        let pre_canyon = sample_receipt(
            Some(DEPOSIT_TX_TYPE),
            other_fields_from_json(&[("depositNonce", serde_json::json!("0x3d3bbf"))]),
        );
        let pre_encoded = encode_receipt(&pre_canyon).unwrap();
        assert_eq!(pre_encoded[0], DEPOSIT_TX_TYPE as u8);
        assert!(
            !pre_encoded.ends_with(&expected_tail),
            "pre-Canyon deposit receipt must NOT append depositNonce without version"
        );
        assert!(pre_encoded.len() < encoded.len());
    }

    /// Live OP Stack Ecotone header sample: encoding must include Cancun
    /// blob fields and stay under [`crate::circuit_v2::MAX_BLOCK_HEADER_BYTES`].
    /// Full `keccak == block.hash` is covered by axiom-eth's provider tests
    /// against live Base/OP (`get_block_rlp`); reconstructing a `Block` from a
    /// truncated JSON snapshot is fragile (Bloom / type widths), so we only
    /// assert structural properties here.
    #[test]
    fn test_encode_op_ecotone_header_includes_cancun_fields() {
        let raw = include_str!("../fixtures/op_ecotone_header_sample.json");
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let h256 = |k: &str| {
            let s = v[k].as_str().unwrap().trim_start_matches("0x");
            H256::from_slice(&hex::decode(s).unwrap())
        };
        let u256 = |k: &str| {
            let s = v[k].as_str().unwrap();
            U256::from_str_radix(s.trim_start_matches("0x"), 16).unwrap()
        };
        let addr = {
            let s = v["miner"].as_str().unwrap().trim_start_matches("0x");
            Address::from_slice(&hex::decode(s).unwrap())
        };
        let bloom = {
            let s = v["logsBloom"].as_str().unwrap().trim_start_matches("0x");
            Bloom::from_slice(&hex::decode(s).unwrap())
        };
        let nonce = {
            let s = v["nonce"].as_str().unwrap().trim_start_matches("0x");
            H64::from_slice(&hex::decode(s).unwrap())
        };
        let extra = {
            let s = v["extraData"].as_str().unwrap().trim_start_matches("0x");
            Bytes::from(hex::decode(s).unwrap())
        };
        let block: Block<H256> = Block {
            hash: Some(h256("hash")),
            parent_hash: h256("parentHash"),
            uncles_hash: h256("sha3Uncles"),
            author: Some(addr),
            state_root: h256("stateRoot"),
            transactions_root: h256("transactionsRoot"),
            receipts_root: h256("receiptsRoot"),
            number: Some(U64::from(u256("number").as_u64())),
            gas_used: u256("gasUsed"),
            gas_limit: u256("gasLimit"),
            extra_data: extra,
            logs_bloom: Some(bloom),
            timestamp: u256("timestamp"),
            difficulty: u256("difficulty"),
            total_difficulty: None,
            seal_fields: vec![],
            uncles: vec![],
            transactions: vec![],
            size: None,
            mix_hash: Some(h256("mixHash")),
            nonce: Some(nonce),
            base_fee_per_gas: Some(u256("baseFeePerGas")),
            withdrawals_root: Some(h256("withdrawalsRoot")),
            withdrawals: None,
            blob_gas_used: Some(u256("blobGasUsed")),
            excess_blob_gas: Some(u256("excessBlobGas")),
            parent_beacon_block_root: Some(h256("parentBeaconBlockRoot")),
            other: Default::default(),
        };
        let shanghai_only = {
            let mut b = block.clone();
            b.blob_gas_used = None;
            b.excess_blob_gas = None;
            b.parent_beacon_block_root = None;
            encode_block_header(&b).unwrap()
        };
        let encoded = encode_block_header(&block).unwrap();
        assert!(encoded.len() > shanghai_only.len());
        assert!(encoded.len() <= crate::circuit_v2::MAX_BLOCK_HEADER_BYTES);
        assert!(
            encoded.len() - shanghai_only.len() >= 30,
            "Cancun fields should add blobGasUsed+excessBlobGas+parentBeacon"
        );
    }
}
