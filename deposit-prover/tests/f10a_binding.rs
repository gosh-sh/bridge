//! F10-A integration smoke — instance layout + optional real fixture (may be stale).

use axiom_eth::utils::build_utils::aggregation::CircuitMetadata;
use deposit_prover::{
    audit_circuit_config, circuit_v2::DepositEventCircuitV2, prover::test_circuit_mock,
    synthetic_deposit_proof_input, types::DepositProofInput,
};

#[test]
fn instance_layout_is_eleven_public_inputs() {
    let circuit = DepositEventCircuitV2::new(synthetic_deposit_proof_input(0), &audit_circuit_config());
    assert_eq!(circuit.num_instance(), vec![11]);
}

#[test]
fn real_fixture_satisfies_mock_prover() {
    use std::path::PathBuf;

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/proof_00/input.json");
    let json = std::fs::read_to_string(&path).unwrap();
    let input: DepositProofInput = serde_json::from_str(&json).unwrap();
    test_circuit_mock(input, &audit_circuit_config()).expect("refreshed fixture should satisfy");
}
