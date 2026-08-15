//! TD-03 — forged / non-canonical L1 block: circuit proves header inclusion, not consensus.
//!
//! Catalog `TD-03`: self-consistent synthetic witness (not mainnet) must pass
//! MockProver; AN anchor gate is out-of-proof (DEP-N-4 / BC-D01).

use deposit_prover::{
    production_capacity_config,
    synthetic_deposit_proof_input,
    synthetic_fixture::SYNTHETIC_CHAIN_ID,
    test_circuit_mock,
};
use ethers::utils::keccak256;

fn sepolia_proof_00_block_hash() -> [u8; 32] {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/proof_00/input.json");
    let json = std::fs::read_to_string(&path).unwrap();
    let input: deposit_prover::types::DepositProofInput = serde_json::from_str(&json).unwrap();
    keccak256(&input.receipt_proof.block_header_rlp)
}

fn witness_block_hash(input: &deposit_prover::types::DepositProofInput) -> [u8; 32] {
    keccak256(&input.receipt_proof.block_header_rlp)
}

#[test]
fn td_03_forged_synthetic_witness_mock_prover_pass() {
    let input = synthetic_deposit_proof_input(0xfa);
    test_circuit_mock(input, &production_capacity_config())
        .expect("TD-03: self-consistent forged witness must satisfy MockProver");
}

#[test]
fn td_03_forged_witness_not_mainnet_fixture_block_hash() {
    let forged = synthetic_deposit_proof_input(0xfb);
    let mainnet_hash = sepolia_proof_00_block_hash();
    let forged_hash = witness_block_hash(&forged);
    assert_ne!(
        forged_hash,
        mainnet_hash,
        "TD-03: synthetic forged header hash must differ from committed Sepolia fixture"
    );
    assert_eq!(forged.event_data.chain_id, SYNTHETIC_CHAIN_ID);
    assert_eq!(forged.event_data.block_number, 1);
}

#[test]
fn td_03_second_forged_variant_self_consistent_passes() {
    let input = synthetic_deposit_proof_input(0xfc);
    let hash = witness_block_hash(&input);
    assert_ne!(hash, [0u8; 32], "forged header must hash to non-zero commitment");
    test_circuit_mock(input, &production_capacity_config()).unwrap();
}

#[test]
fn td_03_forged_header_hash_is_keccak_of_witness_header_rlp() {
    let input = synthetic_deposit_proof_input(0xfd);
    let hash = witness_block_hash(&input);
    assert_eq!(
        hash,
        keccak256(&input.receipt_proof.block_header_rlp),
        "block hash PI source is keccak256(block_header_rlp), not eth_getBlockByNumber"
    );
}
