//! TD-22 — tx trie vs receipt trie `transaction_index` coupling (Opus D-21).
//!
//! Receipt and transaction MPT proofs must open at the same `tx_idx` witness.

use deposit_prover::{
    audit_circuit_config,
    mpt::{align_block_header_roots, transaction_proof_from_wire_bytes},
    synthetic_deposit_proof_input,
    test_circuit_mock,
    types::DepositProofInput,
};

fn sepolia_proof_00() -> DepositProofInput {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/proof_00/input.json");
    let json = std::fs::read_to_string(&path).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn expect_mock_reject(input: DepositProofInput, label: &str) {
    let err = test_circuit_mock(input, &audit_circuit_config()).unwrap_err();
    assert!(
        err.contains("not satisfied") || err.contains("MockProver"),
        "TD-22 {label}: expected reject, got: {err}"
    );
}

/// Receipt trie at index 0, tx trie proof path built for index 1 (same leaf bytes).
fn synthetic_receipt_index_zero_tx_trie_index_one() -> DepositProofInput {
    let mut input = synthetic_deposit_proof_input(7);
    let tx_bytes = input.tx_proof.tx_bytes.clone();
    let tx_at_one = transaction_proof_from_wire_bytes(tx_bytes, 1)
        .expect("tx trie proof at index 1");
    input.tx_proof = tx_at_one;
    align_block_header_roots(&mut input.receipt_proof, input.tx_proof.transactions_root)
        .expect("align header to tx root");
    assert_eq!(input.event_data.transaction_index, 0);
    input
}

#[test]
fn td_22_synthetic_aligned_transaction_index_happy() {
    let input = synthetic_deposit_proof_input(3);
    assert_eq!(input.event_data.transaction_index, 0);
    test_circuit_mock(input, &audit_circuit_config())
        .expect("TD-22 synthetic aligned index 0");
}

#[test]
fn td_22_proof_00_aligned_transaction_index_happy() {
    let input = sepolia_proof_00();
    assert_eq!(input.event_data.transaction_index, 137);
    test_circuit_mock(input, &audit_circuit_config())
        .expect("TD-22 proof_00 aligned index 137");
}

#[test]
fn td_22_event_index_mismatch_receipt_trie_reject() {
    let mut input = sepolia_proof_00();
    let aligned = input.event_data.transaction_index;
    input.event_data.transaction_index = aligned.saturating_sub(1);
    expect_mock_reject(input, "proof_00 event index != receipt trie path");
}

#[test]
fn td_22_receipt_index_i_tx_trie_index_j_reject() {
    expect_mock_reject(
        synthetic_receipt_index_zero_tx_trie_index_one(),
        "receipt@0 tx_proof@1 same tx_idx witness",
    );
}

#[test]
fn td_22_event_index_flip_only_reject() {
    let mut input = synthetic_deposit_proof_input(2);
    input.event_data.transaction_index = 1;
    expect_mock_reject(input, "synthetic event index 1 proofs at 0");
}

#[test]
fn td_22_corrupt_tx_trie_leaf_reject() {
    let mut input = sepolia_proof_00();
    if !input.tx_proof.tx_bytes.is_empty() {
        input.tx_proof.tx_bytes[0] ^= 0x01;
    }
    expect_mock_reject(input, "corrupt tx leaf bytes");
}

#[test]
fn td_22_same_tx_idx_witness_couples_both_mpt_paths_documented() {
    let input = sepolia_proof_00();
    let idx = input.event_data.transaction_index;
    let path = deposit_prover::rlp_utils::encode_tx_index(idx);
    assert!(
        !path.is_empty(),
        "receipt/tx trie keys are RLP(transaction_index)"
    );
    test_circuit_mock(input, &audit_circuit_config()).expect("coupled paths satisfy");
}
