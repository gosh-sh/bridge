//! F10-E — canonical deployed ABI vs relayer `finalizeDeposit` params.

use std::path::PathBuf;

use deposit_relayer_daemon::{
    prover::MockProofGenerator,
    submitter::build_finalize_deposit_params,
    types::{DepositEvent, DepositProofBundle, NUM_PUBLIC_INPUTS},
};
use alloy::primitives::{Address, B256, U256};

fn bundle() -> DepositProofBundle {
    let ev = DepositEvent {
        deposit_id: 1,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(42u64),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x77),
        timestamp: U256::ZERO,
        tx_hash: B256::repeat_byte(0xaa),
        log_index: 0,
        block_number: 1,
        block_hash: B256::repeat_byte(0xcd),
        source_contract: Address::repeat_byte(0x22),
    };
    let pi = MockProofGenerator::derive_public_inputs(&ev, U256::from(0xD499u64));
    DepositProofBundle {
        vk_blob: vec![1].into(),
        public_inputs: pi.to_operand().into(),
        proof: vec![0xAB; 32].into(),
        parsed: pi,
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

#[test]
fn canonical_abi_finalize_deposit_is_two_bytes_args() {
    let abi_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/ursus/USDCBridge.abi.json");
    let funcs = abi_functions(&abi_path);
    let finalize = funcs
        .iter()
        .find(|f| f.get("name").and_then(|n| n.as_str()) == Some("finalizeDeposit"))
        .expect("finalizeDeposit in scripts/ursus/USDCBridge.abi.json");
    let inputs = finalize["inputs"].as_array().unwrap();
    assert_eq!(inputs.len(), 2);
    assert_eq!(inputs[0]["type"], "bytes");
    assert_eq!(inputs[0]["name"], "proof");
    assert_eq!(inputs[1]["type"], "bytes");
    assert_eq!(inputs[1]["name"], "publicInputs");

    let params = build_finalize_deposit_params(&bundle());
    let keys: Vec<_> = params.as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys.len(), 2);
    assert!(keys.iter().any(|k| k == "proof"));
    assert!(keys.iter().any(|k| k == "publicInputs"));

    let pi_len = hex::decode(params["publicInputs"].as_str().unwrap())
        .unwrap()
        .len();
    assert_eq!(pi_len, NUM_PUBLIC_INPUTS * 32);
}

#[test]
fn stale_python_abi_finalize_has_more_than_two_args() {
    let stale_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/an-bridge-prover/python/contracts/USDCBridge.abi.json");
    if !stale_path.exists() {
        return;
    }
    let funcs = abi_functions(&stale_path);
    let finalize = funcs
        .iter()
        .find(|f| f.get("name").and_then(|n| n.as_str()) == Some("finalizeDeposit"));
    if let Some(f) = finalize {
        let n = f["inputs"].as_array().map(|a| a.len()).unwrap_or(0);
        assert_ne!(
            n, 2,
            "QC-OFF-12: stale python ABI must not match deployed 2-arg finalizeDeposit"
        );
    }
}
