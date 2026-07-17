//! F10-E / QC-OFF-06 — live submit path maps AN revert to Rejected (not AlreadyFinalized).

use std::sync::Arc;

use acki_nacki_interface::{MockAckiNacki, TransactionStatus};
use alloy::primitives::{Address, B256, U256};
use deposit_relayer_daemon::{
    prover::MockProofGenerator,
    submitter::{AnInterfaceSubmitter, AnSubmitConfig, AnSubmitter, SubmitOutcome},
    types::DepositEvent,
};

fn event(id: u64) -> DepositEvent {
    DepositEvent {
        deposit_id: id,
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
    }
}

fn bundle(ev: &DepositEvent) -> deposit_relayer_daemon::types::DepositProofBundle {
    let pi = MockProofGenerator::derive_public_inputs(ev, U256::from(0xD499u64));
    deposit_relayer_daemon::types::DepositProofBundle {
        vk_blob: vec![1, 2, 3].into(),
        public_inputs: pi.to_operand().into(),
        proof: vec![0xAB; 48].into(),
        parsed: pi,
    }
}

#[tokio::test]
async fn reverted_finalize_maps_to_rejected_not_already_finalized() {
    let client = Arc::new(MockAckiNacki::new());
    client.set_finalize_receipt_status(TransactionStatus::Reverted);
    let dapp = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    let cfg = AnSubmitConfig {
        from: format!(
            "{dapp}::ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
        ),
        token_bridge: format!("{dapp}::{dapp}"),
        confirm_timeout_secs: 2,
    };
    let sub = AnInterfaceSubmitter::new(client, cfg);
    let ev = event(1);
    let b = bundle(&ev);

    match sub.submit(&ev, &b).await.unwrap() {
        SubmitOutcome::Rejected { reason } => {
            assert!(
                reason.contains("Reverted") || reason.contains("status"),
                "unexpected reason: {reason}"
            );
        },
        other => panic!("QC-OFF-06: expected Rejected on duplicate/nullifier revert, got {other:?}"),
    }
}
