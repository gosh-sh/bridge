//! TD-37 — competing submitters grief / peer `finalize-one` unlock (QC-OFF-02 / QC-AN-09).
//!
//! Cross-ref: TD-26 parked skip, TD-28 safety, TD-65 reject loop, `f10_competing_submit.rs`.

use std::{sync::Arc, time::Duration};

use alloy::primitives::{Address, B256, U256};
use deposit_relayer_daemon::{
    prover::{MockProofGenerator, ProofGenerator},
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

fn relayer_cfg(skip_after: Option<u32>) -> RelayerConfig {
    let mut cfg = RelayerConfig::new(std::path::PathBuf::from("state.json"));
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    cfg.skip_after_attempts = skip_after;
    cfg
}

/// Operator / peer path mirroring CLI `finalize-one` → `submit` with a good bundle.
async fn peer_finalize_one(
    submitter: &MockAnSubmitter,
    event: &DepositEvent,
) -> Result<SubmitOutcome, deposit_relayer_daemon::error::RelayerError> {
    let bundle = MockProofGenerator::new().generate(event).await?;
    submitter.submit(event, &bundle).await
}

/// (a) Poisoned id 0 HOL without skip — honest id 1 not processed.
#[tokio::test]
async fn td_37_a_poison_hol_blocks_honest_without_skip() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    source.insert(deposit(1));
    let prover = Arc::new(MockProofGenerator::failing_on(0));
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut cfg = relayer_cfg(None);
    cfg.state_path = dir.path().join("state.json");
    let mut relayer = Relayer::new(cfg, source, prover, submitter.clone()).unwrap();

    for _ in 0..6 {
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::ProofFailed { deposit_id: 0, .. }
        ));
    }
    assert!(!submitter.finalized_log().contains(&1));
    assert_eq!(relayer.state().next_target(0), 0);
    assert!(relayer.state().parked_deposit_ids.is_empty());
}

/// (a) Skip policy parks poisoned id 0; cursor can advance after skip threshold.
#[tokio::test]
async fn td_37_a_skip_parks_poisoned_id_zero() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    source.insert(deposit(1));
    let prover = Arc::new(MockProofGenerator::failing_on(0));
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut cfg = relayer_cfg(Some(2));
    cfg.state_path = dir.path().join("state.json");
    let mut relayer = Relayer::new(cfg, source, prover, submitter.clone()).unwrap();

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::ProofFailed { deposit_id: 0, .. }
    ));
    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Skipped { deposit_id: 0, .. }
    ));
    assert_eq!(relayer.state().parked_deposit_ids, vec![0]);
    assert_eq!(relayer.state().next_target(0), 1);
    assert!(!submitter.finalized_log().contains(&0));
}

/// (b) Peer `finalize-one` on parked id 0 → relayer A advances past 0 and finalizes id 1.
#[tokio::test]
async fn td_37_b_peer_finalize_one_unlocks_honest_on_relayer_a() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    source.insert(deposit(1));
    let prover = Arc::new(MockProofGenerator::failing_on(0));
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut cfg = relayer_cfg(None);
    cfg.state_path = dir.path().join("state.json");
    let mut relayer = Relayer::new(cfg, source, prover, submitter.clone()).unwrap();

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::ProofFailed { deposit_id: 0, .. }
    ));
    assert_eq!(relayer.state().next_target(0), 0);

    assert!(matches!(
        peer_finalize_one(submitter.as_ref(), &deposit(0)).await.unwrap(),
        SubmitOutcome::Finalized { .. }
    ));
    assert_eq!(submitter.finalized_count(), 1);

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::AlreadyFinalized { deposit_id: 0 }
    ));
    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 1, .. }
    ));
    assert_eq!(submitter.finalized_log(), vec![0, 1]);
}

/// (b) After skip+parking, peer finalize-one clears AN nullifier for id 0.
#[tokio::test]
async fn td_37_b_peer_finalize_parked_id_after_skip() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    source.insert(deposit(1));
    let prover = Arc::new(MockProofGenerator::failing_on(0));
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut cfg = relayer_cfg(Some(2));
    cfg.state_path = dir.path().join("state.json");
    let mut relayer = Relayer::new(cfg, source, prover, submitter.clone()).unwrap();

    relayer.tick().await.unwrap();
    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Skipped { deposit_id: 0, .. }
    ));
    assert_eq!(relayer.state().parked_deposit_ids, vec![0]);

    assert!(matches!(
        peer_finalize_one(submitter.as_ref(), &deposit(0)).await.unwrap(),
        SubmitOutcome::Finalized { .. }
    ));

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 1, .. }
    ));
    assert!(submitter.finalized_log().contains(&0));
    assert!(submitter.finalized_log().contains(&1));
}

/// (c) Competing relayer B wins; relayer A observes `AlreadyFinalized` (F10-C).
#[tokio::test]
async fn td_37_c_competing_relayer_b_first_a_advances_on_already_finalized() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    let prover = Arc::new(MockProofGenerator::new());
    let submitter = Arc::new(MockAnSubmitter::accepting());

    let mut cfg_a = relayer_cfg(None);
    cfg_a.state_path = dir.path().join("state_a.json");
    let mut relayer_a = Relayer::new(cfg_a, source.clone(), prover.clone(), submitter.clone())
        .unwrap();

    let mut cfg_b = relayer_cfg(None);
    cfg_b.state_path = dir.path().join("state_b.json");
    let mut relayer_b =
        Relayer::new(cfg_b, source, prover, submitter.clone()).unwrap();

    assert!(matches!(
        relayer_b.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert_eq!(submitter.finalized_count(), 1);

    assert!(matches!(
        relayer_a.tick().await.unwrap(),
        TickOutcome::AlreadyFinalized { deposit_id: 0 }
    ));
    assert_eq!(submitter.finalized_count(), 1);
    assert_eq!(relayer_a.state().last_processed_deposit_id, Some(0));
}

/// (d) Grief: invalid proof → `Rejected`, no mint, extra prove attempts only (QC-AN-09).
struct CountingProver {
    inner: MockProofGenerator,
    calls: std::sync::atomic::AtomicU32,
}

impl CountingProver {
    fn new(inner: MockProofGenerator) -> Self {
        Self {
            inner,
            calls: std::sync::atomic::AtomicU32::new(0),
        }
    }

    fn calls(&self) -> u32 {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl ProofGenerator for CountingProver {
    async fn generate(
        &self,
        event: &DepositEvent,
    ) -> Result<DepositProofBundle, deposit_relayer_daemon::error::RelayerError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.generate(event).await
    }
}

#[tokio::test]
async fn td_37_d_grief_invalid_proof_rejected_no_double_mint() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    let prover = Arc::new(CountingProver::new(MockProofGenerator::new()));
    let submitter = Arc::new(MockAnSubmitter::with_verifier(Arc::new(|_| false)));
    let mut cfg = relayer_cfg(None);
    cfg.state_path = dir.path().join("state.json");
    let mut relayer = Relayer::new(cfg, source, prover.clone(), submitter.clone()).unwrap();

    let o1 = relayer.tick().await.unwrap();
    assert!(matches!(o1, TickOutcome::AnRejected { deposit_id: 0, .. }));
    let o2 = relayer.tick().await.unwrap();
    assert!(matches!(o2, TickOutcome::AnRejected { deposit_id: 0, .. }));

    assert_eq!(submitter.finalized_count(), 0);
    assert_eq!(prover.calls(), 2, "grief replays prove+submit, no mint");
}

/// Cargo filter gate: `cargo test td_37_competing_grief_finalize_one`.
#[tokio::test]
async fn td_37_competing_grief_finalize_one() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    let prover = Arc::new(MockProofGenerator::new());
    let submitter = Arc::new(MockAnSubmitter::accepting());

    let mut cfg_a = relayer_cfg(None);
    cfg_a.state_path = dir.path().join("state_a.json");
    let mut relayer_a = Relayer::new(cfg_a, source.clone(), prover.clone(), submitter.clone())
        .unwrap();

    let mut cfg_b = relayer_cfg(None);
    cfg_b.state_path = dir.path().join("state_b.json");
    let mut relayer_b =
        Relayer::new(cfg_b, source, prover, submitter.clone()).unwrap();

    assert!(matches!(
        relayer_b.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert!(matches!(
        relayer_a.tick().await.unwrap(),
        TickOutcome::AlreadyFinalized { deposit_id: 0 }
    ));
    assert_eq!(submitter.finalized_count(), 1);
}
