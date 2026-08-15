//! TD-21 — `promiseCommit` public input slot 11 (EthCircuitImpl append).

use deposit_prover::{
    audit_circuit_config,
    circuit_v2::{DEPOSIT_NUM_PUBLIC_INPUTS, DEPOSIT_PUBLIC_INPUT_LAYOUT, PI_CHAIN_ID},
    test_circuit_mock_instances,
    test_circuit_mock_with_pi_corruption,
    types::DepositProofInput,
};
use halo2_base::{
    halo2_proofs::halo2curves::bn256::Fr,
    utils::ScalarField,
};

pub const PI_PROMISE_COMMIT: usize = 11;
pub const PI_BLOCK_HASH_HIGH: usize = 9;
pub const PI_BLOCK_HASH_LOW: usize = 10;
pub const PI_DEPOSIT_ID: usize = 0;

fn sepolia_proof_00() -> DepositProofInput {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/proof_00/input.json");
    let json = std::fs::read_to_string(&path).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn expect_pi_corruption_rejected(slot: usize, label: &str) {
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
        "TD-21 {label}: expected reject, got: {err}"
    );
}

#[test]
fn td_21_layout_slot_eleven_is_promise_commit() {
    assert_eq!(DEPOSIT_NUM_PUBLIC_INPUTS, 12);
    assert_eq!(DEPOSIT_PUBLIC_INPUT_LAYOUT.len(), 12);
    assert_eq!(DEPOSIT_PUBLIC_INPUT_LAYOUT[PI_CHAIN_ID], "chainId");
    assert_eq!(
        DEPOSIT_PUBLIC_INPUT_LAYOUT[PI_PROMISE_COMMIT],
        "promiseCommit"
    );
    assert_eq!(
        *DEPOSIT_PUBLIC_INPUT_LAYOUT.last().unwrap(),
        "promiseCommit"
    );
}

#[test]
fn td_21_proof_00_baseline_slot_eleven_documented() {
    let instances =
        test_circuit_mock_instances(sepolia_proof_00(), &audit_circuit_config(), None)
            .expect("baseline");
    assert_eq!(instances[0].len(), 12);
    let commit = instances[0][PI_PROMISE_COMMIT];
    assert_ne!(
        commit,
        Fr::zero(),
        "TD-21: proof_00 promiseCommit slot 11 should be non-zero (keccak promise)"
    );
    // Stable snapshot for operators (fixture pin).
    let bytes = commit.to_bytes_le();
    assert_ne!(bytes, [0u8; 32]);
}

#[test]
fn td_21_flip_promise_commit_rejected() {
    expect_pi_corruption_rejected(PI_PROMISE_COMMIT, "promiseCommit_flip");
}

#[test]
fn td_21_flip_block_hash_high_control_rejected() {
    expect_pi_corruption_rejected(PI_BLOCK_HASH_HIGH, "blockHashHigh_flip");
}

#[test]
fn td_21_flip_block_hash_low_control_rejected() {
    expect_pi_corruption_rejected(PI_BLOCK_HASH_LOW, "blockHashLow_flip");
}

#[test]
fn td_21_flip_deposit_id_control_rejected() {
    expect_pi_corruption_rejected(PI_DEPOSIT_ID, "depositId_flip");
}

#[test]
fn td_21_baseline_promise_commit_stable_across_mock_runs() {
    let a = test_circuit_mock_instances(sepolia_proof_00(), &audit_circuit_config(), None)
        .unwrap();
    let b = test_circuit_mock_instances(sepolia_proof_00(), &audit_circuit_config(), None)
        .unwrap();
    assert_eq!(
        a[0][PI_PROMISE_COMMIT],
        b[0][PI_PROMISE_COMMIT],
        "promiseCommit must be deterministic for same witness"
    );
}
