//! TD-64 — seven-chain deposit header corpus (1:1 with `SUPPORTED_DEPOSIT_CHAIN_IDS`).

use deposit_prover::{
    CHAIN_HEADER_CORPUS, HEADER_SHAPE_SAMPLES, MAX_BLOCK_HEADER_BYTES,
    SUPPORTED_DEPOSIT_CHAIN_IDS, rlp_header_field_count, verify_block_header_rlp,
};
use ethers::types::{Block, H256};
use ethers::utils::keccak256;

fn load_block(raw: &str, name: &str) -> Block<H256> {
    serde_json::from_str(raw).unwrap_or_else(|e| panic!("{name}: deserialize: {e}"))
}

#[test]
fn td_64_each_supported_chain_has_header_fixture() {
    assert_eq!(
        CHAIN_HEADER_CORPUS.len(),
        SUPPORTED_DEPOSIT_CHAIN_IDS.len(),
        "TD-64: corpus rows must match allowlist"
    );
    for (i, entry) in CHAIN_HEADER_CORPUS.iter().enumerate() {
        assert_eq!(
            entry.chain_id,
            SUPPORTED_DEPOSIT_CHAIN_IDS[i],
            "TD-64: corpus order must match SUPPORTED_DEPOSIT_CHAIN_IDS at index {i}"
        );
        assert!(
            !entry.raw_json.is_empty(),
            "TD-64: missing fixture body for {}",
            entry.fixture_name
        );
        let block = load_block(entry.raw_json, entry.fixture_name);
        assert!(
            block.hash.is_some(),
            "TD-64: fixture {} must include hash",
            entry.fixture_name
        );
    }
}

#[test]
fn td_64_verify_block_header_rlp_all_corpus() {
    for entry in CHAIN_HEADER_CORPUS {
        let block = load_block(entry.raw_json, entry.fixture_name);
        let rlp = verify_block_header_rlp(&block)
            .unwrap_or_else(|e| panic!("{}: verify: {e}", entry.fixture_name));
        assert_eq!(
            rlp_header_field_count(&rlp),
            entry.expected_fields,
            "{}: field count (TD-32 cross-ref)",
            entry.fixture_name
        );
        assert!(
            rlp.len() <= MAX_BLOCK_HEADER_BYTES,
            "{}: {} bytes > MAX_BLOCK_HEADER_BYTES {}",
            entry.fixture_name,
            rlp.len(),
            MAX_BLOCK_HEADER_BYTES
        );
        let expected = block.hash.expect("hash");
        assert_eq!(
            H256::from(keccak256(&rlp)),
            expected,
            "{}: keccak",
            entry.fixture_name
        );
    }
}

#[test]
fn td_64_mainnet_shanghai_legacy_shape_not_in_deposit_corpus() {
    let shanghai_only = HEADER_SHAPE_SAMPLES
        .iter()
        .find(|s| s.name == "mainnet_shanghai")
        .expect("mainnet_shanghai shape sample");
    assert!(
        CHAIN_HEADER_CORPUS
            .iter()
            .all(|e| e.raw_json != shanghai_only.raw_json),
        "TD-64: mainnet_shanghai is legacy shape ref, not deposit allowlist"
    );
}
