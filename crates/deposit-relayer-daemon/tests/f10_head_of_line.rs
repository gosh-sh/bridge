//! F10-F — head-of-line blocking when depositId N cannot finalize.
//!
//! Liveness: sequential cursor (`next_target = last_processed + 1`) means a
//! stuck deposit blocks all later ids. Documents QC-OFF-01.

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
async fn proof_failure_on_id_zero_blocks_id_one() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    source.insert(deposit(1));
    let prover = Arc::new(MockProofGenerator::failing_on(0));
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = make_relayer(source, prover, submitter.clone(), dir.path().join("state.json"));

    for _ in 0..3 {
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::ProofFailed { deposit_id: 0, .. }
        ));
    }
    assert_eq!(submitter.finalized_log(), Vec::<u64>::new());
    assert_eq!(relayer.state().last_processed_deposit_id, None);
    assert_eq!(relayer.state().attempts_since_progress, 3);
}

#[tokio::test]
async fn an_rejection_on_id_zero_blocks_id_one() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    source.insert(deposit(1));
    let prover = Arc::new(MockProofGenerator::new());
    let submitter = Arc::new(MockAnSubmitter::with_verifier(Arc::new(|_| false)));
    let mut relayer = make_relayer(source, prover, submitter.clone(), dir.path().join("state.json"));

    for _ in 0..3 {
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::AnRejected { deposit_id: 0, .. }
        ));
    }
    assert_eq!(submitter.finalized_log(), Vec::<u64>::new());
    assert_eq!(relayer.state().next_target(0), 0);
}
