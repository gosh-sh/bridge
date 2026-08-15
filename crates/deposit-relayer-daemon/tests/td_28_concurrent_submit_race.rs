//! TD-28 — concurrent submitters, lost receipt, double-mint race (DEP-T14 / DEP-COMPETE).
//!
//! Extends F10-C (`f10_competing_submit.rs`) with barrier-synchronized races and
//! lost-receipt (Pending after on-chain mint) scenarios.

use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy::primitives::{Address, B256, U256};
use async_trait::async_trait;
use deposit_relayer_daemon::{
    error::RelayerError,
    prover::MockProofGenerator,
    relayer::{Relayer, RelayerConfig, TickOutcome},
    source::InMemoryDepositSource,
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

type R<A> = Relayer<InMemoryDepositSource, MockProofGenerator, A>;

fn relayer_cfg(state_path: PathBuf) -> RelayerConfig {
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    cfg
}

fn make_relayer<A: AnSubmitter + 'static>(
    source: Arc<InMemoryDepositSource>,
    prover: Arc<MockProofGenerator>,
    submitter: Arc<A>,
    state_path: PathBuf,
) -> R<A> {
    Relayer::new(relayer_cfg(state_path), source, prover, submitter).unwrap()
}

/// Synchronize concurrent `submit` calls at a tokio barrier.
struct BarrierSubmitter<A: AnSubmitter + Send + Sync + 'static> {
    inner: Arc<A>,
    barrier: Arc<tokio::sync::Barrier>,
}

#[async_trait]
impl<A: AnSubmitter + Send + Sync + 'static> AnSubmitter for BarrierSubmitter<A> {
    async fn is_finalized(&self, deposit_id: u64) -> Result<bool, RelayerError> {
        self.inner.is_finalized(deposit_id).await
    }

    async fn submit(
        &self,
        event: &DepositEvent,
        bundle: &DepositProofBundle,
    ) -> Result<SubmitOutcome, RelayerError> {
        self.barrier.wait().await;
        self.inner.submit(event, bundle).await
    }
}

/// First `submit` mints on AN but returns `Pending` (lost receipt); retries observe mint.
struct LostReceiptOnceSubmitter {
    inner: Arc<MockAnSubmitter>,
    arm_lost_receipt: Mutex<HashSet<u64>>,
}

impl LostReceiptOnceSubmitter {
    fn new(inner: Arc<MockAnSubmitter>) -> Self {
        Self {
            inner,
            arm_lost_receipt: Mutex::new(HashSet::new()),
        }
    }

    fn arm(&self, deposit_id: u64) {
        self.arm_lost_receipt
            .lock()
            .expect("poisoned")
            .insert(deposit_id);
    }
}

#[async_trait]
impl AnSubmitter for LostReceiptOnceSubmitter {
    async fn is_finalized(&self, deposit_id: u64) -> Result<bool, RelayerError> {
        self.inner.is_finalized(deposit_id).await
    }

    async fn submit(
        &self,
        event: &DepositEvent,
        bundle: &DepositProofBundle,
    ) -> Result<SubmitOutcome, RelayerError> {
        let simulate_lost = {
            let mut armed = self.arm_lost_receipt.lock().expect("poisoned");
            if armed.contains(&event.deposit_id) {
                armed.remove(&event.deposit_id);
                true
            } else {
                false
            }
        };
        let outcome = self.inner.submit(event, bundle).await?;
        if simulate_lost {
            match outcome {
                SubmitOutcome::Finalized { .. } => Ok(SubmitOutcome::Pending {
                    reason: "mock lost receipt after broadcast".to_string(),
                }),
                other => Ok(other),
            }
        } else {
            Ok(outcome)
        }
    }
}

/// Control — single relayer, one mint.
#[tokio::test]
async fn td_28_happy_single_submitter_one_mint() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = make_relayer(
        source,
        Arc::new(MockProofGenerator::new()),
        submitter.clone(),
        dir.path().join("state.json"),
    );

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert_eq!(submitter.finalized_count(), 1);
    assert_eq!(submitter.finalized_log(), vec![0]);
}

/// Barrier-synchronized concurrent ticks — exactly one `Finalized`, one `AlreadyFinalized`.
#[tokio::test]
async fn td_28_concurrent_barrier_exactly_one_mint() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    let prover = Arc::new(MockProofGenerator::new());
    let inner = Arc::new(MockAnSubmitter::accepting());
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let submitter: Arc<BarrierSubmitter<MockAnSubmitter>> = Arc::new(BarrierSubmitter {
        inner,
        barrier,
    });

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

    let (out_a, out_b) = tokio::join!(relayer_a.tick(), relayer_b.tick());

    let outcomes = [out_a.unwrap(), out_b.unwrap()];
    let finalized = outcomes
        .iter()
        .filter(|o| matches!(o, TickOutcome::Finalized { .. }))
        .count();
    let already = outcomes
        .iter()
        .filter(|o| matches!(o, TickOutcome::AlreadyFinalized { .. }))
        .count();

    assert_eq!(finalized, 1, "barrier race: one winner");
    assert_eq!(already, 1, "barrier race: loser AlreadyFinalized");
    assert_eq!(submitter.inner.finalized_count(), 1);
}

/// Lost receipt — mint on AN, relayer sees `Pending` then `AlreadyFinalized` on retry.
#[tokio::test]
async fn td_28_lost_receipt_relayer_retries_already_finalized() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    let inner = Arc::new(MockAnSubmitter::accepting());
    let submitter = Arc::new(LostReceiptOnceSubmitter::new(inner.clone()));
    submitter.arm(0);
    let mut relayer = make_relayer(
        source,
        Arc::new(MockProofGenerator::new()),
        submitter,
        dir.path().join("state.json"),
    );

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::AnPending { deposit_id: 0, .. }
    ));
    assert_eq!(inner.finalized_count(), 1, "AN minted despite lost receipt");

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::AlreadyFinalized { deposit_id: 0 }
    ));
    assert_eq!(inner.finalized_count(), 1);
}

/// Second concurrent submitter after lost-receipt mint — no double mint.
#[tokio::test]
async fn td_28_lost_receipt_second_submitter_no_double_mint() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    let prover = Arc::new(MockProofGenerator::new());
    let inner = Arc::new(MockAnSubmitter::accepting());
    let submitter = Arc::new(LostReceiptOnceSubmitter::new(inner.clone()));
    submitter.arm(0);

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
        TickOutcome::AnPending { deposit_id: 0, .. }
    ));
    assert!(matches!(
        relayer_b.tick().await.unwrap(),
        TickOutcome::AlreadyFinalized { deposit_id: 0 }
    ));
    assert_eq!(inner.finalized_count(), 1);
}

/// Barrier + shared nullifier mirror under DEP-N-5 identity scheme.
#[tokio::test]
async fn td_28_dep_n5_concurrent_barrier_one_mint() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    let prover = Arc::new(MockProofGenerator::new());
    let inner = Arc::new(MockAnSubmitter::accepting_dep_n5());
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let submitter: Arc<BarrierSubmitter<MockAnSubmitter>> = Arc::new(BarrierSubmitter {
        inner,
        barrier,
    });

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

    let (out_a, out_b) = tokio::join!(relayer_a.tick(), relayer_b.tick());
    let outcomes = [out_a.unwrap(), out_b.unwrap()];
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o, TickOutcome::Finalized { .. }))
            .count(),
        1
    );
    assert_eq!(submitter.inner.finalized_count(), 1);
}

/// Concurrent barrier after arming lost receipt — one mint, no double count.
#[tokio::test]
async fn td_28_barrier_concurrent_lost_receipt_one_mint() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    let prover = Arc::new(MockProofGenerator::new());
    let inner = Arc::new(MockAnSubmitter::accepting());
    let lost = Arc::new(LostReceiptOnceSubmitter::new(inner.clone()));
    lost.arm(0);
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let submitter: Arc<BarrierSubmitter<LostReceiptOnceSubmitter>> =
        Arc::new(BarrierSubmitter {
            inner: lost,
            barrier,
        });

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

    let (out_a, out_b) = tokio::join!(relayer_a.tick(), relayer_b.tick());
    let outcomes = [out_a.unwrap(), out_b.unwrap()];
    let mint_like = outcomes
        .iter()
        .filter(|o| {
            matches!(
                o,
                TickOutcome::Finalized { .. }
                    | TickOutcome::AnPending { .. }
                    | TickOutcome::AlreadyFinalized { .. }
            )
        })
        .count();
    assert_eq!(mint_like, 2);
    assert_eq!(inner.finalized_count(), 1);
}
