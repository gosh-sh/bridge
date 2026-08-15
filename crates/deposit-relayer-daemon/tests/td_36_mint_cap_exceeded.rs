//! TD-36 — AN `setMintCap` / exit 229 vs L1 uncapped aggregate; relayer HOL on cap hit.
//!
//! Cross-ref: TD-33, TD-34 (stuck L1), TD-65 (reject loop), QC-AN-J1.

use std::{sync::Arc, time::Duration};

use acki_nacki_interface::{MockAckiNacki, TransactionStatus};
use alloy::primitives::{Address, B256, U256};
use deposit_relayer_daemon::{
    prover::{MockProofGenerator, ProofGenerator},
    relayer::{Relayer, RelayerConfig, TickOutcome},
    source::InMemoryDepositSource,
    submitter::{
        AnInterfaceSubmitter, AnSubmitConfig, AnSubmitter, MockAnSubmitter, SubmitOutcome,
    },
    types::DepositEvent,
};
use tempfile::tempdir;

const DAPP_HEX: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

fn deposit(id: u64, amount: U256) -> DepositEvent {
    DepositEvent {
        deposit_id: id,
        sender: Address::repeat_byte(0x11),
        amount,
        an_workchain: 0,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(1_700_000_000u64 + id),
        tx_hash: B256::repeat_byte(0xaa + id as u8),
        log_index: 0,
        block_number: 100 + id,
        block_hash: B256::repeat_byte(0xcd),
        source_contract: Address::repeat_byte(0x22),
        source_chain_id: 11155111,
    }
}

fn an_submit_config() -> AnSubmitConfig {
    AnSubmitConfig {
        from: format!(
            "{DAPP_HEX}::ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
        ),
        token_bridge: format!("{DAPP_HEX}::{DAPP_HEX}"),
        confirm_timeout_secs: 2,
    }
}

#[tokio::test]
async fn td_36_mock_mint_cap_exceeded_maps_to_rejected_with_setmintcap_hint() {
    let cap = U256::from(50_000_000u64); // 50 USDC @ 6 decimals
    let submitter = MockAnSubmitter::with_mint_cap(cap);
    let ev = deposit(0, U256::from(60_000_000u64));
    let bundle = MockProofGenerator::new().generate(&ev).await.unwrap();

    match submitter.submit(&ev, &bundle).await.unwrap() {
        SubmitOutcome::Rejected { reason } => {
            assert!(reason.contains("ERR_MINT_CAP_EXCEEDED"), "{reason}");
            assert!(reason.contains("229"), "{reason}");
            assert!(reason.contains("setMintCap"), "{reason}");
        },
        other => panic!("TD-36: expected Rejected on cap, got {other:?}"),
    }
    assert_eq!(submitter.finalized_count(), 0);
}

#[tokio::test]
async fn td_36_mock_under_cap_finalizes_then_cap_blocks_second() {
    let cap = U256::from(100_000_000u64);
    let submitter = MockAnSubmitter::with_mint_cap(cap);
    let ev0 = deposit(0, U256::from(60_000_000u64));
    let ev1 = deposit(1, U256::from(60_000_000u64));
    let b0 = MockProofGenerator::new().generate(&ev0).await.unwrap();
    let b1 = MockProofGenerator::new().generate(&ev1).await.unwrap();

    assert!(matches!(
        submitter.submit(&ev0, &b0).await.unwrap(),
        SubmitOutcome::Finalized { .. }
    ));
    match submitter.submit(&ev1, &b1).await.unwrap() {
        SubmitOutcome::Rejected { reason } => {
            assert!(reason.contains("ERR_MINT_CAP_EXCEEDED"));
        },
        other => panic!("second mint should hit cap: {other:?}"),
    }
    assert_eq!(submitter.finalized_count(), 1);
}

#[tokio::test]
async fn td_36_interface_exit_229_maps_to_rejected_with_hint() {
    let client = Arc::new(MockAckiNacki::new());
    client.set_finalize_receipt_status(TransactionStatus::Reverted);
    client.set_finalize_receipt_exit_code(229);
    let sub = AnInterfaceSubmitter::new(client, an_submit_config());
    let ev = deposit(2, U256::from(1_000_000u64));
    let bundle = MockProofGenerator::new().generate(&ev).await.unwrap();

    match sub.submit(&ev, &bundle).await.unwrap() {
        SubmitOutcome::Rejected { reason } => {
            assert!(reason.contains("ERR_MINT_CAP_EXCEEDED"));
            assert!(reason.contains("setMintCap"));
        },
        other => panic!("TD-36 interface: expected Rejected, got {other:?}"),
    }
}

#[tokio::test]
async fn td_36_relayer_tick_mint_cap_hol_same_deposit_id() {
    let dir = tempdir().unwrap();
    let cap = U256::from(10_000_000u64);
    let submitter = Arc::new(MockAnSubmitter::with_mint_cap(cap));
    let source = Arc::new(InMemoryDepositSource::new());
    let amount = U256::from(20_000_000u64);
    source.insert(deposit(0, amount));
    let prover = Arc::new(MockProofGenerator::new());

    let mut cfg = RelayerConfig::new(dir.path().join("state.json"));
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    let mut relayer =
        Relayer::new(cfg, source, prover, submitter.clone()).unwrap();

    let o1 = relayer.tick().await.unwrap();
    assert!(
        matches!(o1, TickOutcome::AnRejected { deposit_id: 0, .. }),
        "TD-36: cap hit {o1:?}"
    );
    assert_eq!(relayer.state().last_processed_deposit_id, None);
    assert_eq!(relayer.state().attempts_since_progress, 1);

    let o2 = relayer.tick().await.unwrap();
    assert!(matches!(o2, TickOutcome::AnRejected { deposit_id: 0, .. }));
    assert_eq!(relayer.state().attempts_since_progress, 2);
    assert_eq!(submitter.finalized_count(), 0, "no AN mint on cap reject");
}
