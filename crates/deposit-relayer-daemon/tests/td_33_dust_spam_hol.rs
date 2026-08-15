//! TD-33 — dust spam economics + head-of-line blocking / skip recovery.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};

use alloy::primitives::{Address, B256, U256};
use async_trait::async_trait;
use deposit_relayer_daemon::{
    error::RelayerError,
    prover::{MockProofGenerator, ProofGenerator},
    relayer::{Relayer, RelayerConfig, TickOutcome},
    source::InMemoryDepositSource,
    submitter::{AnSubmitter, MockAnSubmitter, SubmitOutcome},
    types::{DepositEvent, DepositProofBundle},
};
use tempfile::tempdir;

/// Dust ids 0..K-1 plus honest deposit id `K`.
const K: u64 = 5;
const HONEST_ID: u64 = K;
const DUST_AMOUNT: u64 = 1;
const HONEST_AMOUNT: u64 = 1_000_000;

fn dust_event(id: u64) -> DepositEvent {
    DepositEvent {
        deposit_id: id,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(DUST_AMOUNT),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(1_700_000_000u64 + id),
        tx_hash: B256::repeat_byte(0xaa + id as u8),
        log_index: 0,
        block_number: 100 + id,
        block_hash: B256::repeat_byte(0xcd),
        source_contract: Address::repeat_byte(0x22),
        source_chain_id: 11_155_111,
    }
}

fn honest_event() -> DepositEvent {
    let mut ev = dust_event(HONEST_ID);
    ev.amount = U256::from(HONEST_AMOUNT);
    ev
}

fn relayer_cfg(state_path: PathBuf, skip_after_attempts: Option<u32>) -> RelayerConfig {
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 32;
    cfg.skip_after_attempts = skip_after_attempts;
    cfg
}

fn seed_dust_plus_honest(source: &InMemoryDepositSource) {
    for id in 0..K {
        source.insert(dust_event(id));
    }
    source.insert(honest_event());
}

struct CountingProver {
    inner: MockProofGenerator,
    calls: AtomicU32,
}

impl CountingProver {
    fn new(inner: MockProofGenerator) -> Self {
        Self {
            inner,
            calls: AtomicU32::new(0),
        }
    }

    fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ProofGenerator for CountingProver {
    async fn generate(&self, event: &DepositEvent) -> Result<DepositProofBundle, RelayerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.generate(event).await
    }
}

type R<P> = Relayer<InMemoryDepositSource, P, MockAnSubmitter>;

#[tokio::test]
async fn td_33_poisoned_dust_id_zero_hol_without_skip_blocks_honest() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    seed_dust_plus_honest(&source);
    let prover = Arc::new(MockProofGenerator::failing_on(0));
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = Relayer::new(
        relayer_cfg(dir.path().join("state.json"), None),
        source,
        prover,
        submitter.clone(),
    )
    .unwrap();

    for _ in 0..8 {
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::ProofFailed { deposit_id: 0, .. }
        ));
    }
    assert!(!submitter.finalized_log().contains(&HONEST_ID));
    assert_eq!(submitter.finalized_log(), Vec::<u64>::new());
    assert_eq!(relayer.state().next_target(0), 0);
}

#[tokio::test]
async fn td_33_skip_after_attempts_recovers_honest_id_k() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    seed_dust_plus_honest(&source);
    let prover = Arc::new(MockProofGenerator::failing_on(0));
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let skip_after = 3u32;
    let mut relayer = Relayer::new(
        relayer_cfg(dir.path().join("state.json"), Some(skip_after)),
        source,
        prover,
        submitter.clone(),
    )
    .unwrap();

    let mut ticks = 0u32;
    let mut honest_finalized_at: Option<u32> = None;

    while ticks < 20 && honest_finalized_at.is_none() {
        ticks += 1;
        let outcome = relayer.tick().await.unwrap();
        if matches!(outcome, TickOutcome::Finalized { deposit_id: HONEST_ID, .. }) {
            honest_finalized_at = Some(ticks);
        }
    }

    assert_eq!(
        honest_finalized_at,
        Some(skip_after + HONEST_ID as u32),
        "TD-33: ticks until honest id K finalized (skip at attempt threshold)"
    );
    assert!(submitter.finalized_log().contains(&HONEST_ID));
    assert_eq!(relayer.state().parked_deposit_ids, vec![0]);
}

#[tokio::test]
async fn td_33_k_dust_sequential_ticks_linear_prove_cost() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    seed_dust_plus_honest(&source);
    let prover = Arc::new(CountingProver::new(MockProofGenerator::new()));
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = Relayer::new(
        relayer_cfg(dir.path().join("state.json"), None),
        source,
        prover.clone(),
        submitter.clone(),
    )
    .unwrap();

    let expected_ticks = HONEST_ID + 1;
    for _ in 0..expected_ticks {
        relayer.tick().await.unwrap();
    }

    assert_eq!(submitter.finalized_log().len(), expected_ticks as usize);
    assert_eq!(submitter.finalized_log().last().copied(), Some(HONEST_ID));
    assert_eq!(
        prover.calls(),
        expected_ticks as u32,
        "TD-33: prove cost O(N) — one generate() per deposit"
    );
}

#[tokio::test]
async fn td_33_dust_dep_n5_nullifier_blocks_replay_not_bc() {
    let submitter = MockAnSubmitter::accepting_dep_n5();
    let event = dust_event(0);
    let bundle = MockProofGenerator::new().generate(&event).await.unwrap();
    assert!(matches!(
        submitter.submit(&event, &bundle).await.unwrap(),
        SubmitOutcome::Finalized { .. }
    ));
    assert!(matches!(
        submitter.submit(&event, &bundle).await.unwrap(),
        SubmitOutcome::AlreadyFinalized
    ));
}
