//! PoC: non-zero bytes in MPT `key_bytes` padding slots.
//!
//! `axiom-eth` RLC key check uses only the first `key_byte_len` bytes of
//! `key_bytes`; trailing buffer slots are not constrained to zero. This PoC
//! documents that behaviour on the committed Sepolia `proof_00` fixture.

use deposit_prover::{
    audit_circuit_config, test_circuit_mock, test_circuit_mock_with_mpt_mutation,
    types::DepositProofInput, MptWitnessMutation,
};

fn sepolia_proof_00() -> DepositProofInput {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/proof_00/input.json");
    let json = std::fs::read_to_string(&path).unwrap();
    serde_json::from_str(&json).unwrap()
}

#[test]
fn baseline_proof_00_satisfies_mock() {
    let input = sepolia_proof_00();
    assert_eq!(input.event_data.transaction_index, 0);
    test_circuit_mock(input, &audit_circuit_config()).expect("committed fixture");
}

/// Post-assign `key_bytes[1] = 0x42` with `max_key_byte_len = 4` override (legacy layout).
#[test]
fn poc_padding_slot_garbage_accepted_with_max_key_len_four_override() {
    let input = sepolia_proof_00();
    let mutation = MptWitnessMutation {
        max_key_byte_len: Some(4),
        corrupt_key_byte_at: Some((1, 0x42)),
    };
    test_circuit_mock_with_mpt_mutation(input, &audit_circuit_config(), Some(mutation))
        .expect("PoC: unconstrained padding accepted (max_key_byte_len=4)");
}

/// Canonical `max_key_byte_len = 3`.
#[test]
fn poc_padding_slot_garbage_accepted_with_max_key_len_three() {
    let input = sepolia_proof_00();
    let mutation = MptWitnessMutation {
        max_key_byte_len: Some(3),
        corrupt_key_byte_at: Some((1, 0x42)),
    };
    test_circuit_mock_with_mpt_mutation(input, &audit_circuit_config(), Some(mutation))
        .expect("PoC: unconstrained padding accepted (max_key_byte_len=3)");
}

/// Negative control: corrupting the active key prefix must break the circuit.
#[test]
#[should_panic(expected = "circuit was not satisfied")]
fn poc_active_key_byte_corruption_rejected() {
    let input = sepolia_proof_00();
    let mutation = MptWitnessMutation {
        max_key_byte_len: Some(3),
        corrupt_key_byte_at: Some((0, 0x81)),
    };
    let _ = test_circuit_mock_with_mpt_mutation(input, &audit_circuit_config(), Some(mutation));
}
