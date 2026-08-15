//! TD-67 — competing provers: one L1 `Deposit` event, different `AN_DAPP_ID` PI namespaces.
//!
//! Cross-ref: TD-05 wrong dappId, TD-37 competing finalize-one, BC-AN-01 closed.

use std::{path::PathBuf, sync::Arc, time::Duration};

use alloy::primitives::{Address, B256, U256};
use deposit_relayer_daemon::{
    prover::{MockProofGenerator, ProofGenerator},
    relayer::{Relayer, RelayerConfig, TickOutcome},
    source::InMemoryDepositSource,
    submitter::{AnSubmitter, MockAnSubmitter, SubmitOutcome},
    types::DepositEvent,
};
use tempfile::tempdir;

const EXIT_WRONG_DAPP: i32 = 223;
const DAPP_A: U256 = U256::from_limbs([0x0a11_a000, 0, 0, 0]);
const DAPP_B: U256 = U256::from_limbs([0x0b22_b000, 0, 0, 0]);

/// Shared L1 deposit id 0 for all TD-67 cases.
fn shared_deposit_event() -> DepositEvent {
    DepositEvent {
        deposit_id: 0,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(1_000_000u64),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(1_700_000_300u64),
        tx_hash: B256::repeat_byte(0xaa),
        log_index: 0,
        block_number: 100,
        block_hash: B256::repeat_byte(0xcd),
        source_contract: Address::repeat_byte(0x22),
        source_chain_id: 11_155_111,
    }
}

fn relayer_cfg(state_path: PathBuf, start: u64) -> RelayerConfig {
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    cfg.start_deposit_id = start;
    cfg
}

async fn generate_with_dapp(
    prover: &MockProofGenerator,
    event: &DepositEvent,
) -> deposit_relayer_daemon::types::DepositProofBundle {
    prover.generate(event).await.unwrap()
}

/// (a) Prover A + submitter expecting A → Finalized.
#[tokio::test]
async fn td_67_a_prover_a_expected_dapp_finalized() {
    let event = shared_deposit_event();
    let prover = MockProofGenerator::with_dapp_id(DAPP_A);
    let bundle = generate_with_dapp(&prover, &event).await;
    let submitter = MockAnSubmitter::with_expected_dapp_id(DAPP_A);

    assert!(matches!(
        submitter.submit(&event, &bundle).await.unwrap(),
        SubmitOutcome::Finalized { .. }
    ));
    assert_eq!(submitter.finalized_count(), 1);
    assert_eq!(submitter.finalized_log(), vec![0]);
}

/// (b) Prover B same event after A minted → Rejected ERR_WRONG_DAPP; count still 1.
#[tokio::test]
async fn td_67_b_prover_b_wrong_namespace_rejected_no_double_mint() {
    let event = shared_deposit_event();
    let submitter = MockAnSubmitter::with_expected_dapp_id(DAPP_A);

    let bundle_a = generate_with_dapp(&MockProofGenerator::with_dapp_id(DAPP_A), &event).await;
    assert!(matches!(
        submitter.submit(&event, &bundle_a).await.unwrap(),
        SubmitOutcome::Finalized { .. }
    ));
    assert_eq!(submitter.finalized_count(), 1);

    let bundle_b = generate_with_dapp(&MockProofGenerator::with_dapp_id(DAPP_B), &event).await;
    match submitter.submit(&event, &bundle_b).await.unwrap() {
        SubmitOutcome::Rejected { reason } => {
            assert!(reason.contains("ERR_WRONG_DAPP"), "TD-67(b): {reason}");
            assert!(
                reason.contains(&format!("exit_code={EXIT_WRONG_DAPP}")),
                "TD-67(b): {reason}"
            );
        }
        other => panic!("TD-67(b): wrong dapp PI must not mint: {other:?}"),
    }
    assert_eq!(submitter.finalized_count(), 1);
}

/// (c) Relayer A (dapp A) finalizes; relayer B (dapp B) tick → AN reject before mint.
#[tokio::test]
async fn td_67_c_two_relayers_different_dapp_b_an_rejected() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(shared_deposit_event());

    let submitter = Arc::new(MockAnSubmitter::with_expected_dapp_id(DAPP_A));
    let prover_a = Arc::new(MockProofGenerator::with_dapp_id(DAPP_A));
    let prover_b = Arc::new(MockProofGenerator::with_dapp_id(DAPP_B));

    let mut relayer_a = Relayer::new(
        relayer_cfg(dir.path().join("state_a.json"), 0),
        source.clone(),
        prover_a,
        submitter.clone(),
    )
    .unwrap();

    let mut relayer_b = Relayer::new(
        relayer_cfg(dir.path().join("state_b.json"), 0),
        source,
        prover_b,
        submitter.clone(),
    )
    .unwrap();

    assert!(matches!(
        relayer_a.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert_eq!(submitter.finalized_count(), 1);

    // Relayer B: either AN rejects wrong PI (b) or nullifier pre-check skips prove (TD-37).
    match relayer_b.tick().await.unwrap() {
        TickOutcome::AnRejected { deposit_id: 0, reason } => {
            assert!(reason.contains("ERR_WRONG_DAPP"), "TD-67(c): {reason}");
            assert!(reason.contains("223"), "TD-67(c): {reason}");
            assert_eq!(relayer_b.state().last_processed_deposit_id, None);
        }
        TickOutcome::AlreadyFinalized { deposit_id: 0 } => {
            assert_eq!(
                relayer_b.state().last_processed_deposit_id,
                Some(0),
                "TD-67(c): pre-check advances cursor without wrong-PI submit"
            );
        }
        other => panic!("TD-67(c): relayer B must not double-mint: {other:?}"),
    }
    assert_eq!(submitter.finalized_count(), 1);
}

/// (d) Relayer B matching dapp after A finalized → AlreadyFinalized (TD-37 nullifier).
#[tokio::test]
async fn td_67_d_matching_dapp_already_finalized_no_double_mint() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(shared_deposit_event());

    let submitter = Arc::new(MockAnSubmitter::with_expected_dapp_id(DAPP_A));
    let prover = Arc::new(MockProofGenerator::with_dapp_id(DAPP_A));

    let mut relayer_a = Relayer::new(
        relayer_cfg(dir.path().join("state_a.json"), 0),
        source.clone(),
        prover.clone(),
        submitter.clone(),
    )
    .unwrap();

    let mut relayer_b = Relayer::new(
        relayer_cfg(dir.path().join("state_b.json"), 0),
        source,
        prover,
        submitter.clone(),
    )
    .unwrap();

    assert!(matches!(
        relayer_a.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert_eq!(submitter.finalized_count(), 1);

    assert!(matches!(
        relayer_b.tick().await.unwrap(),
        TickOutcome::AlreadyFinalized { deposit_id: 0 }
    ));
    assert_eq!(submitter.finalized_count(), 1);
    assert_eq!(relayer_b.state().last_processed_deposit_id, Some(0));
}
