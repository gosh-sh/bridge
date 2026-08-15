//! TD-38 — stale finalize ABI / PI operand reorder safety (QC-OFF-12 / DEP-ABI-STALE).
//!
//! Cross-ref: `f10_e_abi.rs`, TD-02 layout, TD-01 bind mutations.

use std::path::PathBuf;

use alloy::primitives::{Address, B256, Bytes, U256};
use deposit_relayer_daemon::{
    prover::MockProofGenerator,
    submitter::{
        build_finalize_deposit_params, decode_finalize_deposit, encode_finalize_deposit,
        AnSubmitter, MockAnSubmitter, SubmitOutcome,
    },
    types::{
        DepositEvent, DepositProofBundle, DepositPublicInputs, NUM_PUBLIC_INPUTS,
    },
};

fn sample_event() -> DepositEvent {
    DepositEvent {
        deposit_id: 7,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(5_000_000u64),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x55),
        timestamp: U256::ZERO,
        tx_hash: B256::repeat_byte(0xaa),
        log_index: 0,
        block_number: 42,
        block_hash: B256::repeat_byte(0xcd),
        source_contract: Address::repeat_byte(0x22),
        source_chain_id: 11_155_111,
    }
}

fn good_pi(event: &DepositEvent) -> DepositPublicInputs {
    MockProofGenerator::derive_public_inputs(event, U256::from(0xD499u64))
}

fn bundle_from_pi(pi: DepositPublicInputs) -> DepositProofBundle {
    DepositProofBundle {
        vk_blob: Bytes::from(vec![0x56, 0x4b, 0x00]),
        public_inputs: Bytes::from(pi.to_operand()),
        proof: Bytes::from(vec![0xAB; 32]),
        parsed: pi,
    }
}

fn swap_pi_slots(operand: &mut [u8], slot_a: usize, slot_b: usize) {
    let a = slot_a * 32;
    let b = slot_b * 32;
    for i in 0..32 {
        operand.swap(a + i, b + i);
    }
}

fn abi_functions(path: &PathBuf) -> Vec<serde_json::Value> {
    let abi: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read ABI")).unwrap();
    if let Some(arr) = abi.as_array() {
        return arr.clone();
    }
    abi.get("functions")
        .and_then(|f| f.as_array())
        .cloned()
        .unwrap_or_default()
}

fn abi_finalize_inputs(path: &PathBuf) -> Option<Vec<serde_json::Value>> {
    if !path.exists() {
        return None;
    }
    abi_functions(path)
        .iter()
        .find(|f| f.get("name").and_then(|n| n.as_str()) == Some("finalizeDeposit"))
        .and_then(|f| f.get("inputs"))
        .and_then(|i| i.as_array())
        .cloned()
}

/// (a) Canonical `build_finalize_deposit_params` — 2 keys, `NUM_PUBLIC_INPUTS×32` operand.
#[test]
fn td_38_a_build_finalize_params_canonical_shape() {
    let event = sample_event();
    let bundle = bundle_from_pi(good_pi(&event));
    let params = build_finalize_deposit_params(&bundle);
    let obj = params.as_object().unwrap();
    assert_eq!(obj.len(), 2);
    let pi_len = hex::decode(params["publicInputs"].as_str().unwrap())
        .unwrap()
        .len();
    assert_eq!(pi_len, NUM_PUBLIC_INPUTS * 32);
}

/// (a) Swap operand slots 0↔2 → decoded PI drifts → `check_binds_to` fails.
#[tokio::test]
async fn td_38_a_swap_pi_slots_operand_bind_fails() {
    let event = sample_event();
    let pi = good_pi(&event);
    let mut operand = pi.to_operand();
    swap_pi_slots(&mut operand, 0, 2);

    let drifted = DepositPublicInputs::from_operand(&operand).unwrap();
    assert_ne!(drifted.deposit_id, pi.deposit_id);
    assert_ne!(drifted.amount, pi.amount);

    let bundle = bundle_from_pi(drifted);
    let err = bundle.check_binds_to(&event).unwrap_err().to_string();
    assert!(
        err.contains("depositId") || err.contains("amount"),
        "TD-38: swapped slots should break bind: {err}"
    );

    let submitter = MockAnSubmitter::accepting();
    assert!(
        submitter.submit(&event, &bundle).await.is_err(),
        "TD-38: drifted operand must not reach mock mint"
    );
}

/// Operand bytes swapped but `parsed` left canonical — relayer does not auto-redecode (QC).
#[test]
fn td_38_operand_parsed_drift_undetected_without_redecode() {
    let event = sample_event();
    let pi = good_pi(&event);
    let mut bundle = bundle_from_pi(pi);
    let mut operand = bundle.public_inputs.to_vec();
    swap_pi_slots(&mut operand, 0, 2);
    bundle.public_inputs = Bytes::from(operand);

    let decoded = DepositPublicInputs::from_operand(&bundle.public_inputs).unwrap();
    assert_ne!(decoded.deposit_id, bundle.parsed.deposit_id);
    bundle.check_binds_to(&event).unwrap();
}

/// (b) `encode_finalize_deposit` ↔ `decode_finalize_deposit` roundtrip; permute scalars ≠ `parsed`.
#[test]
fn td_38_b_encode_decode_roundtrip_permute_scalars_mismatch_parsed() {
    let event = sample_event();
    let bundle = bundle_from_pi(good_pi(&event));
    let body = encode_finalize_deposit(&bundle);
    let (mut scalars, proof) = decode_finalize_deposit(&body).unwrap();
    assert_eq!(proof, bundle.proof.to_vec());

    scalars[0] = scalars[2];
    scalars[2] = bundle.parsed.deposit_id;

    assert_ne!(scalars[0], bundle.parsed.deposit_id);
    assert_ne!(scalars[2], bundle.parsed.amount);
}

/// (c) Deployed ABI: `finalizeDeposit(proof, publicInputs)` — not legacy multi-arg `confirmDeposit`.
#[test]
fn td_38_c_finalize_abi_two_args_matches_canonical_not_confirm_shape() {
    let ursus = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/ursus/USDCBridge.abi.json");
    let finalize = abi_finalize_inputs(&ursus).expect("ursus finalizeDeposit");
    assert_eq!(finalize.len(), 2);
    assert_eq!(finalize[0]["name"], "proof");
    assert_eq!(finalize[1]["name"], "publicInputs");

    let funcs = abi_functions(&ursus);
    let confirm = funcs
        .iter()
        .find(|f| f.get("name").and_then(|n| n.as_str()) == Some("confirmDeposit"))
        .expect("confirmDeposit present");
    let confirm_n = confirm["inputs"].as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(confirm_n, 6, "legacy confirmDeposit is not the finalize ABI");
    assert_ne!(finalize.len(), confirm_n, "finalizeDeposit must not drift to confirmDeposit arity");

    let stale = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/an-bridge-prover/python/contracts/USDCBridge.abi.json");
    if let Some(stale_inputs) = abi_finalize_inputs(&stale) {
        assert_eq!(stale_inputs.len(), 2, "python ABI finalizeDeposit must match ursus");
    }
}

/// (d) Control — canonical bundle passes bind + mock finalize.
#[tokio::test]
async fn td_38_d_control_canonical_bundle_mock_finalized() {
    let event = sample_event();
    let bundle = bundle_from_pi(good_pi(&event));
    bundle.check_binds_to(&event).unwrap();

    let submitter = MockAnSubmitter::accepting();
    assert!(matches!(
        submitter.submit(&event, &bundle).await.unwrap(),
        SubmitOutcome::Finalized { .. }
    ));
}
