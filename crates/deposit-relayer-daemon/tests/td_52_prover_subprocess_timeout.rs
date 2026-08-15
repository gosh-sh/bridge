//! TD-52 — prover subprocess timeout / zombie on cancel.
//!
//! `SubprocessProofGenerator::run_example` must kill children on timeout;
//! relayer maps proof errors to `TickOutcome::ProofFailed` without advancing
//! `last_processed_deposit_id` (TD-07 cross-ref).

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use alloy::primitives::{Address, B256, U256};
use async_trait::async_trait;
use deposit_relayer_daemon::{
    error::RelayerError,
    prover::{
        MockProofGenerator, ProofGenerator, SubprocessProofGenerator, SubprocessProverConfig,
    },
    relayer::{Relayer, RelayerConfig, TickOutcome},
    source::InMemoryDepositSource,
    state::RelayerState,
    submitter::MockAnSubmitter,
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

fn process_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{}", pid)).exists()
}

fn kill_process_pid(pid: Option<u32>) {
    if let Some(p) = pid {
        let _ = std::process::Command::new("kill")
            .arg("-TERM")
            .arg(p.to_string())
            .status();
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("chmod");
}

/// Spawns `/bin/sleep` with the same kill-on-timeout pattern as `run_example`.
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
            RelayerError::ProofGeneration(format!("failed to spawn sleep prover mock: {e}"))
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
                kill_process_pid(child_pid);
                Err(RelayerError::ProofGeneration(format!(
                    "deposit-prover example timed out after {:?}",
                    self.timeout
                )))
            },
        }
    }
}

fn relayer_cfg(state_path: PathBuf) -> RelayerConfig {
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    cfg
}

fn setup_sleep_hanging_example(fixture_root: &Path, example_name: &str, pid_file: &Path) {
    let examples_dir = fixture_root.join("target/release/examples");
    std::fs::create_dir_all(&examples_dir).expect("create examples dir");
    let bin = examples_dir.join(example_name);
    let staging = examples_dir.join(format!("{example_name}.staging"));
    let script = format!(
        "#!/bin/sh\necho $$ > {}\nexec sleep 300\n",
        pid_file.display()
    );
    std::fs::write(&staging, script).expect("write sleep stub");
    make_executable(&staging);
    if bin.exists() {
        std::fs::remove_file(&bin).expect("remove stale stub");
    }
    std::fs::rename(&staging, &bin).expect("rename sleep stub");
}

/// (a) Slow prover mock + short timeout → `generate` Err contains `timed out`.
#[tokio::test]
async fn td_52_a_hang_proof_generator_times_out() {
    let gen = HangProofGenerator {
        sleep_secs: 600,
        timeout: Duration::from_millis(200),
    };
    let err = gen.generate(&deposit(0)).await.unwrap_err().to_string();
    assert!(
        err.contains("timed out"),
        "TD-52 hang mock must fail closed on timeout: {err}"
    );
}

/// (b) Relayer tick with slow prover → `ProofFailed`; cursor not advanced; state saved.
#[tokio::test]
async fn td_52_b_relayer_proof_failed_no_cursor_advance() {
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));

    let prover = Arc::new(HangProofGenerator {
        sleep_secs: 600,
        timeout: Duration::from_millis(200),
    });
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = Relayer::new(
        relayer_cfg(state_path.clone()),
        source,
        prover,
        submitter.clone(),
    )
    .unwrap();

    let outcome = relayer.tick().await.unwrap();
    assert!(
        matches!(outcome, TickOutcome::ProofFailed { deposit_id: 0, .. }),
        "TD-52 expected ProofFailed, got {outcome:?}"
    );
    assert_eq!(relayer.state().last_processed_deposit_id, None);
    assert_eq!(submitter.finalized_log(), Vec::<u64>::new());

    assert!(state_path.is_file(), "TD-52 proof failure must persist state");
    let loaded = RelayerState::load(&state_path)
        .expect("reload state")
        .expect("state file present");
    assert_eq!(loaded.last_processed_deposit_id, None);
    assert!(
        loaded.attempts_since_progress > 0,
        "TD-52 TD-07: attempt recorded on proof failure"
    );
}

/// (c) `SubprocessProofGenerator` + sleep stub binary → timeout + child killed.
#[tokio::test]
async fn td_52_c_subprocess_timeout_kills_child() {
    let dir = tempdir().unwrap();
    let pid_file = dir.path().join("td52_child.pid");
    setup_sleep_hanging_example(dir.path(), "fetch_deposit_data", &pid_file);

    let mut cfg = SubprocessProverConfig::new(dir.path(), "http://localhost:8545");
    cfg.timeout = Duration::from_millis(100);
    let gen = SubprocessProofGenerator::new(cfg);

    let err = gen.generate(&deposit(0)).await.unwrap_err().to_string();
    assert!(
        err.contains("timed out"),
        "TD-52 subprocess must timeout on hanging fetch_deposit_data: {err}"
    );

    tokio::time::sleep(Duration::from_millis(150)).await;

    if pid_file.is_file() {
        let raw = std::fs::read_to_string(&pid_file).expect("read pid file");
        let pid = raw.trim().parse::<u32>().expect("parse pid");
        assert!(
            !process_alive(pid),
            "TD-52 zombie probe: child pid {pid} still alive after timeout kill"
        );
    } else {
        panic!("TD-52 sleep stub did not record child pid");
    }
}

/// (d) Control: `MockProofGenerator` → `Finalized`.
#[tokio::test]
async fn td_52_d_control_mock_proof_generator_finalizes() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));

    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = Relayer::new(
        relayer_cfg(dir.path().join("state.json")),
        source,
        Arc::new(MockProofGenerator::new()),
        submitter.clone(),
    )
    .unwrap();

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert_eq!(submitter.finalized_log(), vec![0]);
    assert_eq!(relayer.state().last_processed_deposit_id, Some(0));
}
