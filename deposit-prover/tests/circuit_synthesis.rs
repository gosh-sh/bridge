//! In-circuit synthesis at the production shape.
//!
//! Each case is ~2 min at k=18 (same cost as `mock_fixture`), so they are
//! `#[ignore]`d. Run from `deposit-prover/`:
//!
//! ```text
//! cargo test --release --test circuit_synthesis -- --ignored --nocapture
//! ```

use deposit_prover::{
    circuit_v2::{DepositEventCircuitV2, PRODUCTION_MAX_LOG_NUM},
    prover::{mock_circuit_pinned, test_circuit_mock_pinned, CircuitConfig},
    synthetic::{build, SyntheticSpec},
    types::DepositProofInput,
};

fn production() -> CircuitConfig {
    CircuitConfig::production()
}

fn load_proof_00() -> DepositProofInput {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fixtures/deposit_10proofs/proof_00/input.json"
    );
    let json = std::fs::read_to_string(path).expect("proof_00 input");
    serde_json::from_str(&json).expect("proof_00 json")
}

#[test]
#[ignore]
fn proof_00_satisfies_production_shape() {
    test_circuit_mock_pinned(load_proof_00(), &production()).expect("proof_00");
}

#[test]
#[ignore]
fn type2_synthetic_satisfies_production_shape() {
    test_circuit_mock_pinned(build(SyntheticSpec::type2_direct()), &production())
        .expect("type2 synthetic");
}

#[test]
#[ignore]
fn type1_with_access_list_satisfies_production_shape() {
    test_circuit_mock_pinned(
        build(SyntheticSpec::type1_with_access_list()),
        &production(),
    )
    .expect("type1 + access list");
}

#[test]
#[ignore]
fn safe_l2_exec_satisfies_production_shape() {
    test_circuit_mock_pinned(build(SyntheticSpec::safe_l2_exec()), &production())
        .expect("SafeL2 execTransaction");
}

#[test]
#[ignore]
fn erc4337_handle_ops_satisfies_production_shape() {
    test_circuit_mock_pinned(build(SyntheticSpec::erc4337_handle_ops()), &production())
        .expect("4337 handleOps");
}

#[test]
#[ignore]
fn type0_legacy_is_rejected_in_circuit() {
    let err = test_circuit_mock_pinned(build(SyntheticSpec::type0_legacy()), &production())
        .expect_err("type 0 must fail the (t-1)(t-2)==0 gate");
    assert!(
        err.contains("circuit not satisfied") || err.contains("Constraint"),
        "{err}"
    );
}

/// A prover that rewrites the parsed log's `amount` bytes, and claims the
/// same value as the public input, must not satisfy the circuit: the parsed
/// fields have to be the bytes of the log the receipt commits to.
#[test]
#[ignore]
fn forged_log_amount_is_rejected_in_circuit() {
    let mut input = load_proof_00();
    let mut forged = input.event_data.amount;
    forged[16] ^= 0x01;
    input.event_data.amount = forged;
    let mut circuit = DepositEventCircuitV2::new(input, &production());
    circuit.forged_log_amount = Some(forged);
    let err = mock_circuit_pinned(circuit).expect_err("forged amount must not verify");
    assert!(err.contains("circuit not satisfied"), "{err}");
}

#[test]
#[ignore]
fn forged_recipient_is_rejected_in_circuit() {
    let mut input = load_proof_00();
    input.event_data.an_account[0] ^= 1;
    let err = test_circuit_mock_pinned(input, &production())
        .expect_err("the recipient must match the receipt log");
    assert!(err.contains("circuit not satisfied"), "{err}");
}

/// A prover that points `log_index` past the receipt's last log, and writes a
/// well-formed `Deposit` log into that empty slot, must not satisfy the
/// circuit: the selected log has to be one of the logs the receipt commits to.
/// proof_00's receipt has one log, so slot 19 is empty — as is every slot
/// from 1 on; the receipt's own RLC check does not look at an empty slot's
/// bytes.
#[test]
#[ignore]
fn log_slot_past_the_receipts_logs_is_rejected_in_circuit() {
    let mut input = load_proof_00();
    let real_log_index = input.event_data.log_index;
    input.event_data.log_index = PRODUCTION_MAX_LOG_NUM - 1;
    let mut circuit = DepositEventCircuitV2::new(input, &production());
    circuit.forged_log_source = Some(real_log_index);
    let err = mock_circuit_pinned(circuit)
        .expect_err("a log slot past the receipt's logs must not verify");
    assert!(err.contains("circuit not satisfied"), "{err}");
}
