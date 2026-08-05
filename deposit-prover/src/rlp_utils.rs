use alloy_rlp::{Encodable, RlpEncodable};
use anyhow::{anyhow, Result};
use ethers::types::{Block, Log, TransactionReceipt, H256, U256, U64};
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

/// Read the Prague / OP Isthmus `requestsHash` (EIP-7685) out of the RPC
/// response's untyped `other` map.
///
/// ethers-core 2.0.14 predates EIP-7685, so `requestsHash` never lands in a
/// typed `Block` field — dropping it silently is what makes
/// [`encode_block_header`] produce a non-canonical header on every Prague chain
/// (Base, Mantle, World Chain, OP Mainnet, Sepolia as of 2026-07).
fn requests_hash_from_other<T>(block: &Block<T>) -> Result<Option<H256>> {
    let Some(raw) = block.other.get("requestsHash") else {
        return Ok(None);
    };
    let hex = raw
        .as_str()
        .ok_or_else(|| anyhow!("requestsHash is not a string: {raw}"))?;
    let bytes = hex::decode(hex.trim_start_matches("0x"))
        .map_err(|e| anyhow!("requestsHash is not hex: {e}"))?;
    if bytes.len() != 32 {
        return Err(anyhow!("requestsHash is {} bytes, expected 32", bytes.len()));
    }
    Ok(Some(H256::from_slice(&bytes)))
}

/// RLP encode a block header
///
/// Block header structure (through Prague / OP Stack Isthmus):
/// [parentHash, ommersHash, beneficiary, stateRoot, transactionsRoot,
/// receiptsRoot, logsBloom, difficulty, number, gasLimit, gasUsed, timestamp,
/// extraData, mixHash, nonce, baseFeePerGas?, withdrawalsRoot?, blobGasUsed?,
/// excessBlobGas?, parentBeaconBlockRoot?, requestsHash?]
///
/// Optional fields are appended only when present so `keccak(header) ==
/// block.hash` on Arbitrum (no `withdrawalsRoot`), L1 Cancun, OP Stack Ecotone
/// and Prague / Isthmus alike. Encoding matches axiom-eth
/// `providers/block.rs::get_block_rlp` (ethers `rlp` crate) plus the EIP-7685
/// tail ethers-core 2.0.14 doesn't model.
///
/// Callers that fetched the block from an RPC should assert
/// `keccak256(result) == block.hash` — see [`verify_block_header_rlp`].
pub fn encode_block_header<T>(block: &Block<T>) -> Result<Vec<u8>> {
    use rlp::RlpStream;

    let base_fee = block.base_fee_per_gas;
    let withdrawals_root = block.withdrawals_root;
    let blob_gas_used = block.blob_gas_used;
    let excess_blob_gas = block.excess_blob_gas;
    let parent_beacon_block_root = block.parent_beacon_block_root;
    let requests_hash = requests_hash_from_other(block)?;

    let mut rlp_len = 15;
    for opt in [
        base_fee.is_some(),
        withdrawals_root.is_some(),
        blob_gas_used.is_some(),
        excess_blob_gas.is_some(),
        parent_beacon_block_root.is_some(),
        requests_hash.is_some(),
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
    if let Some(requests_hash) = requests_hash {
        rlp.append(&requests_hash);
    }
    Ok(rlp.out().into())
}

/// EIP-1559 (`0x02`) is the only transaction type the circuit can bind, since
/// `chain_id` must be a top-level RLP field (legacy txs hide it inside `v`).
pub const EIP1559_TX_TYPE: u8 = 0x02;

/// Read `chain_id` out of an EIP-1559 transaction's wire encoding.
///
/// This is the same field the circuit extracts (typed-tx RLP field 0); decoding
/// it out-of-circuit lets callers reject a witness/flag mismatch before paying
/// for a proof.
pub fn typed_tx_chain_id(tx_bytes: &[u8]) -> Result<u64> {
    let (&tx_type, rest) = tx_bytes
        .split_first()
        .ok_or_else(|| anyhow!("transaction bytes are empty"))?;
    if tx_type != EIP1559_TX_TYPE {
        return Err(anyhow!(
            "transaction type {tx_type:#04x} is not EIP-1559 (0x02); the deposit circuit \
             cannot bind chain_id for this type"
        ));
    }
    let prefix = *rest
        .first()
        .ok_or_else(|| anyhow!("typed transaction has no RLP payload"))?;
    if prefix < 0xc0 {
        return Err(anyhow!("typed transaction payload is not an RLP list"));
    }
    // Every slice below goes through `get` — this function is reachable from
    // `DepositProofInput::require_chain_id`, i.e. from loading any witness off
    // disk, so a truncated or hand-edited `tx_bytes` must surface as an error
    // rather than unwind the process (BC-D09).
    let body = if prefix <= 0xf7 {
        rest.get(1..)
            .ok_or_else(|| anyhow!("truncated typed transaction"))?
    } else {
        let len_len = (prefix - 0xf7) as usize;
        rest.get(1 + len_len..)
            .ok_or_else(|| anyhow!("truncated RLP list header"))?
    };
    let first = *body
        .first()
        .ok_or_else(|| anyhow!("transaction RLP list is empty"))?;
    let chain_id_bytes = match first {
        0x00..=0x7f => &body[..1],
        0x80..=0xb7 => {
            let n = (first - 0x80) as usize;
            body.get(1..1 + n)
                .ok_or_else(|| anyhow!("truncated chain_id field"))?
        }
        _ => return Err(anyhow!("chain_id field is not a short RLP string")),
    };
    if chain_id_bytes.len() > 8 {
        return Err(anyhow!("chain_id is {} bytes, too wide", chain_id_bytes.len()));
    }
    Ok(chain_id_bytes.iter().fold(0u64, |acc, b| (acc << 8) | *b as u64))
}

/// Encode `block`'s header and assert it reproduces the hash the RPC reported.
///
/// This is the guard that turns "we silently dropped a header field the local
/// ethers version doesn't model" into a loud failure. Without it a truncated
/// header still hashes to *something*, the circuit happily commits that value
/// as the `blockHash` public input, and every downstream canonical-block
/// cross-check becomes unsatisfiable.
pub fn verify_block_header_rlp<T>(block: &Block<T>) -> Result<Vec<u8>> {
    let rlp = encode_block_header(block)?;
    let expected = block
        .hash
        .ok_or_else(|| anyhow!("block has no hash (pending block?)"))?;
    let actual = H256::from(ethers::utils::keccak256(&rlp));
    if actual != expected {
        return Err(anyhow!(
            "encoded block header does not reproduce the block hash: got {actual:#x}, \
             RPC reported {expected:#x} (block {:?}, {} RLP bytes). The header shape is \
             unsupported — a consensus upgrade most likely added a field.",
            block.number,
            rlp.len(),
        ));
    }
    Ok(rlp)
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

    /// Verbatim `eth_getBlockByNumber` responses (minus the tx/withdrawal
    /// lists) for one block per header shape the supported chains emit.
    const HEADER_SAMPLES: [(&str, &str, usize); 3] = [
        // Prague / EIP-7685: 21 fields incl. `requestsHash`. Base, Mantle,
        // World Chain and OP Mainnet have the same shape.
        (
            "sepolia_prague",
            include_str!("../fixtures/headers/sepolia_prague.json"),
            21,
        ),
        // OP Stack Isthmus: Prague shape with a 16-byte Holocene `extraData`.
        (
            "op_isthmus",
            include_str!("../fixtures/headers/op_isthmus.json"),
            21,
        ),
        // Arbitrum One: no `withdrawalsRoot`, no Cancun tail, and a 2^50
        // `gasLimit` (7 bytes) — the widest numeric field of any supported chain.
        (
            "arbitrum_one",
            include_str!("../fixtures/headers/arbitrum_one.json"),
            16,
        ),
    ];

    fn rlp_field_count(header: &[u8]) -> usize {
        let prefix = header[0];
        let mut i = if prefix >= 0xf8 {
            1 + (prefix - 0xf7) as usize
        } else {
            1
        };
        let mut fields = 0;
        while i < header.len() {
            let b = header[i];
            i += match b {
                0x00..=0x7f => 1,
                0x80..=0xb7 => 1 + (b - 0x80) as usize,
                _ => {
                    let n = (b - 0xb7) as usize;
                    let mut len = 0usize;
                    for byte in &header[i + 1..i + 1 + n] {
                        len = (len << 8) | *byte as usize;
                    }
                    1 + n + len
                },
            };
            fields += 1;
        }
        fields
    }

    /// The load-bearing test for header encoding: every supported header shape
    /// must RLP-encode back to the hash the chain reported. Without this, a
    /// field the local `ethers` version doesn't model (EIP-7685 `requestsHash`
    /// is exactly that) is dropped silently, the circuit commits the hash of a
    /// truncated header as its `blockHash` public input, and no downstream
    /// consumer can tie the proof to a canonical block.
    #[test]
    fn header_samples_reproduce_canonical_block_hash() {
        for (name, raw, expected_fields) in HEADER_SAMPLES {
            let block: Block<H256> =
                serde_json::from_str(raw).unwrap_or_else(|e| panic!("{name}: deserialize: {e}"));
            let rlp = verify_block_header_rlp(&block)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(
                rlp_field_count(&rlp),
                expected_fields,
                "{name}: unexpected header field count"
            );
            assert!(
                rlp.len() <= crate::circuit_v2::MAX_BLOCK_HEADER_BYTES,
                "{name}: encoded header is {} bytes, over MAX_BLOCK_HEADER_BYTES ({})",
                rlp.len(),
                crate::circuit_v2::MAX_BLOCK_HEADER_BYTES
            );
        }
    }

    /// Dropping the Prague tail must fail loudly rather than yield a header
    /// that hashes to a plausible-looking non-canonical value.
    #[test]
    fn header_without_requests_hash_is_rejected() {
        let (_, raw, _) = HEADER_SAMPLES[0];
        let mut block: Block<H256> = serde_json::from_str(raw).unwrap();
        assert!(
            block.other.remove("requestsHash").is_some(),
            "sample must carry requestsHash"
        );
        let truncated = encode_block_header(&block).unwrap();
        assert_eq!(rlp_field_count(&truncated), 20);
        assert_ne!(H256::from(keccak256(&truncated)), block.hash.unwrap());
        let err = verify_block_header_rlp(&block).unwrap_err().to_string();
        assert!(
            err.contains("does not reproduce the block hash"),
            "unexpected error: {err}"
        );
    }

    /// The out-of-circuit `chain_id` decode must agree with what the circuit
    /// binds — it is what lets the CLI reject a witness/flag mismatch early.
    #[test]
    fn typed_tx_chain_id_matches_the_sepolia_fixture() {
        #[derive(serde::Deserialize)]
        struct Witness {
            tx_proof: TxProof,
        }
        #[derive(serde::Deserialize)]
        struct TxProof {
            tx_bytes: Vec<u8>,
        }
        let raw = include_str!("../fixtures/chain_binding_sepolia_dep0/input.json");
        let witness: Witness = serde_json::from_str(raw).unwrap();
        assert_eq!(
            typed_tx_chain_id(&witness.tx_proof.tx_bytes).unwrap(),
            crate::supported_chains::CHAIN_ID_SEPOLIA
        );
    }

    #[test]
    fn typed_tx_chain_id_rejects_non_1559() {
        let err = typed_tx_chain_id(&[0x00, 0xc0]).unwrap_err().to_string();
        assert!(err.contains("not EIP-1559"), "unexpected error: {err}");
        assert!(typed_tx_chain_id(&[]).is_err());
    }

    /// BC-D09: a truncated typed-tx must error, never panic. Both inputs are the
    /// audit's fuzz reproducers — `0xff` claims an 8-byte list-length header that
    /// isn't there, and `0x85` claims a 5-byte chain_id string that isn't there.
    #[test]
    fn typed_tx_chain_id_errors_on_truncated_input() {
        for bytes in [
            vec![0x02, 0xff],
            vec![0x02, 0xc0, 0x85],
            vec![0x02, 0xf8],
            vec![0x02, 0xc0],
        ] {
            assert!(
                typed_tx_chain_id(&bytes).is_err(),
                "expected Err for {bytes:02x?}"
            );
        }
    }

    /// Exhaustive over every 2- and 3-byte typed-tx prefix: the decoder must
    /// terminate with Ok or Err for all of them, and never unwind.
    #[test]
    fn typed_tx_chain_id_never_panics_on_short_input() {
        for a in 0u8..=255 {
            let _ = typed_tx_chain_id(&[0x02, a]);
            for b in 0u8..=255 {
                let _ = typed_tx_chain_id(&[0x02, a, b]);
            }
        }
    }

    /// Arbitrum's `gasLimit` is 2^50; the circuit's per-field cap must cover it.
    #[test]
    fn arbitrum_gas_limit_fits_the_circuit_field_cap() {
        let (_, raw, _) = HEADER_SAMPLES[2];
        let block: Block<H256> = serde_json::from_str(raw).unwrap();
        let gas_limit_bytes = (block.gas_limit.bits() + 7) / 8;
        assert!(gas_limit_bytes > 4, "sample no longer exercises the wide case");
        assert!(
            gas_limit_bytes <= crate::circuit_v2::BLOCK_HEADER_MAX_FIELD_LENS[9],
            "gasLimit needs {gas_limit_bytes} bytes, field cap is {}",
            crate::circuit_v2::BLOCK_HEADER_MAX_FIELD_LENS[9]
        );
    }

    /// BC-D06: every integer header slot must be able to hold the widest value
    /// its own `gasLimit` permits, not merely the widest value observed. A slot
    /// that is too narrow makes the RLP decode fail, so deposits in such a block
    /// become permanently unprovable — and unrefundable, since `withdraw()` was
    /// retired in Phase 4.3.
    ///
    /// Empirical context for the sizing (sampled 2026-08-03, 32 blocks spread
    /// across Arbitrum One's history): max `gasUsed` 2 719 399 (3 bytes) against
    /// a `gasLimit` of 2^50. The 8-byte slot leaves the protocol no way to
    /// overflow it at all, which is the property worth having.
    #[test]
    fn integer_header_slots_cover_their_own_gas_limit() {
        use crate::circuit_v2::BLOCK_HEADER_MAX_FIELD_LENS;
        for (slot, name) in [(8, "number"), (10, "gasUsed"), (11, "timestamp")] {
            assert!(
                BLOCK_HEADER_MAX_FIELD_LENS[slot] >= BLOCK_HEADER_MAX_FIELD_LENS[9],
                "slot {slot} ({name}) caps at {} bytes while gasLimit (slot 9) allows {} — \
                 a chain may emit a value the circuit cannot decode",
                BLOCK_HEADER_MAX_FIELD_LENS[slot],
                BLOCK_HEADER_MAX_FIELD_LENS[9],
            );
        }
    }

    /// Every sample header must still decode inside the per-field caps, so a
    /// slot can never be narrowed below live data by accident.
    #[test]
    fn sample_headers_fit_every_integer_slot() {
        use crate::circuit_v2::BLOCK_HEADER_MAX_FIELD_LENS;
        for (name, raw, _) in HEADER_SAMPLES {
            let block: Block<H256> = serde_json::from_str(raw).unwrap();
            for (slot, width, field) in [
                (8, block.number.unwrap().as_u64().into(), "number"),
                (9, block.gas_limit, "gasLimit"),
                (10, block.gas_used, "gasUsed"),
                (11, block.timestamp, "timestamp"),
            ] {
                let need = ((width.bits() + 7) / 8).max(1);
                assert!(
                    need <= BLOCK_HEADER_MAX_FIELD_LENS[slot],
                    "{name}: {field} needs {need} bytes, slot {slot} caps at {}",
                    BLOCK_HEADER_MAX_FIELD_LENS[slot]
                );
            }
        }
    }
}
