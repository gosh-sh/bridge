//! TD-32 — per-L2 header shape matrix (717B envelope, 16–21 fields).

use deposit_prover::{
    BLOCK_HEADER_MAX_FIELD_LENS, HEADER_SHAPE_SAMPLES, MAX_BLOCK_HEADER_BYTES,
    rlp_header_field_count, verify_block_header_rlp,
};
use ethers::types::{Block, H256};
use ethers::utils::keccak256;

fn load_block(raw: &str, name: &str) -> Block<H256> {
    serde_json::from_str(raw).unwrap_or_else(|e| panic!("{name}: deserialize: {e}"))
}

fn assert_all_shapes_reproduce_canonical_hash_within_envelope() {
    for sample in HEADER_SHAPE_SAMPLES {
        let block = load_block(sample.raw_json, sample.name);
        let rlp = verify_block_header_rlp(&block)
            .unwrap_or_else(|e| panic!("{}: verify: {e}", sample.name));
        assert_eq!(
            rlp_header_field_count(&rlp),
            sample.expected_fields,
            "{}: field count",
            sample.name
        );
        assert!(
            rlp.len() <= MAX_BLOCK_HEADER_BYTES,
            "{}: {} bytes > MAX_BLOCK_HEADER_BYTES {}",
            sample.name,
            rlp.len(),
            MAX_BLOCK_HEADER_BYTES
        );
        let expected = block.hash.expect("hash");
        assert_eq!(H256::from(keccak256(&rlp)), expected, "{}: keccak", sample.name);
    }
}

/// Full matrix: each supported shape encodes to canonical `block.hash` within 717B.
#[test]
fn td_32_matrix_all_shapes_reproduce_canonical_hash_within_envelope() {
    assert_all_shapes_reproduce_canonical_hash_within_envelope();
}

/// Cargo filter gate: `cargo test td_32_l2_header_matrix` (CI/doc inventory).
#[test]
fn td_32_l2_header_matrix() {
    assert_all_shapes_reproduce_canonical_hash_within_envelope();
}

#[test]
fn td_32_arbitrum_one_16_fields_wide_gas_limit_qc() {
    let sample = &HEADER_SHAPE_SAMPLES[0];
    assert_eq!(sample.name, "arbitrum_one");
    assert_eq!(sample.expected_fields, 16);
    let block = load_block(sample.raw_json, sample.name);
    let gas_bytes = ((block.gas_limit.bits() + 7) / 8) as usize;
    assert!(
        gas_bytes <= BLOCK_HEADER_MAX_FIELD_LENS[9],
        "BC-D06: gasLimit slot must cover Arbitrum 2^50"
    );
}

#[test]
fn td_32_integer_slots_cover_gas_limit_bc_d06_qc() {
    for (slot, label) in [(8, "number"), (10, "gasUsed"), (11, "timestamp")] {
        assert!(
            BLOCK_HEADER_MAX_FIELD_LENS[slot] >= BLOCK_HEADER_MAX_FIELD_LENS[9],
            "slot {slot} ({label}) narrower than gasLimit — liveness cliff QC"
        );
    }
}

#[test]
fn td_32_truncated_prague_header_rejected_not_silent_pass() {
    let sample = &HEADER_SHAPE_SAMPLES[3];
    let mut block = load_block(sample.raw_json, sample.name);
    assert!(
        block.other.remove("requestsHash").is_some(),
        "fixture must include requestsHash"
    );
    let truncated = deposit_prover::encode_block_header(&block).unwrap();
    assert_eq!(rlp_header_field_count(&truncated), 20);
    assert_ne!(H256::from(keccak256(&truncated)), block.hash.unwrap());
    let err = verify_block_header_rlp(&block).unwrap_err().to_string();
    assert!(
        err.contains("does not reproduce the block hash"),
        "unexpected: {err}"
    );
}

#[test]
fn td_32_forged_extra_field_changes_hash_rejected() {
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
    assert!(err.contains("does not reproduce the block hash"), "{err}");
}

#[test]
fn td_32_max_envelope_equals_717_static_assert() {
    assert_eq!(MAX_BLOCK_HEADER_BYTES, 717);
    assert_eq!(BLOCK_HEADER_MAX_FIELD_LENS.len(), 21);
}
