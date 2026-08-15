//! TD-65 / QC-OFF-06 — G3 live path: `AnInterfaceSubmitter` revert classification +
//! relayer HOL loop when duplicate is not mapped to `AlreadyFinalized`.
//!
//! Cross-ref: `f10_interface_reverted.rs`, `f10_competing_submit.rs`, TD-26 HOL.

use std::{path::PathBuf, sync::Arc, time::Duration};

use acki_nacki_interface::{EXIT_CONSTRUCTOR_ALREADY_CALLED, MockAckiNacki, TransactionStatus};
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

fn bundle(ev: &DepositEvent) -> deposit_relayer_daemon::types::DepositProofBundle {
    let pi = MockProofGenerator::derive_public_inputs(ev, U256::from(0xD499u64));
    deposit_relayer_daemon::types::DepositProofBundle {
        vk_blob: vec![1, 2, 3].into(),
        public_inputs: pi.to_operand().into(),
        proof: vec![0xAB; 48].into(),
        parsed: pi,
    }
}

fn an_submit_config() -> AnSubmitConfig {
    AnSubmitConfig {
        from: format!("{DAPP_HEX}::ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
        token_bridge: format!("{DAPP_HEX}::{DAPP_HEX}"),
        confirm_timeout_secs: 2,
    }
}

fn interface_submitter(client: Arc<MockAckiNacki>) -> AnInterfaceSubmitter<MockAckiNacki> {
    AnInterfaceSubmitter::new(client, an_submit_config())
}

type MockRelayer<A> = Relayer<InMemoryDepositSource, MockProofGenerator, A>;

fn make_mock_relayer(
    source: Arc<InMemoryDepositSource>,
    prover: Arc<MockProofGenerator>,
    submitter: Arc<MockAnSubmitter>,
    state_path: PathBuf,
) -> MockRelayer<MockAnSubmitter> {
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    Relayer::new(cfg, source, prover, submitter).unwrap()
}

fn make_interface_relayer<P: ProofGenerator>(
    source: Arc<InMemoryDepositSource>,
    prover: Arc<P>,
    submitter: Arc<AnInterfaceSubmitter<MockAckiNacki>>,
    state_path: PathBuf,
) -> Relayer<InMemoryDepositSource, P, AnInterfaceSubmitter<MockAckiNacki>> {
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    Relayer::new(cfg, source, prover, submitter).unwrap()
}

/// (a) Generic `Reverted` receipt without exit 51 → `Rejected` (not `AlreadyFinalized`).
#[tokio::test]
async fn td_65_a_interface_reverted_without_51_maps_to_rejected() {
    let client = Arc::new(MockAckiNacki::new());
    client.set_finalize_receipt_status(TransactionStatus::Reverted);
    let sub = interface_submitter(client);
    let ev = deposit(1);
    let b = bundle(&ev);

    match sub.submit(&ev, &b).await.unwrap() {
        SubmitOutcome::Rejected { reason } => {
            assert!(
                reason.contains("Reverted") || reason.contains("status"),
                "unexpected reason: {reason}"
            );
        },
        other => panic!("TD-65(a): expected Rejected, got {other:?}"),
    }
}

/// (b) `Reverted` with exit 51 (`EXIT_CONSTRUCTOR_ALREADY_CALLED`) → `AlreadyFinalized`.
#[tokio::test]
async fn td_65_b_interface_exit_51_maps_to_already_finalized() {
    let client = Arc::new(MockAckiNacki::new());
    client.set_finalize_receipt_status(TransactionStatus::Reverted);
    client.set_finalize_receipt_exit_code(EXIT_CONSTRUCTOR_ALREADY_CALLED);
    let sub = interface_submitter(client);
    let ev = deposit(2);
    let b = bundle(&ev);

    assert!(matches!(
        sub.submit(&ev, &b).await.unwrap(),
        SubmitOutcome::AlreadyFinalized
    ));
}

/// (c) Competing relayer ticks: loser observes `AlreadyFinalized`, cursor advances (F10-C).
#[tokio::test]
async fn td_65_c_competing_relayer_already_finalized_advances_cursor() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    let prover = Arc::new(MockProofGenerator::new());
    let submitter = Arc::new(MockAnSubmitter::accepting());

    let mut relayer_a = make_mock_relayer(
        source.clone(),
        prover.clone(),
        submitter.clone(),
        dir.path().join("state_a.json"),
    );
    let mut relayer_b = make_mock_relayer(
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

    assert!(matches!(
        relayer_b.tick().await.unwrap(),
        TickOutcome::AlreadyFinalized { deposit_id: 0 }
    ));
    assert_eq!(submitter.finalized_count(), 1, "no double mint");
    assert_eq!(relayer_b.state().last_processed_deposit_id, Some(0));
}

/// (d) Live-path `Reverted` without 51 → `AnRejected` HOL on same deposit id (QC-OFF-06).
#[tokio::test]
async fn td_65_d_interface_reverted_hol_same_deposit_id() {
    let dir = tempdir().unwrap();
    let client = Arc::new(MockAckiNacki::new());
    client.set_finalize_receipt_status(TransactionStatus::Reverted);
    let submitter = Arc::new(interface_submitter(client));
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    let prover = Arc::new(MockProofGenerator::new());
    let mut relayer = make_interface_relayer(
        source,
        prover,
        submitter,
        dir.path().join("state.json"),
    );

    let o1 = relayer.tick().await.unwrap();
    assert!(
        matches!(o1, TickOutcome::AnRejected { deposit_id: 0, .. }),
        "TD-65(d): first tick {o1:?}"
    );
    assert_eq!(relayer.state().last_processed_deposit_id, None);
    assert_eq!(relayer.state().attempts_since_progress, 1);

    let o2 = relayer.tick().await.unwrap();
    assert!(
        matches!(o2, TickOutcome::AnRejected { deposit_id: 0, .. }),
        "TD-65(d): HOL loop {o2:?}"
    );
    assert_eq!(relayer.state().attempts_since_progress, 2);
}

/// Interface relayer: exit 51 on first submit → `AlreadyFinalized`, cursor advances.
#[tokio::test]
async fn td_65_h_interface_relayer_exit_51_advances_cursor() {
    let dir = tempdir().unwrap();
    let client = Arc::new(MockAckiNacki::new());
    client.set_finalize_receipt_status(TransactionStatus::Reverted);
    client.set_finalize_receipt_exit_code(EXIT_CONSTRUCTOR_ALREADY_CALLED);
    let submitter = Arc::new(interface_submitter(client));
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    let prover = Arc::new(MockProofGenerator::new());
    let mut relayer = make_interface_relayer(
        source,
        prover,
        submitter,
        dir.path().join("state.json"),
    );

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::AlreadyFinalized { deposit_id: 0 }
    ));
    assert_eq!(relayer.state().last_processed_deposit_id, Some(0));
}

/// (e) `is_finalized` stub always `false` — re-prove waste after successful finalize (QC-OFF-05).
#[tokio::test]
async fn td_65_e_is_finalized_stub_false_after_mock_finalize() {
    let client = Arc::new(MockAckiNacki::new());
    let sub = interface_submitter(client);
    let ev = deposit(3);
    let b = bundle(&ev);

    assert!(!sub.is_finalized(ev.deposit_id).await.unwrap());
    assert!(matches!(
        sub.submit(&ev, &b).await.unwrap(),
        SubmitOutcome::Finalized { .. }
    ));
    assert!(
        !sub.is_finalized(ev.deposit_id).await.unwrap(),
        "QC-OFF-05: no read API — stub stays false after finalize"
    );
}

/// Relayer proves each deposit while `is_finalized` stub stays false (QC-OFF-05 waste).
#[tokio::test]
async fn td_65_e_relayer_proves_per_deposit_with_stub_is_finalized() {
    let dir = tempdir().unwrap();
    let client = Arc::new(MockAckiNacki::new());
    let submitter = Arc::new(interface_submitter(client));
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    source.insert(deposit(1));
    let prover = Arc::new(CountingProver::new(MockProofGenerator::new()));
    let mut relayer = make_interface_relayer(
        source,
        prover.clone(),
        submitter,
        dir.path().join("state.json"),
    );

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 1, .. }
    ));
    assert_eq!(
        prover.calls(),
        2,
        "stub is_finalized cannot skip prove — one generate() per deposit"
    );
}

/// Smoke: exit 220 on receipt → `Rejected` with ERR_INVALID_ZKPROOF hint (classify_reverted path).
#[tokio::test]
async fn td_65_f_interface_exit_220_maps_to_rejected_with_hint() {
    let client = Arc::new(MockAckiNacki::new());
    client.set_finalize_receipt_status(TransactionStatus::Reverted);
    client.set_finalize_receipt_exit_code(220);
    let sub = interface_submitter(client);
    let ev = deposit(4);
    let b = bundle(&ev);

    match sub.submit(&ev, &b).await.unwrap() {
        SubmitOutcome::Rejected { reason } => {
            assert!(reason.contains("220") || reason.contains("ERR_INVALID_ZKPROOF"));
        },
        other => panic!("TD-65(f): expected Rejected, got {other:?}"),
    }
}

struct CountingProver {
    inner: MockProofGenerator,
    calls: std::sync::Mutex<u32>,
}

impl CountingProver {
    fn new(inner: MockProofGenerator) -> Self {
        Self {
            inner,
            calls: std::sync::Mutex::new(0),
        }
    }

    fn calls(&self) -> u32 {
        *self.calls.lock().expect("poisoned")
    }
}

#[async_trait::async_trait]
impl ProofGenerator for CountingProver {
    async fn generate(
        &self,
        event: &DepositEvent,
    ) -> Result<deposit_relayer_daemon::types::DepositProofBundle, deposit_relayer_daemon::error::RelayerError> {
        *self.calls.lock().expect("poisoned") += 1;
        self.inner.generate(event).await
    }
}
