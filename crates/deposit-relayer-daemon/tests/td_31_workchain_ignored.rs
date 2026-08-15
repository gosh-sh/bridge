//! TD-31 — `anWorkchain` not in PI / relayer bind; mock AN credits workchain 0 (QC-AN-J4).

use alloy::primitives::{Address, B256, U256};
use deposit_relayer_daemon::{
    prover::MockProofGenerator,
    submitter::{AnSubmitter, MockAnSubmitter, SubmitOutcome},
    types::{DepositEvent, DepositProofBundle},
};

const DAPP: U256 = U256::from_limbs([0x0a11_a000, 0, 0, 0]);

fn deposit_event(deposit_id: u64, an_workchain: i8) -> DepositEvent {
    DepositEvent {
        deposit_id,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(1_000_000u64),
        an_workchain,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(1_700_000_200u64),
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
async fn td_31_check_binds_to_ignores_an_workchain() {
    let wc0 = deposit_event(0, 0);
    let bundle = bundle_for(&wc0);

    let mut event_wc127 = wc0.clone();
    event_wc127.an_workchain = 127;
    assert!(bundle.check_binds_to(&event_wc127).is_ok(), "TD-31: WC not PI-bound");

    let mut event_wc_neg = wc0.clone();
    event_wc_neg.an_workchain = -1;
    assert!(bundle.check_binds_to(&event_wc_neg).is_ok());
}

#[tokio::test]
async fn td_31_event_wc127_proof_wc0_finalizes_mock_credits_wc_zero() {
    let proof_event = deposit_event(1, 0);
    let bundle = bundle_for(&proof_event);
    let rpc_event = deposit_event(1, 127);

    let submitter = MockAnSubmitter::with_expected_dapp_id(DAPP);
    assert!(matches!(
        submitter.submit(&rpc_event, &bundle).await.unwrap(),
        SubmitOutcome::Finalized { .. }
    ));
    assert_eq!(submitter.last_mint_workchain(), 0, "QC-AN-J4: AN path is WC 0");
    assert_eq!(
        submitter.last_mint_account(),
        Some(bundle.parsed.an_account()),
        "mint account from PI, not event WC"
    );
    assert_ne!(rpc_event.an_workchain, 0);
}

#[tokio::test]
async fn td_31_event_wc_neg_one_same_pi_finalizes_qc_not_bc() {
    let proof_event = deposit_event(2, 0);
    let bundle = bundle_for(&proof_event);
    let rpc_event = deposit_event(2, -1);

    let submitter = MockAnSubmitter::accepting();
    assert!(matches!(
        submitter.submit(&rpc_event, &bundle).await.unwrap(),
        SubmitOutcome::Finalized { .. }
    ));
    assert_eq!(submitter.last_mint_workchain(), 0);
    assert_eq!(submitter.last_mint_account(), Some(bundle.parsed.an_account()));
}

#[tokio::test]
async fn td_31_pi_an_account_independent_of_event_workchain_field() {
    let proof_event = deposit_event(3, 0);
    let bundle = bundle_for(&proof_event);
    let event_wc127 = deposit_event(3, 127);

    assert!(
        bundle.check_binds_to(&event_wc127).is_ok(),
        "relayer does not compare an_workchain"
    );
    assert_eq!(
        bundle.parsed.an_account(),
        U256::from_be_slice(event_wc127.an_account.as_slice()),
        "PI anAccount matches event account; WC mismatch is orthogonal"
    );
}
