//! M7 ETH-side proof pipeline: turn a **real** Circuit 4 `PrivateWitness` into
//! the SHPLONK aggregator calldata that the deployed
//! `BridgeWithdrawalAggregatorVerifier` accepts on-chain.
//!
//! Two out-of-process steps, each behind a trait so the relayer + submit path
//! stay unit-testable in microseconds (exactly like [`crate::withdraw_prover`]
//! and `deposit-relayer-daemon`'s `SubprocessProofGenerator`):
//!
//! 1. [`Circuit4SnarkProver`] — re-prove the witness with a **Poseidon**
//!    transcript and emit a snark-verifier `.snark`
//!    ([`SubprocessCircuit4SnarkProver`] shells out to
//!    `bridge-prover-orchestrator`'s `export-c4-poseidon-snark --fixture`).
//!    This is the `our_side_reprove` ETH leg: the AN-side default is Blake2b,
//!    but the aggregator only consumes Poseidon inner snarks.
//! 2. [`ProofAggregator`] — aggregate that inner snark into EVM calldata
//!    `instances ‖ proof` ([`SubprocessAggregator`] shells out to
//!    `bridge-evm-aggregator`'s `aggregate-proof`, which additionally
//!    self-checks that the regenerated Yul verifier is byte-identical to the
//!    committed/deployed `.bin`).
//!
//! [`Circuit4ShplonkPipeline`] composes the two and returns a
//! [`PartnerWithdrawalProof`] whose `proof_hex` is the aggregator calldata and
//! whose `public_instances_hex` are the ten Circuit-4 public inputs (LE Fr) —
//! exactly the shape `submit-withdraw` / `daemon-withdraw` / `daemon-bridge`
//! already consume. A cross-check ([`calldata_binds_instances`]) proves the
//! calldata's re-exposed instances match the ten public inputs before the
//! proof is surfaced, so a passing pipeline cannot forward mismatched bytes.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use alloy::primitives::U256;
use async_trait::async_trait;

use crate::{
    error::RelayerError,
    withdrawal::{
        fr_hex_to_u256, PartnerWithdrawalProof, SHPLONK_MIN_WITHDRAWAL_INSTANCES,
        WITHDRAWAL_PUBLIC_INPUTS,
    },
};

/// Number of 32-byte KZG accumulator limbs the aggregator prepends to the
/// re-exposed inner public inputs (snark-verifier SHPLONK accumulator).
pub const NUM_ACCUMULATOR_INSTANCES: usize = 12;

/// The committed Circuit-4 withdrawal verifier name (matches the `.bin` in
/// `contracts/ethereum/verifiers/`).
pub const WITHDRAWAL_VERIFIER_NAME: &str = "BridgeWithdrawalAggregatorVerifier";

/// Orchestrator binary that re-proves Circuit 4 with a Poseidon transcript.
pub const SNARK_BIN: &str = "export-c4-poseidon-snark";

/// Aggregator binary that turns a Poseidon inner snark into EVM calldata.
pub const AGGREGATE_BIN: &str = "aggregate-proof";

// ─────────────────────────────────────────────────────────────────────
// Circuit4SnarkProver — witness → Poseidon inner `.snark`
// ─────────────────────────────────────────────────────────────────────

/// Paths written by a [`Circuit4SnarkProver`] run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnarkArtefacts {
    /// The bincode-serialized snark-verifier `Snark` (aggregator inner input).
    pub snark_path: PathBuf,
    /// The ten Circuit-4 public instances, 32-byte **little-endian** Fr each
    /// (`save_instances_binary` layout), i.e. 320 bytes total.
    pub instances_path: PathBuf,
}

/// Re-prove a Circuit 4 `PrivateWitness` with a Poseidon transcript and emit a
/// snark-verifier `.snark` for the aggregator.
#[async_trait]
pub trait Circuit4SnarkProver: Send + Sync {
    async fn prove(
        &self,
        witness_path: &Path,
        snark_dir: &Path,
        name: &str,
    ) -> Result<SnarkArtefacts, RelayerError>;
}

/// Configuration for the out-of-process `export-c4-poseidon-snark` invocation.
#[derive(Clone, Debug)]
pub struct SubprocessCircuit4SnarkProverConfig {
    /// Path to the `crates/bridge-prover-orchestrator` root. The prebuilt
    /// release binary is expected at
    /// `<dir>/target/release/export-c4-poseidon-snark`; if absent we fall
    /// back to `cargo run --release --bin export-c4-poseidon-snark`.
    pub orchestrator_dir: PathBuf,
    /// Directory holding `kzg_bn254_*.srs` + Circuit-4 keys. Passed as
    /// `--params-dir` and exported as `PARAMS_DIR` for the subprocess.
    pub params_dir: PathBuf,
    /// Hard timeout for the (cold-PK) proving run.
    pub timeout: Duration,
}

impl SubprocessCircuit4SnarkProverConfig {
    pub fn new(orchestrator_dir: impl Into<PathBuf>, params_dir: impl Into<PathBuf>) -> Self {
        Self {
            orchestrator_dir: orchestrator_dir.into(),
            params_dir: params_dir.into(),
            timeout: Duration::from_secs(1800),
        }
    }
}

/// Production snark prover. Shells out to the orchestrator binary.
pub struct SubprocessCircuit4SnarkProver {
    config: SubprocessCircuit4SnarkProverConfig,
}

impl SubprocessCircuit4SnarkProver {
    pub fn new(mut config: SubprocessCircuit4SnarkProverConfig) -> Self {
        if let Ok(abs) = config.orchestrator_dir.canonicalize() {
            config.orchestrator_dir = abs;
        }
        if let Ok(abs) = config.params_dir.canonicalize() {
            config.params_dir = abs;
        }
        Self {
            config,
        }
    }

    fn release_bin(&self) -> Option<PathBuf> {
        let bin = self
            .config
            .orchestrator_dir
            .join("target/release")
            .join(SNARK_BIN);
        bin.is_file().then_some(bin)
    }

    /// argv (program excluded) — pulled out for unit-testing flag construction.
    fn args(&self, witness_path: &Path, snark_dir: &Path, name: &str) -> Vec<String> {
        vec![
            "--params-dir".to_string(),
            self.config.params_dir.display().to_string(),
            "--snark-dir".to_string(),
            snark_dir.display().to_string(),
            "--name".to_string(),
            name.to_string(),
            "--fixture".to_string(),
            witness_path.display().to_string(),
        ]
    }
}

#[async_trait]
impl Circuit4SnarkProver for SubprocessCircuit4SnarkProver {
    async fn prove(
        &self,
        witness_path: &Path,
        snark_dir: &Path,
        name: &str,
    ) -> Result<SnarkArtefacts, RelayerError> {
        use tokio::process::Command;

        std::fs::create_dir_all(snark_dir)
            .map_err(|e| RelayerError::other(format!("create snark dir: {e}")))?;
        let args = self.args(witness_path, snark_dir, name);

        let mut cmd = if let Some(bin) = self.release_bin() {
            let mut c = Command::new(bin);
            c.current_dir(&self.config.orchestrator_dir).args(&args);
            c
        } else {
            let mut c = Command::new("cargo");
            c.current_dir(&self.config.orchestrator_dir)
                .args(["run", "--release", "--bin", SNARK_BIN, "--"])
                .args(&args);
            c
        };
        cmd.env("PARAMS_DIR", &self.config.params_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = tokio::time::timeout(self.config.timeout, cmd.output())
            .await
            .map_err(|_| {
                RelayerError::other(format!(
                    "{SNARK_BIN} timed out after {:?}",
                    self.config.timeout
                ))
            })?
            .map_err(|e| RelayerError::other(format!("failed to spawn {SNARK_BIN}: {e}")))?;

        if !output.status.success() {
            return Err(RelayerError::other(format!(
                "{SNARK_BIN} exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        let artefacts = SnarkArtefacts {
            snark_path: snark_dir.join(format!("{name}.snark")),
            instances_path: snark_dir.join(format!("{name}.instances.bin")),
        };
        if !artefacts.snark_path.is_file() {
            return Err(RelayerError::other(format!(
                "{SNARK_BIN} did not produce {}",
                artefacts.snark_path.display()
            )));
        }
        Ok(artefacts)
    }
}

// ─────────────────────────────────────────────────────────────────────
// ProofAggregator — inner `.snark` → EVM calldata `instances ‖ proof`
// ─────────────────────────────────────────────────────────────────────

/// Aggregate a Poseidon inner `.snark` into EVM calldata for the named,
/// already-committed aggregator verifier.
#[async_trait]
pub trait ProofAggregator: Send + Sync {
    async fn aggregate(
        &self,
        inner_snark: &Path,
        verifier_name: &str,
    ) -> Result<Vec<u8>, RelayerError>;
}

/// Configuration for the out-of-process `aggregate-proof` invocation.
#[derive(Clone, Debug)]
pub struct SubprocessAggregatorConfig {
    /// Path to the `crates/bridge-evm-aggregator` root. The prebuilt release
    /// binary is expected at `<dir>/target/release/aggregate-proof`; if absent
    /// we fall back to `cargo run --release --bin aggregate-proof`.
    pub aggregator_dir: PathBuf,
    /// Directory of committed verifier `.bin` files (the self-check target).
    /// Passed as `--verifiers-dir`.
    pub verifiers_dir: PathBuf,
    /// Directory holding `kzg_bn254_21.srs` (the outer SRS). Exported as
    /// `PARAMS_DIR` for the subprocess so `gen_srs(21)` finds the ceremony
    /// file.
    pub params_dir: PathBuf,
    /// Hard timeout for the (K=21) aggregation run.
    pub timeout: Duration,
}

impl SubprocessAggregatorConfig {
    pub fn new(
        aggregator_dir: impl Into<PathBuf>,
        verifiers_dir: impl Into<PathBuf>,
        params_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            aggregator_dir: aggregator_dir.into(),
            verifiers_dir: verifiers_dir.into(),
            params_dir: params_dir.into(),
            timeout: Duration::from_secs(1800),
        }
    }
}

/// Production aggregator. Shells out to `aggregate-proof`.
pub struct SubprocessAggregator {
    config: SubprocessAggregatorConfig,
}

impl SubprocessAggregator {
    pub fn new(mut config: SubprocessAggregatorConfig) -> Self {
        if let Ok(abs) = config.aggregator_dir.canonicalize() {
            config.aggregator_dir = abs;
        }
        if let Ok(abs) = config.verifiers_dir.canonicalize() {
            config.verifiers_dir = abs;
        }
        if let Ok(abs) = config.params_dir.canonicalize() {
            config.params_dir = abs;
        }
        Self {
            config,
        }
    }

    fn release_bin(&self) -> Option<PathBuf> {
        let bin = self
            .config
            .aggregator_dir
            .join("target/release")
            .join(AGGREGATE_BIN);
        bin.is_file().then_some(bin)
    }

    /// argv (program excluded) for a given inner snark, verifier name and
    /// output path — pulled out for unit-testing flag construction.
    fn args(&self, inner_snark: &Path, verifier_name: &str, out_path: &Path) -> Vec<String> {
        vec![
            "--inner-snark".to_string(),
            inner_snark.display().to_string(),
            "--name".to_string(),
            verifier_name.to_string(),
            "--out".to_string(),
            out_path.display().to_string(),
            "--verifiers-dir".to_string(),
            self.config.verifiers_dir.display().to_string(),
        ]
    }
}

#[async_trait]
impl ProofAggregator for SubprocessAggregator {
    async fn aggregate(
        &self,
        inner_snark: &Path,
        verifier_name: &str,
    ) -> Result<Vec<u8>, RelayerError> {
        use tokio::process::Command;

        let out_path = std::env::temp_dir().join(format!(
            "agg_calldata_{verifier_name}_{}.bin",
            std::process::id()
        ));
        let args = self.args(inner_snark, verifier_name, &out_path);

        let mut cmd = if let Some(bin) = self.release_bin() {
            let mut c = Command::new(bin);
            c.current_dir(&self.config.aggregator_dir).args(&args);
            c
        } else {
            let mut c = Command::new("cargo");
            c.current_dir(&self.config.aggregator_dir)
                .args(["run", "--release", "--bin", AGGREGATE_BIN, "--"])
                .args(&args);
            c
        };
        cmd.env("PARAMS_DIR", &self.config.params_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = tokio::time::timeout(self.config.timeout, cmd.output())
            .await
            .map_err(|_| {
                RelayerError::other(format!(
                    "{AGGREGATE_BIN} timed out after {:?}",
                    self.config.timeout
                ))
            })?
            .map_err(|e| RelayerError::other(format!("failed to spawn {AGGREGATE_BIN}: {e}")))?;

        if !output.status.success() {
            return Err(RelayerError::other(format!(
                "{AGGREGATE_BIN} exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        let calldata = std::fs::read(&out_path).map_err(|e| {
            RelayerError::other(format!(
                "read aggregator calldata {}: {e}",
                out_path.display()
            ))
        })?;
        std::fs::remove_file(&out_path).ok();
        Ok(calldata)
    }
}

// ─────────────────────────────────────────────────────────────────────
// Pipeline — compose snark prover + aggregator → PartnerWithdrawalProof
// ─────────────────────────────────────────────────────────────────────

/// Compose a [`Circuit4SnarkProver`] and a [`ProofAggregator`] into the full
/// ETH-side withdrawal proof path.
pub struct Circuit4ShplonkPipeline<S: Circuit4SnarkProver, A: ProofAggregator> {
    pub snark_prover: S,
    pub aggregator: A,
    pub verifier_name: String,
}

impl<S: Circuit4SnarkProver, A: ProofAggregator> Circuit4ShplonkPipeline<S, A> {
    pub fn new(snark_prover: S, aggregator: A) -> Self {
        Self {
            snark_prover,
            aggregator,
            verifier_name: WITHDRAWAL_VERIFIER_NAME.to_string(),
        }
    }

    /// Prove `witness_path` → Poseidon snark → aggregate → calldata, and return
    /// a [`PartnerWithdrawalProof`] carrying the calldata + ten public inputs.
    /// `snark_dir` receives the intermediate `<name>.snark` / `.instances.bin`.
    pub async fn prove(
        &self,
        witness_path: &Path,
        snark_dir: &Path,
        seq_no: u64,
    ) -> Result<PartnerWithdrawalProof, RelayerError> {
        let artefacts = self
            .snark_prover
            .prove(witness_path, snark_dir, "circuit4")
            .await?;

        let calldata = self
            .aggregator
            .aggregate(&artefacts.snark_path, &self.verifier_name)
            .await?;

        let instances_hex = read_instances_le(&artefacts.instances_path)?;

        // The calldata's re-exposed inner instances (words 12..21, big-endian)
        // must equal the ten public inputs. If they don't, the on-chain verifier
        // would bind different values than the caller passes in
        // `WithdrawalPublicInputs` — refuse to surface such a proof.
        calldata_binds_instances(&calldata, &instances_hex)?;

        Ok(PartnerWithdrawalProof {
            schema_version: 1,
            seq_no,
            proof_hex: hex::encode(&calldata),
            public_instances_hex: instances_hex,
            self_verified: true,
        })
    }
}

/// Read a `save_instances_binary` file (N × 32-byte LE Fr) into per-instance
/// LE hex strings. Requires exactly [`WITHDRAWAL_PUBLIC_INPUTS`] instances.
pub fn read_instances_le(path: &Path) -> Result<Vec<String>, RelayerError> {
    let bytes = std::fs::read(path)
        .map_err(|e| RelayerError::other(format!("read instances {}: {e}", path.display())))?;
    if bytes.len() != WITHDRAWAL_PUBLIC_INPUTS * 32 {
        return Err(RelayerError::other(format!(
            "instances file {} is {} bytes; expected {} ({} × 32B LE Fr)",
            path.display(),
            bytes.len(),
            WITHDRAWAL_PUBLIC_INPUTS * 32,
            WITHDRAWAL_PUBLIC_INPUTS
        )));
    }
    Ok(bytes.as_chunks::<32>().0.iter().map(hex::encode).collect())
}

/// Assert that the aggregator calldata re-exposes exactly the ten Circuit-4
/// public inputs: `calldata[(12+i)*32 .. (13+i)*32]` (big-endian EVM word)
/// numerically equals `instances_hex[i]` (little-endian Fr repr), for all i.
pub fn calldata_binds_instances(
    calldata: &[u8],
    instances_hex: &[String],
) -> Result<(), RelayerError> {
    if calldata.len() < SHPLONK_MIN_WITHDRAWAL_INSTANCES {
        return Err(RelayerError::other(format!(
            "aggregator calldata is {} bytes; need >= {} to hold {} accumulator limbs + {} inputs",
            calldata.len(),
            SHPLONK_MIN_WITHDRAWAL_INSTANCES,
            NUM_ACCUMULATOR_INSTANCES,
            WITHDRAWAL_PUBLIC_INPUTS
        )));
    }
    if instances_hex.len() != WITHDRAWAL_PUBLIC_INPUTS {
        return Err(RelayerError::other(format!(
            "expected {} instances, got {}",
            WITHDRAWAL_PUBLIC_INPUTS,
            instances_hex.len()
        )));
    }
    for (i, inst) in instances_hex.iter().enumerate() {
        let off = (NUM_ACCUMULATOR_INSTANCES + i) * 32;
        let word = &calldata[off..off + 32];
        let from_calldata = U256::from_be_slice(word);
        let from_instance = fr_hex_to_u256(inst)?;
        if from_calldata != from_instance {
            return Err(RelayerError::other(format!(
                "calldata instance[{i}] ({from_calldata}) != public input[{i}] ({from_instance}) \
                 — aggregator calldata does not bind the declared public inputs"
            )));
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────
// Mocks — deterministic, no subprocess
// ─────────────────────────────────────────────────────────────────────

/// Deterministic snark prover for tests: writes an empty `<name>.snark` and a
/// 320-byte instances file (ten ascending LE Fr) into `snark_dir`.
#[derive(Clone, Debug, Default)]
pub struct MockCircuit4SnarkProver {
    pub fail: bool,
}

#[async_trait]
impl Circuit4SnarkProver for MockCircuit4SnarkProver {
    async fn prove(
        &self,
        _witness_path: &Path,
        snark_dir: &Path,
        name: &str,
    ) -> Result<SnarkArtefacts, RelayerError> {
        if self.fail {
            return Err(RelayerError::other("mock snark prover configured to fail"));
        }
        std::fs::create_dir_all(snark_dir)
            .map_err(|e| RelayerError::other(format!("mkdir: {e}")))?;
        let snark_path = snark_dir.join(format!("{name}.snark"));
        let instances_path = snark_dir.join(format!("{name}.instances.bin"));
        std::fs::write(&snark_path, b"mock-snark")
            .map_err(|e| RelayerError::other(format!("write snark: {e}")))?;
        let mut instances = Vec::with_capacity(WITHDRAWAL_PUBLIC_INPUTS * 32);
        for i in 0..WITHDRAWAL_PUBLIC_INPUTS as u8 {
            let mut le = [0u8; 32];
            le[0] = i;
            instances.extend_from_slice(&le);
        }
        std::fs::write(&instances_path, &instances)
            .map_err(|e| RelayerError::other(format!("write instances: {e}")))?;
        Ok(SnarkArtefacts {
            snark_path,
            instances_path,
        })
    }
}

/// Deterministic aggregator for tests: returns 3616-byte calldata whose
/// re-exposed instance words (12..21) match [`MockCircuit4SnarkProver`]'s ten
/// ascending LE instances, so [`calldata_binds_instances`] passes.
#[derive(Clone, Debug, Default)]
pub struct MockAggregator {
    pub fail: bool,
}

impl MockAggregator {
    /// Build calldata that binds the given LE-instance hex strings (big-endian
    /// words at positions 12..21), padded to a realistic 3616-byte length.
    pub fn calldata_binding(instances_hex: &[String]) -> Vec<u8> {
        let total_len = 3616;
        let mut cd = vec![0u8; total_len];
        for (i, inst) in instances_hex.iter().enumerate() {
            let val = fr_hex_to_u256(inst).unwrap_or(U256::ZERO);
            let be = val.to_be_bytes::<32>();
            let off = (NUM_ACCUMULATOR_INSTANCES + i) * 32;
            cd[off..off + 32].copy_from_slice(&be);
        }
        cd
    }
}

#[async_trait]
impl ProofAggregator for MockAggregator {
    async fn aggregate(
        &self,
        inner_snark: &Path,
        _verifier_name: &str,
    ) -> Result<Vec<u8>, RelayerError> {
        if self.fail {
            return Err(RelayerError::other("mock aggregator configured to fail"));
        }
        // Derive the instances from the sibling `<name>.instances.bin` so the
        // mock stays consistent with MockCircuit4SnarkProver.
        let instances_path = inner_snark.with_extension("instances.bin");
        let instances_hex = read_instances_le(&instances_path).unwrap_or_else(|_| {
            (0..WITHDRAWAL_PUBLIC_INPUTS as u8)
                .map(|i| {
                    let mut le = [0u8; 32];
                    le[0] = i;
                    hex::encode(le)
                })
                .collect()
        });
        Ok(Self::calldata_binding(&instances_hex))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subprocess_snark_args_are_stable() {
        let cfg = SubprocessCircuit4SnarkProverConfig::new("/orch", "/params");
        let prover = SubprocessCircuit4SnarkProver {
            config: cfg,
        };
        let args = prover.args(
            Path::new("/w/witness.json"),
            Path::new("/tmp/snarks"),
            "circuit4",
        );
        assert_eq!(args, vec![
            "--params-dir",
            "/params",
            "--snark-dir",
            "/tmp/snarks",
            "--name",
            "circuit4",
            "--fixture",
            "/w/witness.json",
        ]);
    }

    #[test]
    fn subprocess_aggregate_args_are_stable() {
        let cfg = SubprocessAggregatorConfig::new("/agg", "/verifiers", "/params");
        let agg = SubprocessAggregator {
            config: cfg,
        };
        let args = agg.args(
            Path::new("/s/circuit4.snark"),
            WITHDRAWAL_VERIFIER_NAME,
            Path::new("/tmp/out.bin"),
        );
        assert_eq!(args, vec![
            "--inner-snark",
            "/s/circuit4.snark",
            "--name",
            WITHDRAWAL_VERIFIER_NAME,
            "--out",
            "/tmp/out.bin",
            "--verifiers-dir",
            "/verifiers",
        ]);
    }

    #[test]
    fn read_instances_le_roundtrip_and_length_check() {
        let dir = std::env::temp_dir().join(format!("agg_inst_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("circuit4.instances.bin");
        let mut bytes = Vec::new();
        for i in 0..WITHDRAWAL_PUBLIC_INPUTS as u8 {
            let mut le = [0u8; 32];
            le[0] = i + 1;
            bytes.extend_from_slice(&le);
        }
        std::fs::write(&good, &bytes).unwrap();
        let hexes = read_instances_le(&good).unwrap();
        assert_eq!(hexes.len(), WITHDRAWAL_PUBLIC_INPUTS);
        assert_eq!(fr_hex_to_u256(&hexes[0]).unwrap(), U256::from(1u64));

        let bad = dir.join("bad.instances.bin");
        std::fs::write(&bad, [0u8; 64]).unwrap();
        assert!(read_instances_le(&bad).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn calldata_binds_matches_and_detects_mismatch() {
        let instances: Vec<String> = (0..WITHDRAWAL_PUBLIC_INPUTS as u8)
            .map(|i| {
                let mut le = [0u8; 32];
                le[0] = i + 1;
                hex::encode(le)
            })
            .collect();
        let cd = MockAggregator::calldata_binding(&instances);
        assert!(calldata_binds_instances(&cd, &instances).is_ok());

        // Corrupt one bound word → mismatch detected.
        let mut bad = cd.clone();
        let off = (NUM_ACCUMULATOR_INSTANCES + 3) * 32;
        bad[off + 31] ^= 0xFF;
        assert!(calldata_binds_instances(&bad, &instances).is_err());

        // Too-short calldata → error.
        assert!(calldata_binds_instances(&[0u8; 100], &instances).is_err());
    }

    #[tokio::test]
    async fn mock_pipeline_produces_submit_shaped_proof() {
        let dir = std::env::temp_dir().join(format!("agg_pipe_{}", std::process::id()));
        let pipeline = Circuit4ShplonkPipeline::new(
            MockCircuit4SnarkProver::default(),
            MockAggregator::default(),
        );
        let witness = dir.join("witness.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&witness, b"{}").unwrap();

        let proof = pipeline.prove(&witness, &dir, 42).await.unwrap();
        assert_eq!(proof.seq_no, 42);
        assert!(proof.self_verified);
        assert_eq!(proof.public_instances_hex.len(), WITHDRAWAL_PUBLIC_INPUTS);
        // proof_hex is the aggregator calldata (>= SHPLONK min).
        let bytes = proof.proof_bytes().unwrap();
        assert!(bytes.len() >= SHPLONK_MIN_WITHDRAWAL_INSTANCES);
        // The ten public inputs decode into a well-formed struct.
        let pi = proof.public_inputs().unwrap();
        assert_eq!(pi.token_id, U256::ZERO); // MockCircuit4SnarkProver: instance[0]=0
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn mock_pipeline_propagates_prover_failure() {
        let dir = std::env::temp_dir().join(format!("agg_pipe_fail_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let witness = dir.join("w.json");
        std::fs::write(&witness, b"{}").unwrap();
        let pipeline = Circuit4ShplonkPipeline::new(
            MockCircuit4SnarkProver {
                fail: true,
            },
            MockAggregator::default(),
        );
        assert!(pipeline.prove(&witness, &dir, 0).await.is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
