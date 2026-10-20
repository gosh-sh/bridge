//! Keep log capture in its own test binary so parallel unit tests cannot
//! initialize the same tracing callsites without a subscriber.

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

use alloy::primitives::{Address, B256, U256};
use deposit_relayer_daemon::{
    state::unix_now_secs, AnSubmitter, DepositEvent, DepositProofBundle, InMemoryDepositSource,
    MockAnSubmitter, MockProofGenerator, ProofGenerator, Relayer, RelayerConfig, RelayerError,
    RelayerState, SubmitOutcome, TickOutcome,
};
use tempfile::tempdir;

#[derive(Default)]
struct CountingProver(AtomicUsize);

#[async_trait::async_trait]
impl ProofGenerator for CountingProver {
    async fn generate(&self, event: &DepositEvent) -> Result<DepositProofBundle, RelayerError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        MockProofGenerator::new().generate(event).await
    }
}

struct AnchorGate {
    accepted: AtomicBool,
    inner: MockAnSubmitter,
}

impl Default for AnchorGate {
    fn default() -> Self {
        Self {
            accepted: AtomicBool::new(false),
            inner: MockAnSubmitter::accepting(),
        }
    }
}

#[async_trait::async_trait]
impl AnSubmitter for AnchorGate {
    async fn is_block_accepted(&self, _: &DepositEvent) -> Result<bool, RelayerError> {
        Ok(self.accepted.load(Ordering::Relaxed))
    }

    async fn is_finalized(&self, event: &DepositEvent) -> Result<bool, RelayerError> {
        self.inner.is_finalized(event).await
    }

    async fn submit(
        &self,
        event: &DepositEvent,
        bundle: &DepositProofBundle,
    ) -> Result<SubmitOutcome, RelayerError> {
        self.inner.submit(event, bundle).await
    }
}

fn deposit(id: u64) -> DepositEvent {
    DepositEvent {
        deposit_id: id,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(id * 1000 + 1),
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(1_700_000_000u64),
        tx_hash: B256::repeat_byte(0xaa),
        log_index: 0,
        block_number: 100 + id,
        block_hash: B256::repeat_byte(0xcd),
        source_contract: Address::repeat_byte(0x22),
        source_chain_id: 1,
    }
}

#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CapturedLogs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn anchor_wait_logs_identify_the_deposit_and_warn_without_spamming_after_restart() {
    use tracing::instrument::WithSubscriber;

    let logs = CapturedLogs::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    async {
        assert_eq!(
            RelayerConfig::new("unused.json").anchor_wait_warn_after_secs,
            0
        );
        for warn_after_secs in [0, 3600] {
            logs.0.lock().unwrap().clear();
            let dir = tempdir().unwrap();
            let source = Arc::new(InMemoryDepositSource::new());
            source.insert(deposit(0));
            let prover = Arc::new(CountingProver::default());
            let gate = Arc::new(AnchorGate::default());
            let cfg = RelayerConfig {
                outstanding_backoff_secs: 0,
                anchor_wait_warn_after_secs: warn_after_secs,
                skip_after_attempts: Some(1),
                ..RelayerConfig::new(dir.path().join("state.json"))
            };
            let mut r =
                Relayer::new(cfg.clone(), source.clone(), prover.clone(), gate.clone()).unwrap();
            r.tick().await.unwrap();
            r.tick().await.unwrap();
            let output = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
            assert_eq!(output.matches("waiting for AN block anchor;").count(), 1);
            assert!(output.contains("INFO"));
            assert!(output.contains("deposit_id=0"));
            assert!(output.contains("chain_id=1"));
            assert!(output.contains("block_number=100"));
            assert!(output.contains(&format!("block_hash={}", deposit(0).block_hash)));
            let initial_warnings = usize::from(warn_after_secs == 0);
            assert_eq!(
                output.matches("AN block anchor still missing;").count(),
                initial_warnings
            );

            // Load an old wait with any prior warning due for repetition.
            let mut state = RelayerState::load(&cfg.state_path).unwrap().unwrap();
            state.outstanding_deposits[0].anchor_wait_started_unix =
                Some(unix_now_secs() - (60 * 60));
            if initial_warnings > 0 {
                state.outstanding_deposits[0].anchor_wait_last_warned_unix =
                    Some(unix_now_secs() - (15 * 60));
            }
            state.save(&cfg.state_path).unwrap();
            drop(r);
            let mut r =
                Relayer::new(cfg.clone(), source.clone(), prover.clone(), gate.clone()).unwrap();
            r.tick().await.unwrap();
            r.tick().await.unwrap();
            drop(r);
            let mut r = Relayer::new(cfg, source, prover.clone(), gate.clone()).unwrap();
            r.tick().await.unwrap();
            let output = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
            assert_eq!(output.matches("waiting for AN block anchor;").count(), 1);
            assert_eq!(
                output.matches("AN block anchor still missing;").count(),
                initial_warnings + 1
            );
            let warning = output.lines().find(|line| line.contains("WARN")).unwrap();
            for field in [
                "deposit_id=0",
                "chain_id=1",
                "block_number=100",
                "wait_secs=",
            ] {
                assert!(warning.contains(field), "missing {field}: {warning}");
            }
            assert!(warning.contains(&format!("block_hash={}", deposit(0).block_hash)));
            assert!(warning.contains("deposit_anchor_params.py --verify"));
            assert!(warning.contains("setAcceptedBlockHash"));
            assert_eq!(r.state().outstanding(0).unwrap().attempts, 0);
            assert!(r.state().parked_deposit_ids.is_empty());
            assert_eq!(prover.0.load(Ordering::Relaxed), 0);
            assert_eq!(gate.inner.finalized_count(), 0);

            gate.accepted.store(true, Ordering::Relaxed);
            assert!(matches!(r.tick().await.unwrap(), TickOutcome::Finalized {
                deposit_id: 0,
                ..
            }));
            assert!(r.state().outstanding_deposits.is_empty());
        }
    }
    .with_subscriber(subscriber)
    .await;
}
