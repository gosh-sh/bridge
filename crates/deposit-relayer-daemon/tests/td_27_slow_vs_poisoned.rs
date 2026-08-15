//! TD-27 — slow (`NotYetAvailable`) vs poisoned (`ProofFailed`) share `record_failure`.
//!
//! Opus D-16: `attempts_since_progress` looks identical; operator must read tick outcome / logs.

use std::{path::PathBuf, sync::Arc, time::Duration};

use alloy::primitives::{Address, B256, U256};
use deposit_relayer_daemon::{
    prover::MockProofGenerator,
    relayer::{Relayer, RelayerConfig, TickOutcome},
    source::InMemoryDepositSource,
    submitter::MockAnSubmitter,
    types::DepositEvent,
};
use tempfile::tempdir;

fn deposit(id: u64) -> DepositEvent {
    DepositEvent {
        deposit_id: id,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(id * 1000 + 1),
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

fn relayer_cfg(state_path: PathBuf, skip_after_attempts: Option<u32>) -> RelayerConfig {
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    cfg.skip_after_attempts = skip_after_attempts;
    cfg
}

/// (A) Slow deposit — `NotYetAvailable` bumps attempts, no park before threshold.
#[tokio::test]
async fn td_27_not_yet_available_accumulates_attempts_no_park_before_threshold() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    let mut relayer = Relayer::new(
        relayer_cfg(dir.path().join("state.json"), Some(3)),
        source,
        Arc::new(MockProofGenerator::new()),
        Arc::new(MockAnSubmitter::accepting()),
    )
    .unwrap();

    for expected in [1, 2] {
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::NotYetAvailable { deposit_id: 0 }
        ));
        assert_eq!(relayer.state().attempts_since_progress, expected);
    }
    assert!(relayer.state().parked_deposit_ids.is_empty());
    assert_eq!(relayer.state().last_processed_deposit_id, None);
}

/// (B) Poisoned deposit — `ProofFailed` parks after `skip_after_attempts`.
#[tokio::test]
async fn td_27_proof_failed_poison_parks_after_threshold() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    let mut relayer = Relayer::new(
        relayer_cfg(dir.path().join("state.json"), Some(2)),
        source,
        Arc::new(MockProofGenerator::failing_on(0)),
        Arc::new(MockAnSubmitter::accepting()),
    )
    .unwrap();

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::ProofFailed { deposit_id: 0, .. }
    ));
    assert_eq!(relayer.state().attempts_since_progress, 1);

    match relayer.tick().await.unwrap() {
        TickOutcome::Skipped { deposit_id, .. } => assert_eq!(deposit_id, 0),
        other => panic!("expected Skipped, got {other:?}"),
    }
    assert_eq!(relayer.state().parked_deposit_ids, vec![0]);
    assert_eq!(relayer.state().next_target(0), 1);
}

/// (C) Slow deposit appears on tick k == threshold — finalizes, **not** parked (Opus D-16).
#[tokio::test]
async fn td_27_slow_appears_at_threshold_tick_not_in_parked() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    let mut relayer = Relayer::new(
        relayer_cfg(dir.path().join("state.json"), Some(3)),
        source.clone(),
        Arc::new(MockProofGenerator::new()),
        Arc::new(MockAnSubmitter::accepting()),
    )
    .unwrap();

    for _ in 0..2 {
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::NotYetAvailable { deposit_id: 0 }
        ));
    }
    assert_eq!(relayer.state().attempts_since_progress, 2);

    source.insert(deposit(0));
    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert!(relayer.state().parked_deposit_ids.is_empty());
    assert_eq!(relayer.state().last_processed_deposit_id, Some(0));
    assert_eq!(relayer.state().attempts_since_progress, 0);
}

/// (D) Control — poisoned id ends in `parked_deposit_ids`.
#[tokio::test]
async fn td_27_poisoned_id_in_parked_control() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    let submitter = Arc::new(MockAnSubmitter::with_verifier(Arc::new(|_| false)));
    let mut relayer = Relayer::new(
        relayer_cfg(dir.path().join("state.json"), Some(2)),
        source,
        Arc::new(MockProofGenerator::new()),
        submitter,
    )
    .unwrap();

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::AnRejected { deposit_id: 0, .. }
    ));
    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Skipped { deposit_id: 0, .. }
    ));
    assert_eq!(relayer.state().parked_deposit_ids, vec![0]);
}

/// Same `attempts_since_progress` before diverging outcome — QC ops indistinguishable in state.
#[tokio::test]
async fn td_27_attempts_counter_indistinguishable_before_outcome() {
    let slow_source = Arc::new(InMemoryDepositSource::new());
    let mut slow = Relayer::new(
        relayer_cfg(tempdir().unwrap().path().join("slow.json"), Some(3)),
        slow_source,
        Arc::new(MockProofGenerator::new()),
        Arc::new(MockAnSubmitter::accepting()),
    )
    .unwrap();

    let poison_source = Arc::new(InMemoryDepositSource::new());
    poison_source.insert(deposit(0));
    let mut poison = Relayer::new(
        relayer_cfg(tempdir().unwrap().path().join("poison.json"), Some(3)),
        poison_source,
        Arc::new(MockProofGenerator::failing_on(0)),
        Arc::new(MockAnSubmitter::accepting()),
    )
    .unwrap();

    for _ in 0..2 {
        assert!(matches!(
            slow.tick().await.unwrap(),
            TickOutcome::NotYetAvailable { deposit_id: 0 }
        ));
        assert!(matches!(
            poison.tick().await.unwrap(),
            TickOutcome::ProofFailed { deposit_id: 0, .. }
        ));
    }
    assert_eq!(slow.state().attempts_since_progress, 2);
    assert_eq!(poison.state().attempts_since_progress, 2);
    assert_eq!(
        slow.state().attempts_since_progress,
        poison.state().attempts_since_progress,
        "TD-27 QC: state file cannot distinguish slow vs poison"
    );
}

/// Slow deposit never confirming past threshold → same park path as poison (QC ops risk).
#[tokio::test]
async fn td_27_slow_past_threshold_without_event_parked_via_record_failure() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    let mut relayer = Relayer::new(
        relayer_cfg(dir.path().join("state.json"), Some(2)),
        source,
        Arc::new(MockProofGenerator::new()),
        Arc::new(MockAnSubmitter::accepting()),
    )
    .unwrap();

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::NotYetAvailable { deposit_id: 0 }
    ));
    match relayer.tick().await.unwrap() {
        TickOutcome::Skipped { deposit_id, .. } => assert_eq!(deposit_id, 0),
        other => panic!("expected Skipped for slow-not-confirmed, got {other:?}"),
    }
    assert_eq!(relayer.state().parked_deposit_ids, vec![0]);
}
