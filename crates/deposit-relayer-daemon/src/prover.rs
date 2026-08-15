//! [`ProofGenerator`] — abstraction over "how do we turn a `Deposit` event
//! into the AN-consumable proof triple?".
//!
//! The deposit circuit lives in the `deposit-prover` crate, which is its own
//! cargo workspace (it pins axiom's halo2-lib v0.4.1, incompatible with this
//! workspace's dependency tree). Rather than force-merge two halo2 backends,
//! this crate keeps proof generation behind a trait with two backends:
//!
//! - [`MockProofGenerator`] — deterministic, no halo2. Derives the twelve
//!   public inputs (including proven `chainId`, the AN destination account, and
//!   the config dappId tag) straight from the event so the relayer + submitter
//!   can be driven end-to-end in unit tests in microseconds.
//! - [`SubprocessProofGenerator`] — production. Invokes `deposit-prover`'s
//!   `fetch_deposit_data` → `export_vk_blob` → `export_blake2b_proof` example
//!   binaries out-of-process (mirroring how the AN→ETH relayer consumes the
//!   Blake2b SHPLONK proof operands) and assembles the three opcode operands.

use std::{path::PathBuf, process::Stdio, time::Duration};

use alloy::primitives::U256;
use async_trait::async_trait;

use crate::{
    error::RelayerError,
    types::{DepositEvent, DepositProofBundle, DepositPublicInputs},
};

/// Best-effort kill for a timed-out subprocess (TD-52).
fn kill_process_pid(pid: Option<u32>) {
    if let Some(p) = pid {
        let _ = std::process::Command::new("kill")
            .arg("-TERM")
            .arg(p.to_string())
            .status();
    }
}

/// Produces a [`DepositProofBundle`] for a confirmed [`DepositEvent`].
#[async_trait]
pub trait ProofGenerator: Send + Sync {
    async fn generate(&self, event: &DepositEvent) -> Result<DepositProofBundle, RelayerError>;
}

// ─────────────────────────────────────────────────────────────────────
// MockProofGenerator — deterministic, no halo2
// ─────────────────────────────────────────────────────────────────────

/// Deterministic proof generator for tests. Derives the twelve public inputs
/// (proven `chainId` from `event.source_chain_id`, the AN destination account
/// from the event the same way the real circuit binds it, plus the config
/// dappId tag) so the submitter's `finalizeDeposit` args are realistic, and
/// emits canned `vk_blob` / `proof` bytes.
#[derive(Clone, Debug, Default)]
pub struct MockProofGenerator {
    /// When set, `generate` fails for this `deposit_id` — lets tests
    /// exercise the proof-generation-error path.
    pub fail_on: Option<u64>,
    /// Config-supplied AN dApp identifier (UInt256) bound as the dappId public
    /// inputs. Defaults to zero.
    pub dapp_id: U256,
}

impl MockProofGenerator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn failing_on(deposit_id: u64) -> Self {
        Self {
            fail_on: Some(deposit_id),
            dapp_id: U256::ZERO,
        }
    }

    /// Build a mock generator that stamps a given config dappId into the proof.
    pub fn with_dapp_id(dapp_id: U256) -> Self {
        Self {
            fail_on: None,
            dapp_id,
        }
    }

    /// The public inputs the real circuit would commit to for this event, given
    /// the config-supplied `dapp_id` tag (not part of the event).
    pub fn derive_public_inputs(event: &DepositEvent, dapp_id: U256) -> DepositPublicInputs {
        let addr_to_field = |bytes: &[u8]| {
            let mut buf = [0u8; 32];
            buf[12..].copy_from_slice(bytes);
            U256::from_be_bytes::<32>(buf)
        };
        let half = |slice: &[u8]| {
            let mut buf = [0u8; 32];
            buf[32 - slice.len()..].copy_from_slice(slice);
            U256::from_be_bytes::<32>(buf)
        };
        let dapp_be = dapp_id.to_be_bytes::<32>();
        DepositPublicInputs {
            deposit_id: U256::from(event.deposit_id),
            sender: addr_to_field(event.sender.as_slice()),
            amount: event.amount,
            contract_address: addr_to_field(event.source_contract.as_slice()),
            // Proven chainId mirrors RPC eth_chainId stamped on the event.
            chain_id: U256::from(event.source_chain_id),
            // dappId is the config tag, split into 16-byte halves (not from the
            // event). The AN account is bound from the event, split likewise.
            dapp_id_high: half(&dapp_be[0..16]),
            dapp_id_low: half(&dapp_be[16..32]),
            an_account_high: half(&event.an_account.as_slice()[0..16]),
            an_account_low: half(&event.an_account.as_slice()[16..32]),
            block_hash_high: half(&event.block_hash.as_slice()[0..16]),
            block_hash_low: half(&event.block_hash.as_slice()[16..32]),
            promise_commit: U256::ZERO,
        }
    }
}

#[async_trait]
impl ProofGenerator for MockProofGenerator {
    async fn generate(&self, event: &DepositEvent) -> Result<DepositProofBundle, RelayerError> {
        if self.fail_on == Some(event.deposit_id) {
            return Err(RelayerError::ProofGeneration(format!(
                "mock configured to fail on depositId={}",
                event.deposit_id
            )));
        }
        let parsed = Self::derive_public_inputs(event, self.dapp_id);
        Ok(DepositProofBundle {
            // Canned, non-empty operands — the relayer + submitter only care
            // about shape and the decoded public inputs in mock scenarios.
            vk_blob: vec![0x56, 0x4b, 0x00, 0x00].into(),
            public_inputs: parsed.to_operand().into(),
            proof: vec![0xAA; 32].into(),
            parsed,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────
// SubprocessProofGenerator — invokes deposit-prover example binaries
// ─────────────────────────────────────────────────────────────────────

/// Configuration for the out-of-process `deposit-prover` invocation.
#[derive(Clone, Debug)]
pub struct SubprocessProverConfig {
    /// Path to the `deposit-prover` crate root (its own cargo workspace).
    pub deposit_prover_dir: PathBuf,
    /// Ethereum RPC URL passed to `fetch_deposit_data`.
    pub rpc_url: String,
    /// Circuit degree (`k`). The `deposit-prover` examples default to 18.
    pub degree: u32,
    /// `--max-data-byte-len` for the receipt parser.
    pub max_data_byte_len: usize,
    /// `--max-log-num` upper bound on logs in the receipt.
    pub max_log_num: usize,
    /// Config-supplied Acki Nacki dApp identifier (UInt256), hex string. Passed
    /// to `fetch_deposit_data --dapp-id`; bound as the dappId public inputs.
    pub dapp_id: String,
    /// Hard timeout for the whole three-step pipeline. Halo2 proving for a
    /// fresh proving key can take minutes, so default generously.
    pub timeout: Duration,
}

impl SubprocessProverConfig {
    pub fn new(deposit_prover_dir: impl Into<PathBuf>, rpc_url: impl Into<String>) -> Self {
        Self {
            deposit_prover_dir: deposit_prover_dir.into(),
            rpc_url: rpc_url.into(),
            degree: 18,
            max_data_byte_len: 256,
            max_log_num: 20,
            dapp_id: "0".to_string(),
            timeout: Duration::from_secs(900),
        }
    }
}

/// Production proof generator. Shells out to the `deposit-prover` examples:
///
/// 1. `fetch_deposit_data` — RPC → `DepositProofInput` JSON (receipt RLP, MPT
///    proof, block header, parsed event fields);
/// 2. `export_vk_blob` — v2 RLC `VkBlob` for the deposit circuit;
/// 3. `export_blake2b_proof` — raw Blake2b SHPLONK proof + the 12×32-byte LE
///    public-input operand.
///
/// The three resulting files are read back and assembled into a
/// [`DepositProofBundle`]. This is intentionally heavyweight (it runs the
/// halo2 prover); operators typically pre-build the examples in release mode
/// so step (3) doesn't recompile.
pub struct SubprocessProofGenerator {
    config: SubprocessProverConfig,
}

impl SubprocessProofGenerator {
    pub fn new(mut config: SubprocessProverConfig) -> Self {
        // Subprocess spawn requires absolute paths when combined with
        // `current_dir`; operators often pass a relative `--deposit-prover-dir`.
        if let Ok(abs) = config.deposit_prover_dir.canonicalize() {
            config.deposit_prover_dir = abs;
        }
        Self {
            config,
        }
    }

    fn release_example_bin(&self, example: &str) -> Option<PathBuf> {
        let bin = self
            .config
            .deposit_prover_dir
            .join("target/release/examples")
            .join(example);
        if bin.is_file() {
            Some(bin)
        } else {
            None
        }
    }

    async fn run_example(&self, args: &[String]) -> Result<(), RelayerError> {
        use tokio::process::Command;

        let example = args
            .first()
            .ok_or_else(|| RelayerError::ProofGeneration("empty example args".into()))?;
        let passthrough: &[String] = match args.get(1).map(String::as_str) {
            Some("--") => &args[2..],
            _ => &args[1..],
        };

        let mut cmd = if let Some(bin) = self.release_example_bin(example) {
            let mut c = Command::new(bin);
            c.current_dir(&self.config.deposit_prover_dir)
                .args(passthrough);
            c
        } else {
            let mut c = Command::new("cargo");
            c.current_dir(&self.config.deposit_prover_dir)
                .arg("run")
                .arg("--release")
                .arg("--example");
            for a in args {
                c.arg(a);
            }
            c
        };
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);

        let child = cmd.spawn().map_err(|e| {
            RelayerError::ProofGeneration(format!("failed to spawn deposit-prover: {e}"))
        })?;
        let child_pid = child.id();

        // TD-52: on timeout, kill by pid — dropping a cancelled `wait_with_output`
        // future does not always terminate `cargo run --example` children.
        match tokio::time::timeout(self.config.timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => {
                if !output.status.success() {
                    return Err(RelayerError::ProofGeneration(format!(
                        "deposit-prover example {:?} exited with {}: {}",
                        args.first(),
                        output.status,
                        String::from_utf8_lossy(&output.stderr)
                    )));
                }
                Ok(())
            },
            Ok(Err(e)) => Err(RelayerError::ProofGeneration(format!(
                "failed to wait on deposit-prover child: {e}"
            ))),
            Err(_) => {
                kill_process_pid(child_pid);
                Err(RelayerError::ProofGeneration(format!(
                    "deposit-prover example timed out after {:?}",
                    self.config.timeout
                )))
            },
        }
    }
}

#[async_trait]
impl ProofGenerator for SubprocessProofGenerator {
    async fn generate(&self, event: &DepositEvent) -> Result<DepositProofBundle, RelayerError> {
        let workdir = tempfile::tempdir()
            .map_err(|e| RelayerError::ProofGeneration(format!("tempdir failed: {e}")))?;
        let input_json = workdir.path().join("deposit_proof_input.json");
        let vk_blob_path = workdir.path().join("deposit_vk_blob.bin");
        let proof_path = workdir.path().join("deposit_proof_blake2b.bin");
        let pubin_path = workdir.path().join("deposit_public_inputs.bin");

        let degree = self.config.degree.to_string();
        let max_data = self.config.max_data_byte_len.to_string();
        let max_log = self.config.max_log_num.to_string();
        let tx_hash = format!("{:#x}", event.tx_hash);
        let contract = format!("{:#x}", event.source_contract);
        let log_index = event.log_index.to_string();
        // Pass the chain we believe this deposit is on, so the prover can fail
        // fast if the witness it fetched proves a different one. Without it the
        // prover falls back to whatever the witness says and the mismatch would
        // only surface as an AN-side allowlist rejection.
        let chain_id = event.source_chain_id.to_string();

        // 1. Fetch witness.
        self.run_example(&[
            "fetch_deposit_data".into(),
            "--".into(),
            "--rpc-url".into(),
            self.config.rpc_url.clone(),
            "--tx-hash".into(),
            tx_hash,
            "--contract".into(),
            contract,
            "--log-index".into(),
            log_index,
            "--dapp-id".into(),
            self.config.dapp_id.clone(),
            "--output".into(),
            input_json.display().to_string(),
        ])
        .await?;

        // 2. Export the VkBlob.
        self.run_example(&[
            "export_vk_blob".into(),
            "--".into(),
            "--chain-id".into(),
            chain_id.clone(),
            "--input".into(),
            input_json.display().to_string(),
            "--output".into(),
            vk_blob_path.display().to_string(),
            "--degree".into(),
            degree.clone(),
            "--max-data-byte-len".into(),
            max_data.clone(),
            "--max-log-num".into(),
            max_log.clone(),
        ])
        .await?;

        // 3. Export the Blake2b proof + public inputs.
        self.run_example(&[
            "export_blake2b_proof".into(),
            "--".into(),
            "--chain-id".into(),
            chain_id.clone(),
            "--input".into(),
            input_json.display().to_string(),
            "--proof-out".into(),
            proof_path.display().to_string(),
            "--pubin-out".into(),
            pubin_path.display().to_string(),
            "--degree".into(),
            degree,
            "--max-data-byte-len".into(),
            max_data,
            "--max-log-num".into(),
            max_log,
        ])
        .await?;

        let read = |p: &std::path::Path| -> Result<Vec<u8>, RelayerError> {
            std::fs::read(p)
                .map_err(|e| RelayerError::ProofGeneration(format!("reading {}: {e}", p.display())))
        };
        let vk_blob = read(&vk_blob_path)?;
        let public_inputs = read(&pubin_path)?;
        let proof = read(&proof_path)?;

        let bundle = DepositProofBundle::from_operands(vk_blob, public_inputs, proof)?;
        bundle.check_binds_to(event)?;
        Ok(bundle)
    }
}

#[cfg(test)]
mod tests {
    use alloy::primitives::{Address, B256};

    use super::*;
    use crate::types::DepositEvent;

    fn event(id: u64) -> DepositEvent {
        DepositEvent {
            deposit_id: id,
            sender: Address::repeat_byte(0x11),
            amount: U256::from(1_000_000u64),
            an_workchain: 0,
            an_account: B256::repeat_byte(0x33),
            timestamp: U256::from(1_700_000_000u64),
            tx_hash: B256::repeat_byte(0xaa),
            log_index: 2,
            block_number: 500,
            block_hash: B256::repeat_byte(0xcd),
            source_contract: Address::repeat_byte(0x22),
            source_chain_id: 1,
        }
    }

    #[tokio::test]
    async fn mock_generates_bundle_bound_to_event() {
        let gen = MockProofGenerator::new();
        let ev = event(9);
        let bundle = gen.generate(&ev).await.unwrap();
        bundle.check_binds_to(&ev).unwrap();
        assert_eq!(bundle.parsed.deposit_id, U256::from(9u64));
        assert_eq!(bundle.parsed.amount, U256::from(1_000_000u64));
    }

    #[tokio::test]
    async fn mock_fail_on_triggers_error() {
        let gen = MockProofGenerator::failing_on(3);
        assert!(gen.generate(&event(3)).await.is_err());
        assert!(gen.generate(&event(4)).await.is_ok());
    }
}
