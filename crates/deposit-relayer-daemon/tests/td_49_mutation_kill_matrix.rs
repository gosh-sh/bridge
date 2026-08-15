//! TD-49 / DEP-MUTATION-TESTING — full `check_binds_to` kill matrix + documented survivors.

use alloy::primitives::{Address, B256, U256};
use deposit_relayer_daemon::types::{DepositEvent, DepositProofBundle, DepositPublicInputs};

const CHAIN: u64 = 11_155_111;

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
        block_hash: B256::repeat_byte(0x77),
        source_contract: Address::repeat_byte(0x22),
        source_chain_id: CHAIN,
    }
}

fn matching_pi(event: &DepositEvent) -> DepositPublicInputs {
    DepositPublicInputs {
        deposit_id: U256::from(event.deposit_id),
        sender: U256::from_be_bytes::<32>({
            let mut b = [0u8; 32];
            b[12..].copy_from_slice(event.sender.as_slice());
            b
        }),
        amount: event.amount,
        contract_address: U256::from_be_bytes::<32>({
            let mut b = [0u8; 32];
            b[12..].copy_from_slice(event.source_contract.as_slice());
            b
        }),
        chain_id: U256::from(event.source_chain_id),
        dapp_id_high: U256::from(0xaaaa_bbbbu64),
        dapp_id_low: U256::from(0xcccc_ddddu64),
        an_account_high: U256::from_be_slice(&event.an_account.as_slice()[0..16]),
        an_account_low: U256::from_be_slice(&event.an_account.as_slice()[16..32]),
        block_hash_high: U256::from_be_slice(&event.block_hash.as_slice()[0..16]),
        block_hash_low: U256::from_be_slice(&event.block_hash.as_slice()[16..32]),
        promise_commit: U256::from(0x99u64),
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

fn assert_bind_fails(bundle: &DepositProofBundle, event: &DepositEvent, label: &str) {
    let err = bundle.check_binds_to(event).unwrap_err().to_string();
    assert!(
        err.contains("!=") || err.contains("mismatch"),
        "TD-49 kill {label}: {err}"
    );
}

/// Seven bound groups in `check_binds_to` — each flip must Err.
#[test]
fn td_49_kill_matrix_each_bound_field_flip_rejected() {
    let event = sample_event();
    let base = matching_pi(&event);

    let mut wrong_id = base;
    wrong_id.deposit_id = U256::from(99u64);
    assert_bind_fails(&bundle_with_pi(wrong_id), &event, "deposit_id");

    let mut wrong_sender = base;
    wrong_sender.sender = U256::from(0xdead_beef_u64);
    assert_bind_fails(&bundle_with_pi(wrong_sender), &event, "sender");

    let mut wrong_amount = base;
    wrong_amount.amount = U256::from(999u64);
    assert_bind_fails(&bundle_with_pi(wrong_amount), &event, "amount");

    let mut wrong_contract = base;
    wrong_contract.contract_address = U256::from(0xbeef_u64);
    assert_bind_fails(&bundle_with_pi(wrong_contract), &event, "contractAddress");

    let mut wrong_chain = base;
    wrong_chain.chain_id = U256::from(1u64);
    assert_bind_fails(&bundle_with_pi(wrong_chain), &event, "chainId");

    let mut wrong_acct = base;
    wrong_acct.an_account_low = U256::from(0x66u64);
    assert_bind_fails(&bundle_with_pi(wrong_acct), &event, "anAccount");

    let mut wrong_block = base;
    wrong_block.block_hash_low = U256::from(0x88u64);
    assert_bind_fails(&bundle_with_pi(wrong_block), &event, "blockHash");
}

#[test]
fn td_49_control_matching_pi_passes_check_binds_to() {
    let event = sample_event();
    let bundle = bundle_with_pi(matching_pi(&event));
    bundle.check_binds_to(&event).unwrap();
}

/// Documented survivors — config/AN-bound, not event-bound (TD-05 / TD-21).
#[test]
fn td_49_survivor_dapp_id_flip_not_event_bound() {
    let event = sample_event();
    let mut pi = matching_pi(&event);
    pi.dapp_id_high = U256::from(0x1111_1111u64);
    pi.dapp_id_low = U256::from(0x2222_2222u64);
    let bundle = bundle_with_pi(pi);
    bundle.check_binds_to(&event).expect("TD-49 QC: dappId not event-bound");
}

#[test]
fn td_49_survivor_promise_commit_flip_not_event_bound() {
    let event = sample_event();
    let mut pi = matching_pi(&event);
    pi.promise_commit = U256::from(0xfeed_faceu64);
    let bundle = bundle_with_pi(pi);
    bundle
        .check_binds_to(&event)
        .expect("TD-49 QC: promiseCommit not event-bound at relayer");
}

/// TD-31 cross-ref — `an_workchain` not in `check_binds_to`.
#[test]
fn td_49_survivor_an_workchain_not_in_bind() {
    let mut event_wc = sample_event();
    event_wc.an_workchain = 127;
    let bundle = bundle_with_pi(matching_pi(&sample_event()));
    bundle
        .check_binds_to(&event_wc)
        .expect("TD-49 QC: anWorkchain not PI-bound at relayer");
}

/// Cross-ref TD-01 — TD-49 superset covers TD-01 partial matrix.
#[test]
fn td_49_cross_ref_td_01_subset_still_killed() {
    let event = sample_event();
    let base = matching_pi(&event);
    let mut wrong_chain = base;
    wrong_chain.chain_id = U256::from(1u64);
    assert_bind_fails(&bundle_with_pi(wrong_chain), &event, "chainId_td01");
}
