//! F10-A — deposit circuit public-input layout pins (12 PI incl. chainId + promiseCommit).

use axiom_eth::utils::build_utils::aggregation::CircuitMetadata;
use deposit_prover::{
    audit_circuit_config,
    circuit_v2::{
        DepositEventCircuitV2, DEPOSIT_NUM_PUBLIC_INPUTS, DEPOSIT_PUBLIC_INPUT_LAYOUT, PI_CHAIN_ID,
    },
    prover::test_circuit_mock,
    synthetic_deposit_proof_input,
    types::{DepositProofInput, NUM_PUBLIC_INPUTS},
};

#[test]
fn f10a_binding_instance_layout_is_twelve_public_inputs() {
    let circuit =
        DepositEventCircuitV2::new(synthetic_deposit_proof_input(0), &audit_circuit_config());
    assert_eq!(circuit.num_instance(), vec![12]);
    assert_eq!(NUM_PUBLIC_INPUTS, 12);
    assert_eq!(DEPOSIT_NUM_PUBLIC_INPUTS, 12);
}

#[test]
fn f10a_binding_public_input_layout_matches_wire_contract() {
    assert_eq!(DEPOSIT_PUBLIC_INPUT_LAYOUT.len(), 12);
    assert_eq!(DEPOSIT_PUBLIC_INPUT_LAYOUT[PI_CHAIN_ID], "chainId");
    assert_eq!(*DEPOSIT_PUBLIC_INPUT_LAYOUT.last().unwrap(), "promiseCommit");
}

#[test]
fn f10a_binding_real_fixture_satisfies_mock_prover() {
    use std::path::PathBuf;

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/proof_00/input.json");
    let json = std::fs::read_to_string(&path).unwrap();
    let input: DepositProofInput = serde_json::from_str(&json).unwrap();
    test_circuit_mock(input, &audit_circuit_config()).expect("committed fixture should satisfy");
}
