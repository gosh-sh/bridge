//! TD-44 — relayer `check_binds_to` ignores event `timestamp` and `anWorkchain` (TD-31 / TD-21 pattern).

use alloy::primitives::{Address, B256, U256};
use deposit_relayer_daemon::{
    prover::MockProofGenerator,
    submitter::{AnSubmitter, MockAnSubmitter, SubmitOutcome},
    types::{DepositEvent, DepositProofBundle},
};

const DAPP: U256 = U256::from_limbs([0x0a11_a000, 0, 0, 0]);

fn deposit_event(deposit_id: u64, an_workchain: i8, timestamp: u64) -> DepositEvent {
    DepositEvent {
        deposit_id,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(1_000_000u64),
        an_workchain,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(timestamp),
        tx_hash: B256::repeat_byte(0xaa),
        log_index: 0,
        block_number: 100,
        block_hash: B256::repeat_byte(0xcd),
        source_contract: Address::repeat_byte(0x22),
        source_chain_id: 11_155_111,
    }
}

fn bundle_for(event: &DepositEvent) -> DepositProofBundle {
    let parsed = MockProofGenerator::derive_public_inputs(event, DAPP);
    DepositProofBundle {
        vk_blob: vec![0x56, 0x4b, 0x00, 0x00].into(),
        public_inputs: parsed.to_operand().into(),
        proof: vec![0xde, 0xad].into(),
        parsed,
    }
}

#[tokio::test]
async fn td_44_check_binds_to_ignores_timestamp_and_workchain() {
    let witness = deposit_event(0, 0, 1_700_000_200);
    let bundle = bundle_for(&witness);

    let mut rpc = witness.clone();
    rpc.an_workchain = 127;
    rpc.timestamp = U256::from(9_999_999_999u64);
    assert!(
        bundle.check_binds_to(&rpc).is_ok(),
        "TD-44: an_workchain + timestamp not PI-bound at relayer"
    );
}

#[tokio::test]
async fn td_44_event_timestamp_mismatch_mock_an_finalizes() {
    let proof_event = deposit_event(1, 0, 1_700_000_100);
    let bundle = bundle_for(&proof_event);
    let rpc_event = deposit_event(1, 0, 1_700_999_999);

    let submitter = MockAnSubmitter::with_expected_dapp_id(DAPP);
    assert!(matches!(
        submitter.submit(&rpc_event, &bundle).await.unwrap(),
        SubmitOutcome::Finalized { .. }
    ));
    assert_ne!(rpc_event.timestamp, proof_event.timestamp);
}

#[tokio::test]
async fn td_44_bind_field_list_excludes_timestamp_workchain_promise_commit() {
    let witness = deposit_event(2, 0, 42);
    let bundle = bundle_for(&witness);

    let mut ok = witness.clone();
    ok.an_workchain = -1;
    ok.timestamp = U256::MAX;
    assert!(bundle.check_binds_to(&ok).is_ok(), "free variables survive bind");

    let mut bad_amount = witness.clone();
    bad_amount.amount = U256::from(2u64);
    assert!(
        bundle.check_binds_to(&bad_amount).is_err(),
        "amount is bound — contrast with timestamp/WC"
    );

    // Mirror TD-21: checked fields = depositId, sender, chainId, anAccount, amount,
    // contractAddress, blockHash — NOT an_workchain, timestamp, promiseCommit.
}
