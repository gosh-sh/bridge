//! TD-53 — E2E runbook recovery: daemon fail → prove-one → finalize-one → restart.
//!
//! Mock/dry-run PoC (no live RPC/AN). Cross-ref: TD-52 proof timeout, TD-37
//! finalize-one unlock, TD-07 state persist on proof failure.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
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

const SEPOLIA: u64 = 11_155_111;

fn deposit(id: u64) -> DepositEvent {
    DepositEvent {
        deposit_id: id,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(1_000_000u64 + id),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(1_700_000_000u64 + id),
        tx_hash: B256::repeat_byte(0xaa + id as u8),
        log_index: 0,
        block_number: 100 + id,
        block_hash: B256::repeat_byte(0xcd),
        source_contract: Address::repeat_byte(0x22),
        source_chain_id: SEPOLIA,
    }
}

fn relayer_cfg(state_path: PathBuf, skip_after: Option<u32>) -> RelayerConfig {
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    cfg.skip_after_attempts = skip_after;
    cfg
}

/// Mirrors `HangProofGenerator` / TD-52 timeout path.
#[derive(Clone, Debug)]
struct HangProofGenerator {
    sleep_secs: u64,
    timeout: Duration,
}

#[async_trait]
impl ProofGenerator for HangProofGenerator {
    async fn generate(&self, _event: &DepositEvent) -> Result<DepositProofBundle, RelayerError> {
        use tokio::process::Command;

        let mut cmd = Command::new("/bin/sleep");
        cmd.arg(self.sleep_secs.to_string());
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let child = cmd.spawn().map_err(|e| {
            RelayerError::ProofGeneration(format!("failed to spawn hang prover mock: {e}"))
        })?;
        let child_pid = child.id();

        match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => Err(RelayerError::ProofGeneration(format!(
                "hang mock unexpectedly finished: {}",
                output.status
            ))),
            Ok(Err(e)) => Err(RelayerError::ProofGeneration(format!(
                "hang mock wait failed: {e}"
            ))),
            Err(_) => {
                if let Some(p) = child_pid {
                    let _ = std::process::Command::new("kill")
                        .arg("-TERM")
                        .arg(p.to_string())
                        .status();
                }
                Err(RelayerError::ProofGeneration(format!(
                    "deposit-prover example timed out after {:?}",
                    self.timeout
                )))
            },
        }
    }
}

/// Layout written by CLI `prove-one --out-dir`.
async fn write_prove_one_out_dir(out_dir: &Path, event: &DepositEvent) -> DepositProofBundle {
    let bundle = MockProofGenerator::new().generate(event).await.unwrap();
    std::fs::create_dir_all(out_dir).expect("create out_dir");
    std::fs::write(out_dir.join("vk_blob.bin"), &bundle.vk_blob).expect("vk_blob");
    std::fs::write(out_dir.join("public_inputs.bin"), &bundle.public_inputs)
        .expect("public_inputs");
    std::fs::write(out_dir.join("proof.bin"), &bundle.proof).expect("proof");
    bundle
}

/// Mirrors CLI `finalize-one`: read bundle dir → `submit` (MockAn = live `AnInterfaceSubmitter::submit_bundle`).
async fn finalize_one_from_bundle_dir(
    submitter: &MockAnSubmitter,
    event: &DepositEvent,
    bundle_dir: &Path,
) -> Result<SubmitOutcome, RelayerError> {
    let vk_blob = std::fs::read(bundle_dir.join("vk_blob.bin")).unwrap_or_default();
    let public_inputs = std::fs::read(bundle_dir.join("public_inputs.bin")).map_err(|e| {
        RelayerError::ProofGeneration(format!(
            "read {}/public_inputs.bin: {e}",
            bundle_dir.display()
        ))
    })?;
    let proof = std::fs::read(bundle_dir.join("proof.bin")).map_err(|e| {
        RelayerError::ProofGeneration(format!(
            "read {}/proof.bin: {e}",
            bundle_dir.display()
        ))
    })?;
    let bundle = DepositProofBundle::from_operands(vk_blob, public_inputs, proof)?;
    bundle.check_binds_to(event)?;
    submitter.submit(event, &bundle).await
}

fn load_state(path: &Path) -> RelayerState {
    RelayerState::load(path)
        .expect("load state")
        .expect("state file exists")
}

/// (a) Daemon tick with hanging prover → `ProofFailed`; state persisted (TD-52).
#[tokio::test]
async fn td_53_a_proof_failed_persisted_state() {
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));

    let mut relayer = Relayer::new(
        relayer_cfg(state_path.clone(), None),
        source,
        Arc::new(HangProofGenerator {
            sleep_secs: 600,
            timeout: Duration::from_millis(200),
        }),
        Arc::new(MockAnSubmitter::accepting()),
    )
    .unwrap();

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::ProofFailed { deposit_id: 0, .. }
    ));

    let snap = load_state(&state_path);
    assert_eq!(snap.last_processed_deposit_id, None);
    assert_eq!(snap.last_attempt_deposit_id, Some(0));
    assert!(snap.attempts_since_progress > 0);
    assert!(snap.parked_deposit_ids.is_empty());
}

/// (b) Operator `prove-one` out_dir + `finalize-one` submit path.
#[tokio::test]
async fn td_53_b_operator_prove_one_finalize_one() {
    let dir = tempdir().unwrap();
    let out_dir = dir.path().join("deposit-0");
    let event = deposit(0);
    write_prove_one_out_dir(&out_dir, &event).await;

    assert!(out_dir.join("vk_blob.bin").is_file());
    assert!(out_dir.join("public_inputs.bin").is_file());
    assert!(out_dir.join("proof.bin").is_file());

    let submitter = MockAnSubmitter::accepting();
    assert!(matches!(
        finalize_one_from_bundle_dir(&submitter, &event, &out_dir)
            .await
            .unwrap(),
        SubmitOutcome::Finalized { .. }
    ));
    assert_eq!(submitter.finalized_log(), vec![0]);
}

/// (c) Restart daemon: same state → `AlreadyFinalized` then catch-up to id 1.
#[tokio::test]
async fn td_53_c_restart_catches_up_after_finalize_one() {
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    source.insert(deposit(1));

    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = Relayer::new(
        relayer_cfg(state_path.clone(), None),
        source.clone(),
        Arc::new(HangProofGenerator {
            sleep_secs: 600,
            timeout: Duration::from_millis(200),
        }),
        submitter.clone(),
    )
    .unwrap();

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::ProofFailed { deposit_id: 0, .. }
    ));
    assert_eq!(load_state(&state_path).last_processed_deposit_id, None);

    let out_dir = dir.path().join("deposit-0");
    write_prove_one_out_dir(&out_dir, &deposit(0)).await;
    finalize_one_from_bundle_dir(submitter.as_ref(), &deposit(0), &out_dir)
        .await
        .unwrap();

    // Restart: new Relayer instance, same state file, healthy prover.
    let mut restarted = Relayer::new(
        relayer_cfg(state_path.clone(), None),
        source,
        Arc::new(MockProofGenerator::new()),
        submitter.clone(),
    )
    .unwrap();

    assert!(matches!(
        restarted.tick().await.unwrap(),
        TickOutcome::AlreadyFinalized { deposit_id: 0 }
    ));
    assert!(matches!(
        restarted.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 1, .. }
    ));
    assert_eq!(restarted.state().last_processed_deposit_id, Some(1));
    assert_eq!(submitter.finalized_log(), vec![0, 1]);
}

/// (d) Parked id 0 → manual finalize-one → daemon resumes id 1 (TD-37).
#[tokio::test]
async fn td_53_d_parked_manual_finalize_resumes() {
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    source.insert(deposit(1));

    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = Relayer::new(
        relayer_cfg(state_path.clone(), Some(2)),
        source.clone(),
        Arc::new(MockProofGenerator::failing_on(0)),
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
    let parked = load_state(&state_path);
    assert_eq!(parked.parked_deposit_ids, vec![0]);
    assert_eq!(parked.last_processed_deposit_id, Some(0));
    assert_eq!(parked.next_target(0), 1);

    let out_dir = dir.path().join("deposit-0");
    write_prove_one_out_dir(&out_dir, &deposit(0)).await;
    finalize_one_from_bundle_dir(submitter.as_ref(), &deposit(0), &out_dir)
        .await
        .unwrap();

    let mut resumed = Relayer::new(
        relayer_cfg(state_path, None),
        source,
        Arc::new(MockProofGenerator::new()),
        submitter.clone(),
    )
    .unwrap();

    assert!(matches!(
        resumed.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 1, .. }
    ));
    assert!(submitter.finalized_log().contains(&0));
    assert!(submitter.finalized_log().contains(&1));
}

/// Full mock E2E narrative in one test (runbook ordering).
#[tokio::test]
async fn td_53_full_mock_recovery_runbook() {
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));
    source.insert(deposit(1));
    let submitter = Arc::new(MockAnSubmitter::accepting());

    let mut daemon = Relayer::new(
        relayer_cfg(state_path.clone(), None),
        source.clone(),
        Arc::new(HangProofGenerator {
            sleep_secs: 600,
            timeout: Duration::from_millis(200),
        }),
        submitter.clone(),
    )
    .unwrap();

    assert!(matches!(
        daemon.tick().await.unwrap(),
        TickOutcome::ProofFailed { deposit_id: 0, .. }
    ));

    let out_dir = dir.path().join("out/deposit-0");
    write_prove_one_out_dir(&out_dir, &deposit(0)).await;
    finalize_one_from_bundle_dir(submitter.as_ref(), &deposit(0), &out_dir)
        .await
        .unwrap();

    let mut restarted = Relayer::new(
        relayer_cfg(state_path, None),
        source,
        Arc::new(MockProofGenerator::new()),
        submitter.clone(),
    )
    .unwrap();

    assert!(matches!(
        restarted.tick().await.unwrap(),
        TickOutcome::AlreadyFinalized { deposit_id: 0, .. }
    ));
    assert!(matches!(
        restarted.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 1, .. }
    ));
}
