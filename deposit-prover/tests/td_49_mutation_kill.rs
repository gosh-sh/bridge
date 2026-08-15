//! TD-49 — PI slot flips kill `test_circuit_mock` (cross-ref TD-01 / TD-21).

use deposit_prover::{
    audit_circuit_config,
    test_circuit_mock_with_pi_corruption,
};

fn sepolia_proof_00() -> deposit_prover::types::DepositProofInput {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/proof_00/input.json");
    let json = std::fs::read_to_string(&path).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn expect_pi_flip_killed(slot: usize, label: &str) {
    use halo2_base::halo2_proofs::halo2curves::bn256::Fr;

    let err = test_circuit_mock_with_pi_corruption(
        sepolia_proof_00(),
        &audit_circuit_config(),
        |instances| {
            let fr = &mut instances[0][slot];
            *fr = *fr + Fr::from(1u64);
        },
    )
    .unwrap_err();
    assert!(
        err.contains("not satisfied") || err.contains("MockProver"),
        "TD-49 circuit kill {label}: {err}"
    );
}

#[test]
fn td_49_circuit_kill_deposit_id_flip() {
    expect_pi_flip_killed(0, "depositId");
}

#[test]
fn td_49_circuit_kill_amount_flip() {
    expect_pi_flip_killed(2, "amount");
}
