//! TD-01 — per-slot mutation on `DepositPublicInputs` breaks roundtrip / bind checks.
//!
//! Catalog `TD-01`: flip one field in the 12-PI operand → relayer bind must fail.

use alloy::primitives::{Address, B256, U256};
use deposit_relayer_daemon::types::{DepositEvent, DepositProofBundle, DepositPublicInputs};

fn sample_event() -> DepositEvent {
    DepositEvent {
        deposit_id: 3,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(5u64),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x55),
        timestamp: U256::ZERO,
        tx_hash: B256::ZERO,
        log_index: 0,
        block_number: 1,
        block_hash: B256::ZERO,
        source_contract: Address::ZERO,
        source_chain_id: 11_155_111,
    }
}

fn sample_pi() -> DepositPublicInputs {
    DepositPublicInputs {
        deposit_id: U256::from(3u64),
        sender: U256::from_be_bytes::<32>({
            let mut b = [0u8; 32];
            b[12..].copy_from_slice(Address::repeat_byte(0x11).as_slice());
            b
        }),
        amount: U256::from(5u64),
        contract_address: U256::ZERO,
        chain_id: U256::from(11_155_111u64),
        dapp_id_high: U256::ZERO,
        dapp_id_low: U256::ZERO,
        an_account_high: U256::from_be_slice(&[0x55u8; 16]),
        an_account_low: U256::from_be_slice(&[0x55u8; 16]),
        block_hash_high: U256::ZERO,
        block_hash_low: U256::ZERO,
        promise_commit: U256::ZERO,
    }
}

fn bundle_with_pi(pi: DepositPublicInputs) -> DepositProofBundle {
    DepositProofBundle {
        vk_blob: alloy::primitives::Bytes::from(vec![1u8; 4]),
        public_inputs: alloy::primitives::Bytes::from(pi.to_operand()),
        proof: alloy::primitives::Bytes::from(vec![2u8; 8]),
        parsed: pi,
    }
}

#[test]
fn td01_mutate_each_bound_field_rejected_by_check_binds_to() {
    let event = sample_event();
    let base = sample_pi();

    let mut cases: Vec<(DepositPublicInputs, &'static str)> = Vec::new();

    let mut wrong_id = base;
    wrong_id.deposit_id = U256::from(99u64);
    cases.push((wrong_id, "deposit_id"));

    let mut wrong_amount = base;
    wrong_amount.amount = U256::from(999u64);
    cases.push((wrong_amount, "amount"));

    let mut wrong_chain = base;
    wrong_chain.chain_id = U256::from(1u64);
    cases.push((wrong_chain, "chain_id"));

    let mut wrong_acct = base;
    wrong_acct.an_account_low = U256::from(0x66u64);
    cases.push((wrong_acct, "an_account"));

    for (pi, label) in cases {
        let bundle = bundle_with_pi(pi);
        let err = bundle.check_binds_to(&event).unwrap_err().to_string();
        assert!(
            err.contains("!=") || err.contains("mismatch"),
            "TD-01: mutate {label} should fail bind: {err}"
        );
    }
}

#[test]
fn td01_matching_pi_passes_check_binds_to() {
    let event = sample_event();
    let bundle = bundle_with_pi(sample_pi());
    bundle.check_binds_to(&event).unwrap();
}
