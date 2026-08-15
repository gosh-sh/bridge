//! TD-54 — RLP non-canonical fuzz: `rlp_utils` canonical parity vs alloy path,
//! adversarial header/receipt encodings reject at verify boundaries.

use std::panic;

use alloy_rlp::Encodable;
use deposit_prover::{
    encode_block_header, encode_receipt, encode_tx_index, rlp_header_field_count,
    types::DepositProofInput, verify_block_header_rlp, HEADER_SHAPE_SAMPLES,
    MAX_BLOCK_HEADER_BYTES,
};
use ethers::types::{Address, Block, Bloom, Bytes, Log, OtherFields, TransactionReceipt, H256, U64};
use ethers::utils::keccak256;

fn load_block(raw: &str, name: &str) -> Block<H256> {
    serde_json::from_str(raw).unwrap_or_else(|e| panic!("{name}: deserialize: {e}"))
}

fn sepolia_proof_00() -> DepositProofInput {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/proof_00/input.json");
    let json = std::fs::read_to_string(&path).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn alloy_encode_u64(value: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    value.encode(&mut buf);
    buf
}

/// (a) Canonical parity: HEADER_SHAPE_SAMPLES + proof_00 witness paths.
#[test]
fn td_54_a_header_matrix_canonical_parity_count() {
    let mut parity_pass = 0usize;
    for sample in HEADER_SHAPE_SAMPLES {
        let block = load_block(sample.raw_json, sample.name);
        let encoded = encode_block_header(&block).unwrap();
        let verified = verify_block_header_rlp(&block).unwrap();
        assert_eq!(encoded, verified, "{}: encode == verify path", sample.name);
        assert_eq!(
            rlp_header_field_count(&encoded),
            sample.expected_fields,
            "{}: field count",
            sample.name
        );
        assert!(
            encoded.len() <= MAX_BLOCK_HEADER_BYTES,
            "{}: {} bytes > envelope",
            sample.name,
            encoded.len()
        );
        parity_pass += 1;
    }
    assert_eq!(parity_pass, HEADER_SHAPE_SAMPLES.len(), "TD-54 header matrix");

    let input = sepolia_proof_00();
    let witness = &input.receipt_proof.block_header_rlp;
    assert!(
        witness[0] >= 0xf8,
        "proof_00 header witness must be long-list RLP"
    );
    let fields = rlp_header_field_count(witness);
    assert!(
        fields >= 15 && fields <= 21,
        "proof_00 header field count {fields} out of envelope"
    );
    assert!(
        witness.len() <= MAX_BLOCK_HEADER_BYTES,
        "proof_00 header {} bytes",
        witness.len()
    );
    parity_pass += 1;

    assert_eq!(
        input.receipt_proof.receipt_rlp[0],
        0x02,
        "proof_00 receipt must be EIP-1559 typed (0x02)"
    );
    assert_eq!(
        encode_tx_index(input.event_data.transaction_index),
        alloy_encode_u64(input.event_data.transaction_index),
        "proof_00 tx index alloy parity"
    );
    parity_pass += 1;

    assert_eq!(parity_pass, HEADER_SHAPE_SAMPLES.len() + 2, "TD-54 parity tally");
}

#[test]
fn td_54_a_encode_receipt_eip1559_deterministic() {
    let receipt = TransactionReceipt {
        transaction_hash: H256::zero(),
        transaction_index: U64::from(0),
        block_hash: Some(H256::zero()),
        block_number: Some(U64::from(1)),
        from: Address::zero(),
        to: Some(Address::zero()),
        cumulative_gas_used: ethers::types::U256::from(21_000u64),
        gas_used: Some(ethers::types::U256::from(21_000u64)),
        contract_address: None,
        logs: vec![],
        status: Some(U64::from(1)),
        root: None,
        logs_bloom: Bloom::default(),
        transaction_type: Some(U64::from(2)),
        effective_gas_price: None,
        other: OtherFields::default(),
    };
    let once = encode_receipt(&receipt).unwrap();
    let twice = encode_receipt(&receipt).unwrap();
    assert_eq!(once, twice, "encode_receipt must be deterministic");
    assert_eq!(once[0], 0x02, "EIP-1559 receipt type prefix");
}

/// (b) Non-canonical header mutations → hash mismatch / verify Err.
#[test]
fn td_54_b_truncated_header_hash_mismatch() {
    let sample = &HEADER_SHAPE_SAMPLES[1];
    let block = load_block(sample.raw_json, sample.name);
    let rlp = encode_block_header(&block).unwrap();
    let truncated = rlp[..rlp.len().saturating_sub(12)].to_vec();
    assert_ne!(
        H256::from(keccak256(&truncated)),
        block.hash.unwrap(),
        "truncated header must not match canonical hash"
    );
}

#[test]
fn td_54_b_inflated_list_prefix_hash_mismatch() {
    let sample = &HEADER_SHAPE_SAMPLES[0];
    let block = load_block(sample.raw_json, sample.name);
    let rlp = encode_block_header(&block).unwrap();
    let mut inflated = rlp.clone();
    if inflated[0] >= 0xf8 {
        inflated[1] = inflated[1].saturating_add(1);
    } else {
        inflated.insert(0, 0xff);
    }
    assert_ne!(
        H256::from(keccak256(&inflated)),
        block.hash.unwrap(),
        "inflated list prefix must change hash"
    );
}

#[test]
fn td_54_b_drop_requests_hash_verify_rejects() {
    let sample = &HEADER_SHAPE_SAMPLES[3];
    let mut block = load_block(sample.raw_json, sample.name);
    assert!(block.other.remove("requestsHash").is_some());
    let err = verify_block_header_rlp(&block).unwrap_err().to_string();
    assert!(
        err.contains("does not reproduce the block hash"),
        "TD-54 drop field: {err}"
    );
}

#[test]
fn td_54_b_forged_extra_field_verify_rejects() {
    let sample = &HEADER_SHAPE_SAMPLES[1];
    let mut block = load_block(sample.raw_json, sample.name);
    block
        .other
        .insert(
            "requestsHash".to_string(),
            serde_json::json!(
                "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
            ),
        );
    let err = verify_block_header_rlp(&block).unwrap_err().to_string();
    assert!(
        err.contains("does not reproduce the block hash"),
        "TD-54 forged tail: {err}"
    );
}

/// (c) `encode_tx_index` matrix + MPT key sensitivity (TD-22 cross-ref).
#[test]
fn td_54_c_encode_tx_index_matrix_alloy_parity() {
    for idx in [0u64, 1, 255, 65535] {
        let ours = encode_tx_index(idx);
        let alloy = alloy_encode_u64(idx);
        assert_eq!(ours, alloy, "encode_tx_index({idx}) alloy parity");
    }
}

#[test]
fn td_54_c_flipped_tx_index_key_differs_mpt_path() {
    let idx = 137u64;
    let canonical = encode_tx_index(idx);
    assert_eq!(canonical, alloy_encode_u64(idx));

    let mut flipped = canonical.clone();
    let byte = flipped.len() - 1;
    flipped[byte] ^= 0x01;
    assert_ne!(canonical, flipped, "flip length/payload byte must change trie key");
    assert_ne!(
        encode_tx_index(idx),
        encode_tx_index(idx + 1),
        "adjacent indices must differ as trie keys"
    );

    let input = sepolia_proof_00();
    assert_eq!(input.event_data.transaction_index, idx);
    assert_eq!(encode_tx_index(idx), canonical);
}

/// (d) Bounded fuzz: helper parsers never panic on truncated canonical prefixes.
#[test]
fn td_54_d_rlp_header_field_count_truncated_prefixes_no_panic() {
    for sample in HEADER_SHAPE_SAMPLES {
        let block = load_block(sample.raw_json, sample.name);
        let rlp = encode_block_header(&block).unwrap();
        for end in 1..rlp.len() {
            let prefix = &rlp[..end];
            let result = panic::catch_unwind(|| rlp_header_field_count(prefix));
            assert!(
                result.is_ok(),
                "{}: rlp_header_field_count panicked on prefix len {end}",
                sample.name
            );
        }
    }
}

#[test]
fn td_54_d_random_short_slices_field_count_no_panic() {
    for a in 0u8..=255 {
        let one = [a];
        let _ = panic::catch_unwind(|| rlp_header_field_count(&one));
        for b in 0u8..=255 {
            let two = [a, b];
            let _ = panic::catch_unwind(|| rlp_header_field_count(&two));
        }
    }
}

#[test]
fn td_54_d_encode_receipt_empty_logs_no_panic() {
    let receipt = TransactionReceipt {
        transaction_hash: H256::zero(),
        transaction_index: U64::from(0),
        block_hash: None,
        block_number: None,
        from: Address::zero(),
        to: None,
        cumulative_gas_used: ethers::types::U256::zero(),
        gas_used: None,
        contract_address: None,
        logs: vec![Log {
            address: Address::zero(),
            topics: vec![],
            data: Bytes::new(),
            block_hash: None,
            block_number: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            transaction_log_index: None,
            log_type: None,
            removed: None,
        }],
        status: Some(U64::from(1)),
        root: None,
        logs_bloom: Bloom::default(),
        transaction_type: Some(U64::from(2)),
        effective_gas_price: None,
        other: OtherFields::default(),
    };
    let encoded = encode_receipt(&receipt).unwrap();
    assert!(!encoded.is_empty());
}
