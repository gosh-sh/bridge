//! F10-C — competing relayers / submitters share one AN nullifier mirror.
//!
//! Safety: only one finalize per depositId; the second relayer must observe
//! `AlreadyFinalized` without double-counting in the mock nullifier set.

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
        timestamp: U256::from(1_700_000_000u64),
        tx_hash: B256::repeat_byte(0xaa),
        log_index: 0,
        block_number: 100 + id,
        block_hash: B256::repeat_byte(0xcd),
        source_contract: Address::repeat_byte(0x22),
    }
}

type R = Relayer<InMemoryDepositSource, MockProofGenerator, MockAnSubmitter>;

fn make_relayer(
    source: Arc<InMemoryDepositSource>,
    prover: Arc<MockProofGenerator>,
    submitter: Arc<MockAnSubmitter>,
    state_path: PathBuf,
) -> R {
    let cfg = RelayerConfig {
        state_path,
        start_deposit_id: 0,
        poll_interval: Duration::from_millis(0),
        max_attempts_warn: 16,
    };
    Relayer::new(cfg, source, prover, submitter).unwrap()
}

#[tokio::test]
async fn second_relayer_advances_on_already_finalized_nullifier() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    let prover = Arc::new(MockProofGenerator::new());
    let submitter = Arc::new(MockAnSubmitter::accepting());

    let mut relayer_a = make_relayer(
        source.clone(),
        prover.clone(),
        submitter.clone(),
        dir.path().join("state_a.json"),
    );
    let mut relayer_b = make_relayer(
        source,
        prover,
        submitter.clone(),
        dir.path().join("state_b.json"),
    );

    assert!(matches!(
        relayer_a.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert_eq!(submitter.finalized_count(), 1);

    // Relayer B has independent state but shares the AN nullifier mirror.
    assert!(matches!(
        relayer_b.tick().await.unwrap(),
        TickOutcome::AlreadyFinalized { deposit_id: 0 }
    ));
    assert_eq!(
        submitter.finalized_count(),
        1,
        "second relayer must not mint twice"
    );
    assert_eq!(relayer_b.state().last_processed_deposit_id, Some(0));
}

#[tokio::test]
async fn concurrent_ticks_on_same_deposit_only_one_finalize() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    let prover = Arc::new(MockProofGenerator::new());
    let submitter = Arc::new(MockAnSubmitter::accepting());

    let mut relayer_a = make_relayer(
        source.clone(),
        prover.clone(),
        submitter.clone(),
        dir.path().join("state_a.json"),
    );
    let mut relayer_b = make_relayer(
        source,
        prover,
        submitter.clone(),
        dir.path().join("state_b.json"),
    );

    let out_a = relayer_a.tick().await.unwrap();
    let out_b = relayer_b.tick().await.unwrap();

    let finalized = [&out_a, &out_b]
        .iter()
        .filter(|o| matches!(o, TickOutcome::Finalized { .. }))
        .count();
    let already = [&out_a, &out_b]
        .iter()
        .filter(|o| matches!(o, TickOutcome::AlreadyFinalized { .. }))
        .count();

    assert_eq!(finalized + already, 2);
    assert_eq!(finalized, 1, "exactly one relayer should win the race");
    assert_eq!(already, 1, "loser should observe AlreadyFinalized");
    assert_eq!(submitter.finalized_count(), 1);
}
