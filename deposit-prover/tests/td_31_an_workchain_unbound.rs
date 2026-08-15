//! TD-31 — `anWorkchain` in event data but not in PI; circuit does not bind WC.

use deposit_prover::{
    audit_circuit_config,
    circuit_v2::DEPOSIT_PUBLIC_INPUT_LAYOUT,
    production_capacity_config,
    synthetic_fixture::{
        synthetic_deposit_proof_input_with_workchain, SYNTHETIC_CHAIN_ID,
    },
    test_circuit_mock_instances,
    types::DepositProofInput,
};

pub const PI_DAPP_HIGH: usize = 5;
pub const PI_DAPP_LOW: usize = 6;
pub const PI_AN_ACCOUNT_HIGH: usize = 7;
pub const PI_AN_ACCOUNT_LOW: usize = 8;

const TEST_DAPP_ID: [u8; 32] = {
    let mut id = [0u8; 32];
    id[28] = 0x0a;
    id[29] = 0x11;
    id[30] = 0xa0;
    id[31] = 0x00;
    id
};

fn sepolia_proof_00() -> DepositProofInput {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/proof_00/input.json");
    let json = std::fs::read_to_string(&path).unwrap();
    serde_json::from_str(&json).unwrap()
}

#[test]
fn td_31_public_input_layout_has_no_workchain_slot() {
    assert!(!DEPOSIT_PUBLIC_INPUT_LAYOUT.iter().any(|label| {
        label.eq_ignore_ascii_case("anWorkchain") || label.eq_ignore_ascii_case("workchain")
    }));
    assert_eq!(DEPOSIT_PUBLIC_INPUT_LAYOUT[PI_DAPP_HIGH], "dappIdHigh");
    assert_eq!(DEPOSIT_PUBLIC_INPUT_LAYOUT[PI_DAPP_LOW], "dappIdLow");
}

#[test]
fn td_31_sepolia_proof_00_fixture_pi_slots_five_six_are_dapp_id() {
    let instances =
        test_circuit_mock_instances(sepolia_proof_00(), &audit_circuit_config(), None)
            .expect("proof_00 baseline");
    // proof_00 JSON carries default dapp_id = 0; no workchain slot in layout.
    assert_eq!(instances[0][PI_DAPP_HIGH], halo2_base::halo2_proofs::halo2curves::bn256::Fr::zero());
    assert_eq!(instances[0][PI_DAPP_LOW], halo2_base::halo2_proofs::halo2curves::bn256::Fr::zero());
}

#[test]
fn td_31_mutated_workchain_on_sepolia_chain_synthetic_pi_unchanged() {
    let baseline = synthetic_deposit_proof_input_with_workchain(31, 0);
    let baseline_pi =
        test_circuit_mock_instances(baseline.clone(), &production_capacity_config(), None)
            .expect("baseline synthetic");

    for wc in [127i8, -1i8, 42i8] {
        let mutated = synthetic_deposit_proof_input_with_workchain(31, wc);
        assert_eq!(mutated.event_data.an_workchain, wc);
        assert_eq!(mutated.event_data.chain_id, SYNTHETIC_CHAIN_ID);

        let instances =
            test_circuit_mock_instances(mutated, &production_capacity_config(), None)
                .unwrap_or_else(|e| panic!("TD-31 wc={wc}: unexpected reject: {e}"));

        assert_eq!(
            instances[0][PI_DAPP_HIGH],
            baseline_pi[0][PI_DAPP_HIGH],
            "wc={wc}: dappIdHigh must not depend on event workchain"
        );
        assert_eq!(
            instances[0][PI_DAPP_LOW],
            baseline_pi[0][PI_DAPP_LOW],
            "wc={wc}: dappIdLow must not depend on event workchain"
        );
        assert_eq!(
            instances[0][PI_AN_ACCOUNT_HIGH],
            baseline_pi[0][PI_AN_ACCOUNT_HIGH],
            "wc={wc}: anAccountHigh unchanged when only WC word mutates"
        );
        assert_eq!(
            instances[0][PI_AN_ACCOUNT_LOW],
            baseline_pi[0][PI_AN_ACCOUNT_LOW],
            "wc={wc}: anAccountLow unchanged when only WC word mutates"
        );
    }
}

#[test]
fn td_31_fixed_dapp_id_pi_slots_five_six_stable_across_workchain() {
    let baseline =
        synthetic_deposit_proof_input_with_workchain(31, 0);
    let mut baseline = baseline;
    baseline.dapp_id = TEST_DAPP_ID;
    let baseline_pi =
        test_circuit_mock_instances(baseline.clone(), &production_capacity_config(), None)
            .expect("baseline synthetic");

    let mut wc127 = synthetic_deposit_proof_input_with_workchain(31, 127);
    wc127.dapp_id = TEST_DAPP_ID;
    let wc127_pi =
        test_circuit_mock_instances(wc127, &production_capacity_config(), None)
            .expect("wc=127 synthetic");

    assert_eq!(wc127_pi[0][PI_DAPP_HIGH], baseline_pi[0][PI_DAPP_HIGH]);
    assert_eq!(wc127_pi[0][PI_DAPP_LOW], baseline_pi[0][PI_DAPP_LOW]);
    assert_eq!(wc127_pi[0][PI_AN_ACCOUNT_HIGH], baseline_pi[0][PI_AN_ACCOUNT_HIGH]);
    assert_eq!(wc127_pi[0][PI_AN_ACCOUNT_LOW], baseline_pi[0][PI_AN_ACCOUNT_LOW]);
}

#[test]
fn td_31_synthetic_workchain_minus_one_mock_passes_not_bc() {
    let input = synthetic_deposit_proof_input_with_workchain(31, -1);
    assert_eq!(input.event_data.an_workchain, -1);
    assert_eq!(input.event_data.chain_id, SYNTHETIC_CHAIN_ID);
    test_circuit_mock_instances(input, &production_capacity_config(), None)
        .expect("TD-31: WC=-1 witness must satisfy circuit (QC — not bound to PI)");
}
