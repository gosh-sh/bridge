//! TD-12 — EIP-1559-only: enclosing tx must be type `0x02` wire bytes.

use std::sync::Arc;

use cita_trie::{MemoryDB, PatriciaTrie, Trie};
use deposit_prover::{
    mpt::{align_block_header_roots, transaction_proof_from_wire_bytes},
    production_capacity_config,
    rlp_utils::{typed_tx_chain_id, EIP1559_TX_TYPE},
    synthetic_deposit_proof_input,
    test_circuit_mock,
    types::{DepositProofInput, TransactionProof},
};
use hasher::HasherKeccak;

fn sepolia_proof_00() -> DepositProofInput {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/proof_00/input.json");
    let json = std::fs::read_to_string(&path).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn expect_non_1559_rejected(input: DepositProofInput) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        test_circuit_mock(input, &production_capacity_config())
    }));
    match result {
        Ok(Err(err)) => assert!(
            err.contains("not satisfied")
                || err.contains("MockProver")
                || err.contains("EIP-1559")
                || err.contains("0x02"),
            "TD-12: expected reject, got: {err}"
        ),
        Err(_) => {}, // axiom-eth parser panic — fail-closed
        Ok(Ok(())) => panic!("TD-12: non-1559 witness must not satisfy"),
    }
}

/// Build tx trie proof without `mpt.rs` EIP-1559 preflight (circuit-level test).
fn tx_proof_unchecked(tx_bytes: Vec<u8>) -> TransactionProof {
    use axiom_eth::providers::transaction::get_tx_key_from_index;

    let memdb = Arc::new(MemoryDB::new(true));
    let hasher = Arc::new(HasherKeccak::new());
    let mut trie = PatriciaTrie::new(Arc::clone(&memdb), Arc::clone(&hasher));
    let key = get_tx_key_from_index(0);
    trie.insert(key.clone(), tx_bytes.clone())
        .expect("insert tx leaf");
    let transactions_root: [u8; 32] = trie
        .root()
        .expect("tx root")
        .as_slice()
        .try_into()
        .expect("32-byte root");
    let proof_nodes = trie.get_proof(&key).expect("tx proof");
    TransactionProof {
        tx_bytes,
        proof_nodes,
        transactions_root,
    }
}

fn minimal_eip1559_wire() -> Vec<u8> {
    synthetic_deposit_proof_input(0).tx_proof.tx_bytes
}

fn wire_with_type_prefix(tx_type: u8) -> Vec<u8> {
    let full = minimal_eip1559_wire();
    assert_eq!(full[0], EIP1559_TX_TYPE);
    let mut wire = vec![tx_type];
    wire.extend_from_slice(&full[1..]);
    wire
}

fn legacy_wire_without_type_prefix() -> Vec<u8> {
    let full = minimal_eip1559_wire();
    full[1..].to_vec()
}

fn input_with_tx_wire(tx_wire: Vec<u8>) -> DepositProofInput {
    let mut input = synthetic_deposit_proof_input(1);
    input.tx_proof = tx_proof_unchecked(tx_wire);
    align_block_header_roots(&mut input.receipt_proof, input.tx_proof.transactions_root)
        .expect("align header roots");
    input
}

#[test]
fn td_12_proof_00_eip1559_happy_path() {
    test_circuit_mock(sepolia_proof_00(), &production_capacity_config()).unwrap();
    test_circuit_mock(synthetic_deposit_proof_input(2), &production_capacity_config()).unwrap();
}

#[test]
fn td_12_mpt_rejects_legacy_wire_before_prove() {
    let err = transaction_proof_from_wire_bytes(legacy_wire_without_type_prefix(), 0)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("EIP-1559") || err.contains("0x02"),
        "TD-12 mpt preflight: {err}"
    );
}

#[test]
fn td_12_typed_tx_chain_id_rejects_access_list_type() {
    let err = typed_tx_chain_id(&wire_with_type_prefix(0x01))
        .unwrap_err()
        .to_string();
    assert!(err.contains("EIP-1559") || err.contains("0x02"), "TD-12: {err}");
}

#[test]
fn td_12_legacy_enclosing_tx_rejected_by_circuit() {
    expect_non_1559_rejected(input_with_tx_wire(legacy_wire_without_type_prefix()));
}

#[test]
fn td_12_type_01_access_list_rejected() {
    expect_non_1559_rejected(input_with_tx_wire(wire_with_type_prefix(0x01)));
}

#[test]
fn td_12_type_03_blob_rejected() {
    expect_non_1559_rejected(input_with_tx_wire(wire_with_type_prefix(0x03)));
}

#[test]
fn td_12_type_04_setcode_rejected() {
    expect_non_1559_rejected(input_with_tx_wire(wire_with_type_prefix(0x04)));
}
