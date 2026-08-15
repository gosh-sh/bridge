//! TD-05 — dappId injection: witness from config, double-mint namespace.
//!
//! Catalog `TD-05`: `dappId` is not in the L1 event (PI slots 5/6); AN rejects
//! mismatched proof-bound dappId with `ERR_WRONG_DAPP` (223).

use std::{path::PathBuf, sync::Arc, time::Duration};

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

const EXIT_WRONG_DAPP: i32 = 223;
const DAPP_A: U256 = U256::from_limbs([0x0a11_a000, 0, 0, 0]);
const DAPP_B: U256 = U256::from_limbs([0x0b22_b000, 0, 0, 0]);

fn deposit_event(deposit_id: u64) -> DepositEvent {
    DepositEvent {
        deposit_id,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(1_000_000u64),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(1_700_000_200u64),
        tx_hash: B256::repeat_byte(0xaa),
        log_index: 0,
        block_number: 100,
        block_hash: B256::repeat_byte(0xcd),
        source_contract: Address::repeat_byte(0x22),
        source_chain_id: 11_155_111,
    }
}

fn bundle_with_dapp(event: &DepositEvent, dapp_id: U256) -> DepositProofBundle {
    let parsed = MockProofGenerator::derive_public_inputs(event, dapp_id);
    DepositProofBundle {
        vk_blob: vec![0x56, 0x4b, 0x00, 0x00].into(),
        public_inputs: parsed.to_operand().into(),
        proof: vec![0xde, 0xad].into(),
        parsed,
    }
}

/// Prover stamps a config dappId into PI that differs from operator expectation.
#[derive(Clone, Debug)]
struct InjectedDappProver {
    stamped_dapp_id: U256,
}

#[async_trait]
impl ProofGenerator for InjectedDappProver {
    async fn generate(&self, event: &DepositEvent) -> Result<DepositProofBundle, RelayerError> {
        Ok(bundle_with_dapp(event, self.stamped_dapp_id))
    }
}

type R<P> = Relayer<InMemoryDepositSource, P, MockAnSubmitter>;

fn make_relayer<P: ProofGenerator>(
    source: Arc<InMemoryDepositSource>,
    prover: Arc<P>,
    submitter: Arc<MockAnSubmitter>,
    state_path: PathBuf,
    start_deposit_id: u64,
) -> R<P> {
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    cfg.start_deposit_id = start_deposit_id;
    Relayer::new(cfg, source, prover, submitter).unwrap()
}

#[tokio::test]
async fn td_05_wrong_dapp_id_in_pi_rejected_err_wrong_dapp() {
    let event = deposit_event(0);
    let bundle = bundle_with_dapp(&event, DAPP_B);
    let submitter = MockAnSubmitter::with_expected_dapp_id(DAPP_A);

    assert!(bundle.check_binds_to(&event).is_ok(), "dappId is not event-bound");

    match submitter.submit(&event, &bundle).await.unwrap() {
        SubmitOutcome::Rejected { reason } => {
            assert!(reason.contains("ERR_WRONG_DAPP"), "TD-05: {reason}");
            assert!(
                reason.contains(&format!("exit_code={EXIT_WRONG_DAPP}")),
                "TD-05: {reason}"
            );
        }
        other => panic!("TD-05: injected dappId must not mint: {other:?}"),
    }
    assert_eq!(submitter.finalized_count(), 0);
}

#[tokio::test]
async fn td_05_two_namespaces_same_event_only_one_mint() {
    let event = deposit_event(1);
    let bundle_a = bundle_with_dapp(&event, DAPP_A);
    let bundle_b = bundle_with_dapp(&event, DAPP_B);
    let submitter = MockAnSubmitter::with_expected_dapp_id(DAPP_A);

    assert!(matches!(
        submitter.submit(&event, &bundle_a).await.unwrap(),
        SubmitOutcome::Finalized { .. }
    ));
    assert_eq!(submitter.finalized_count(), 1);

    match submitter.submit(&event, &bundle_b).await.unwrap() {
        SubmitOutcome::Rejected { reason } => {
            assert!(reason.contains("ERR_WRONG_DAPP"));
            assert!(reason.contains("223"));
        }
        other => panic!("TD-05: second namespace must not mint: {other:?}"),
    }
    assert_eq!(submitter.finalized_count(), 1);
    assert_eq!(submitter.finalized_log(), vec![1]);
}

#[tokio::test]
async fn td_05_matching_expected_dapp_happy_finalize() {
    let event = deposit_event(2);
    let bundle = bundle_with_dapp(&event, DAPP_A);
    let submitter = MockAnSubmitter::with_expected_dapp_id(DAPP_A);

    assert!(matches!(
        submitter.submit(&event, &bundle).await.unwrap(),
        SubmitOutcome::Finalized { .. }
    ));
    assert_eq!(submitter.finalized_log(), vec![2]);
}

#[tokio::test]
async fn td_05_relayer_injected_pi_dapp_rejected_before_mint() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit_event(3));

    let submitter = Arc::new(MockAnSubmitter::with_expected_dapp_id(DAPP_A));
    let prover = Arc::new(InjectedDappProver {
        stamped_dapp_id: DAPP_B,
    });
    let mut relayer = make_relayer(
        source,
        prover,
        submitter.clone(),
        dir.path().join("state.json"),
        3,
    );

    let outcome = relayer.tick().await.unwrap();
    match outcome {
        TickOutcome::AnRejected { deposit_id: 3, reason } => {
            assert!(reason.contains("ERR_WRONG_DAPP"));
            assert!(reason.contains("223"));
        }
        other => panic!("TD-05: relayer must reject injected PI dappId: {other:?}"),
    }
    assert_eq!(submitter.finalized_count(), 0);
    assert_eq!(relayer.state().last_processed_deposit_id, None);
}

#[tokio::test]
async fn td_05_relayer_config_dapp_matches_pi_happy_path() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit_event(4));

    let submitter = Arc::new(MockAnSubmitter::with_expected_dapp_id(DAPP_A));
    let prover = Arc::new(MockProofGenerator::with_dapp_id(DAPP_A));
    let mut relayer = make_relayer(
        source,
        prover,
        submitter.clone(),
        dir.path().join("state.json"),
        4,
    );

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 4, .. }
    ));
    assert_eq!(submitter.finalized_log(), vec![4]);
}
