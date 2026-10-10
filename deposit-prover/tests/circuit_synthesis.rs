//! In-circuit synthesis at the production shape.
//!
//! Each case is ~2 min at k=18 (same cost as `mock_fixture`), so they are
//! `#[ignore]`d. Run from `deposit-prover/`:
//!
//! ```text
//! cargo test --release --test circuit_synthesis -- --ignored --nocapture
//! ```

use deposit_prover::{
    prover::{test_circuit_mock_pinned, CircuitConfig},
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
