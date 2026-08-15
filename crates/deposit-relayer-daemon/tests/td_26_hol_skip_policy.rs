//! TD-26 — HOL blocking, `record_skip` / parked IDs, transient vs permanent failure.
//!
//! Extends F10-F (`f10_head_of_line.rs`) with skip policy, state persistence, and recovery.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy::primitives::{Address, B256, U256};
use async_trait::async_trait;
use deposit_relayer_daemon::{
    error::RelayerError,
    prover::{MockProofGenerator, ProofGenerator},
    relayer::{Relayer, RelayerConfig, TickOutcome},
    source::InMemoryDepositSource,
    state::RelayerState,
    submitter::{AnSubmitter, MockAnSubmitter, SubmitOutcome},
    types::{DepositEvent, DepositProofBundle},
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

/// Proof generator that fails `fail_count` times per deposit id, then succeeds.
struct CountdownProofGenerator {
    inner: MockProofGenerator,
    remaining: Mutex<HashMap<u64, u32>>,
}

impl CountdownProofGenerator {
    fn with_failures(failures: HashMap<u64, u32>) -> Self {
        Self {
            inner: MockProofGenerator::new(),
            remaining: Mutex::new(failures),
        }
    }
}

#[async_trait]
impl ProofGenerator for CountdownProofGenerator {
    async fn generate(&self, event: &DepositEvent) -> Result<DepositProofBundle, RelayerError> {
        let should_fail = {
            let mut map = self.remaining.lock().expect("poisoned");
            if let Some(left) = map.get_mut(&event.deposit_id) {
                if *left > 0 {
                    *left -= 1;
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        if should_fail {
            return Err(RelayerError::ProofGeneration(format!(
                "TD-26 transient mock fail depositId={}",
                event.deposit_id
            )));
        }
        self.inner.generate(event).await
    }
}

/// Returns `Pending` once per deposit id, then delegates to the inner mock.
struct PendingOnceSubmitter {
    inner: Arc<MockAnSubmitter>,
    pending_once: Mutex<HashMap<u64, bool>>,
}

impl PendingOnceSubmitter {
    fn new(inner: Arc<MockAnSubmitter>) -> Self {
        Self {
            inner,
            pending_once: Mutex::new(HashMap::new()),
        }
    }

    fn arm_pending(&self, deposit_id: u64) {
        self.pending_once
            .lock()
            .expect("poisoned")
            .insert(deposit_id, true);
    }
}

#[async_trait]
impl AnSubmitter for PendingOnceSubmitter {
    async fn is_finalized(&self, deposit_id: u64) -> Result<bool, RelayerError> {
        self.inner.is_finalized(deposit_id).await
    }

    async fn submit(
        &self,
        event: &DepositEvent,
        bundle: &DepositProofBundle,
    ) -> Result<SubmitOutcome, RelayerError> {
        let pending = {
            let mut arms = self.pending_once.lock().expect("poisoned");
            if arms.get(&event.deposit_id) == Some(&true) {
                arms.insert(event.deposit_id, false);
                true
            } else {
                false
            }
        };
        if pending {
            return Ok(SubmitOutcome::Pending {
                reason: "mock AN RPC timeout".to_string(),
            });
        }
        self.inner.submit(event, bundle).await
    }
}

type R<P, A> = Relayer<InMemoryDepositSource, P, A>;

fn relayer_cfg(state_path: PathBuf, skip_after_attempts: Option<u32>) -> RelayerConfig {
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    cfg.skip_after_attempts = skip_after_attempts;
    cfg
}

#[tokio::test]
async fn td_26_poisoned_zero_blocks_one_until_skip_policy() {
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    source.insert(deposit(1));
    let prover = Arc::new(MockProofGenerator::failing_on(0));
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = Relayer::new(
        relayer_cfg(state_path.clone(), Some(3)),
        source,
        prover,
        submitter.clone(),
    )
    .unwrap();

    for _ in 0..2 {
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::ProofFailed { deposit_id: 0, .. }
        ));
    }
    assert_eq!(submitter.finalized_log(), Vec::<u64>::new());
    assert_eq!(relayer.state().next_target(0), 0);

    match relayer.tick().await.unwrap() {
        TickOutcome::Skipped { deposit_id, .. } => assert_eq!(deposit_id, 0),
        other => panic!("expected Skipped, got {other:?}"),
    }
    assert_eq!(relayer.state().parked_deposit_ids, vec![0]);
    assert_eq!(relayer.state().next_target(0), 1);

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 1, .. }
    ));
    assert_eq!(submitter.finalized_log(), vec![1]);
}

#[tokio::test]
async fn td_26_transient_proof_failure_not_permanent_skip() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    let prover = Arc::new(CountdownProofGenerator::with_failures(HashMap::from([(0, 2)])));
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = Relayer::new(
        relayer_cfg(dir.path().join("state.json"), Some(3)),
        source,
        prover,
        submitter.clone(),
    )
    .unwrap();

    for _ in 0..2 {
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::ProofFailed { deposit_id: 0, .. }
        ));
    }
    assert!(relayer.state().parked_deposit_ids.is_empty());

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert_eq!(submitter.finalized_log(), vec![0]);
    assert_eq!(relayer.state().attempts_since_progress, 0);
}

#[tokio::test]
async fn td_26_parked_id_persisted_in_state_json() {
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    let prover = Arc::new(MockProofGenerator::failing_on(0));
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = Relayer::new(
        relayer_cfg(state_path.clone(), Some(2)),
        source,
        prover,
        submitter,
    )
    .unwrap();

    for _ in 0..1 {
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::ProofFailed { deposit_id: 0, .. }
        ));
    }
    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Skipped { deposit_id: 0, .. }
    ));

    let loaded = RelayerState::load(&state_path).unwrap().unwrap();
    assert_eq!(loaded.parked_deposit_ids, vec![0]);
    assert_eq!(loaded.last_processed_deposit_id, Some(0));
    assert_eq!(loaded.next_target(0), 1);
}

#[tokio::test]
async fn td_26_recovery_after_manual_finalize_parked_id() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    source.insert(deposit(1));
    let prover = Arc::new(MockProofGenerator::failing_on(0));
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = Relayer::new(
        relayer_cfg(dir.path().join("state.json"), Some(2)),
        source,
        prover,
        submitter.clone(),
    )
    .unwrap();

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::ProofFailed { deposit_id: 0, .. }
    ));
    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Skipped { deposit_id: 0, .. }
    ));
    assert_eq!(relayer.state().parked_deposit_ids, vec![0]);

    submitter.seed_finalized(0);

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 1, .. }
    ));
    assert_eq!(submitter.finalized_log(), vec![1]);
    assert_eq!(relayer.state().parked_deposit_ids, vec![0]);
}

#[tokio::test]
async fn td_26_transient_an_pending_not_triggers_skip() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    let prover = Arc::new(MockProofGenerator::new());
    let inner = Arc::new(MockAnSubmitter::accepting());
    let submitter = Arc::new(PendingOnceSubmitter::new(inner.clone()));
    submitter.arm_pending(0);
    let mut relayer = Relayer::new(
        relayer_cfg(dir.path().join("state.json"), Some(2)),
        source,
        prover,
        submitter,
    )
    .unwrap();

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::AnPending { deposit_id: 0, .. }
    ));
    assert_eq!(relayer.state().attempts_since_progress, 1);
    assert!(relayer.state().parked_deposit_ids.is_empty());

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert_eq!(inner.finalized_log(), vec![0]);
}

#[tokio::test]
async fn td_26_interleaved_poison_skip_finalize_chain() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    for id in 0..3 {
        source.insert(deposit(id));
    }
    let prover = Arc::new(MockProofGenerator::failing_on(0));
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = Relayer::new(
        relayer_cfg(dir.path().join("state.json"), Some(2)),
        source,
        prover,
        submitter.clone(),
    )
    .unwrap();

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::ProofFailed { deposit_id: 0, .. }
    ));
    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Skipped { deposit_id: 0, .. }
    ));
    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 1, .. }
    ));
    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 2, .. }
    ));
    assert_eq!(submitter.finalized_log(), vec![1, 2]);
}

#[tokio::test]
async fn td_26_interleaved_transient_then_dual_finalize() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    source.insert(deposit(1));
    let prover = Arc::new(CountdownProofGenerator::with_failures(HashMap::from([(0, 1)])));
    let inner = Arc::new(MockAnSubmitter::accepting());
    let submitter = Arc::new(PendingOnceSubmitter::new(inner.clone()));
    submitter.arm_pending(1);
    let mut relayer = Relayer::new(
        relayer_cfg(dir.path().join("state.json"), Some(3)),
        source,
        prover,
        submitter,
    )
    .unwrap();

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::ProofFailed { deposit_id: 0, .. }
    ));
    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::AnPending { deposit_id: 1, .. }
    ));
    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 1, .. }
    ));
    assert_eq!(inner.finalized_log(), vec![0, 1]);
    assert!(relayer.state().parked_deposit_ids.is_empty());
}
