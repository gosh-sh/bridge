//! [`WithdrawalProver`] — drive the partner's Circuit 4 (withdrawal) prover
//! library from the relayer.
//!
//! The withdrawal circuit lives in `crates/an-bridge-prover`'s
//! `bridge-event-prover-lib`, whose cargo workspace pins the gosh `halo2-lib`
//! fork + `tvm_vm` — a dependency tree incompatible with this relayer crate's
//! alloy / `abigen` stack. Force-merging two halo2 backends into one binary
//! would also bloat the relayer and slow CI. So, exactly like
//! `deposit-relayer-daemon` consumes `deposit-prover`, proof generation stays
//! behind a trait with two backends:
//!
//! - [`MockWithdrawalProver`] — deterministic, no halo2; returns a canned
//!   [`PartnerWithdrawalProof`] so the relayer + submit path can be driven in
//!   unit tests in microseconds.
//! - [`SubprocessWithdrawalProver`] — production. Shells out to
//!   `an-bridge-prover`'s `bridge-event-halo2-prover` binary (`--fixture
//!   <witness> [--out-dir <dir>] [--seq-no N]`), which proves the
//!   `PrivateWitness`, **self-verifies**, optionally writes
//!   `proof_event_{seq:06}.json`, and prints a single-line JSON summary (the
//!   exact `proof_event` schema) as the last line of stdout. We parse that
//!   summary back into a [`PartnerWithdrawalProof`].
//!
//! ### Proof format note
//!
//! The summary carries the **raw Halo2 SHPLONK aggregator calldata**
//! (`proof_hex` = `instances ‖ proof`, ≥
//! [`crate::withdrawal::SHPLONK_MIN_WITHDRAWAL_INSTANCES`] bytes). That is the
//! artefact the R15 SHPLONK `BridgeWithdrawalAggregatorVerifier` consumes on
//! chain. `prove-withdraw` only *produces* the proof_event — submission is a
//! deliberate second step.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use async_trait::async_trait;

use crate::{error::RelayerError, withdrawal::PartnerWithdrawalProof};

/// Name of the partner binary inside the `an-bridge-prover` workspace.
pub const PROVER_BIN: &str = "bridge-event-halo2-prover";

/// Produces a [`PartnerWithdrawalProof`] for a Circuit 4 `PrivateWitness`.
#[async_trait]
pub trait WithdrawalProver: Send + Sync {
    /// Prove the `PrivateWitness` JSON at `witness_path` and return the parsed
    /// proof_event summary.
    async fn prove(&self, witness_path: &Path) -> Result<PartnerWithdrawalProof, RelayerError>;
}

// ─────────────────────────────────────────────────────────────────────
// MockWithdrawalProver — deterministic, no halo2
// ─────────────────────────────────────────────────────────────────────

/// Deterministic withdrawal prover for tests. Returns a canned, self-verified
/// [`PartnerWithdrawalProof`] (SHPLONK-shaped proof bytes + ten 32-byte LE
/// public inputs) so the submit path is exercised end-to-end without running
/// halo2.
#[derive(Clone, Debug)]
pub struct MockWithdrawalProver {
    canned: PartnerWithdrawalProof,
    /// When true, `prove` fails — lets tests exercise the prover-error path.
    fail: bool,
}

impl MockWithdrawalProver {
    /// A valid canned proof: ten ascending public inputs and a SHPLONK-shaped
    /// proof blob long enough to pass [`PartnerWithdrawalProof::proof_bytes`].
    pub fn valid() -> Self {
        let public_instances_hex = (0u8..10)
            .map(|i| {
                let mut le = [0u8; 32];
                le[0] = i;
                hex::encode(le)
            })
            .collect();
        Self {
            canned: PartnerWithdrawalProof {
                schema_version: 1,
                seq_no: 0,
                proof_hex: hex::encode(vec![
                    0xAAu8;
                    crate::withdrawal::SHPLONK_MIN_WITHDRAWAL_INSTANCES + 3200
                ]),
                public_instances_hex,
                self_verified: true,
            },
            fail: false,
        }
    }

    /// Override the canned proof returned by
    /// [`prove`](WithdrawalProver::prove).
    pub fn with_canned(canned: PartnerWithdrawalProof) -> Self {
        Self {
            canned,
            fail: false,
        }
    }

    /// A prover that always fails.
    pub fn failing() -> Self {
        let mut m = Self::valid();
        m.fail = true;
        m
    }
}

#[async_trait]
impl WithdrawalProver for MockWithdrawalProver {
    async fn prove(&self, _witness_path: &Path) -> Result<PartnerWithdrawalProof, RelayerError> {
        if self.fail {
            return Err(RelayerError::other(
                "mock withdrawal prover configured to fail",
            ));
        }
        Ok(self.canned.clone())
    }
}

// ─────────────────────────────────────────────────────────────────────
// SubprocessWithdrawalProver — invokes an-bridge-prover's binary
// ─────────────────────────────────────────────────────────────────────

/// Configuration for the out-of-process `bridge-event-halo2-prover` invocation.
#[derive(Clone, Debug)]
pub struct SubprocessWithdrawalProverConfig {
    /// Path to the `crates/an-bridge-prover` workspace root. The prebuilt
    /// release binary is expected at
    /// `<dir>/target/release/bridge-event-halo2-prover`; if absent we fall back
    /// to `cargo run --release -p bridge-event-halo2-prover`.
    pub an_bridge_prover_dir: PathBuf,
    /// Working directory for the prover process. The binary reads the SRS +
    /// Circuit 4 PK/VK from `./params` **relative to this dir**. Defaults to
    /// `an_bridge_prover_dir`. (Only honoured for the prebuilt-binary path;
    /// the `cargo run` fallback always runs from `an_bridge_prover_dir`.)
    pub work_dir: PathBuf,
    /// Optional dir to also persist `proof_event_{seq:06}.json` (passed as
    /// `--out-dir`). The proof is returned regardless via stdout.
    pub out_dir: Option<PathBuf>,
    /// Seqno stamped into the summary / output filename.
    pub seq_no: u32,
    /// Hard timeout for the whole proving run. Circuit 4 keygen + prove from a
    /// cold PK can take minutes, so default generously.
    pub timeout: Duration,
}

impl SubprocessWithdrawalProverConfig {
    pub fn new(an_bridge_prover_dir: impl Into<PathBuf>) -> Self {
        let dir = an_bridge_prover_dir.into();
        Self {
            work_dir: dir.clone(),
            an_bridge_prover_dir: dir,
            out_dir: None,
            seq_no: 0,
            timeout: Duration::from_secs(1800),
        }
    }
}

/// Production withdrawal prover. Shells out to the partner Circuit 4 binary and
/// parses its JSON summary back into a [`PartnerWithdrawalProof`].
pub struct SubprocessWithdrawalProver {
    config: SubprocessWithdrawalProverConfig,
}

impl SubprocessWithdrawalProver {
    pub fn new(mut config: SubprocessWithdrawalProverConfig) -> Self {
        // Subprocess spawn with `current_dir` needs absolute paths; operators
        // often pass relative dirs.
        if let Ok(abs) = config.an_bridge_prover_dir.canonicalize() {
            config.an_bridge_prover_dir = abs;
        }
        if let Ok(abs) = config.work_dir.canonicalize() {
            config.work_dir = abs;
        }
        Self {
            config,
        }
    }

    fn release_bin(&self) -> Option<PathBuf> {
        let bin = self
            .config
            .an_bridge_prover_dir
            .join("target/release")
            .join(PROVER_BIN);
        bin.is_file().then_some(bin)
    }

    /// Build the argv passed to the prover (program excluded). Pulled out so
    /// the flag construction is unit-testable without spawning halo2.
    fn prover_args(&self, witness_path: &Path) -> Vec<String> {
        let mut args = vec![
            "--fixture".to_string(),
            witness_path.display().to_string(),
            "--seq-no".to_string(),
            self.config.seq_no.to_string(),
        ];
        if let Some(dir) = &self.config.out_dir {
            args.push("--out-dir".to_string());
            args.push(dir.display().to_string());
        }
        args
    }
}

#[async_trait]
impl WithdrawalProver for SubprocessWithdrawalProver {
    async fn prove(&self, witness_path: &Path) -> Result<PartnerWithdrawalProof, RelayerError> {
        use tokio::process::Command;

        let args = self.prover_args(witness_path);

        let mut cmd = if let Some(bin) = self.release_bin() {
            let mut c = Command::new(bin);
            c.current_dir(&self.config.work_dir).args(&args);
            c
        } else {
            let mut c = Command::new("cargo");
            c.current_dir(&self.config.an_bridge_prover_dir)
                .args(["run", "--release", "-p", PROVER_BIN, "--"])
                .args(&args);
            c
        };
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let output = tokio::time::timeout(self.config.timeout, cmd.output())
            .await
            .map_err(|_| {
                RelayerError::other(format!(
                    "{PROVER_BIN} timed out after {:?}",
                    self.config.timeout
                ))
            })?
            .map_err(|e| RelayerError::other(format!("failed to spawn {PROVER_BIN}: {e}")))?;

        if !output.status.success() {
            return Err(RelayerError::other(format!(
                "{PROVER_BIN} exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        // The summary is the last non-empty line of stdout (stderr carries logs).
        let stdout = String::from_utf8_lossy(&output.stdout);
        let summary = stdout
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .ok_or_else(|| {
                RelayerError::other(format!("{PROVER_BIN} produced no stdout summary"))
            })?;

        let proof = PartnerWithdrawalProof::from_json_bytes(summary.as_bytes())?;
        if !proof.self_verified {
            return Err(RelayerError::other(format!(
                "{PROVER_BIN} reported self_verified=false — refusing to surface an unverified \
                 proof"
            )));
        }
        Ok(proof)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_returns_canned_self_verified_proof() {
        let prover = MockWithdrawalProver::valid();
        let proof = prover
            .prove(Path::new("/does/not/matter.json"))
            .await
            .unwrap();
        assert!(proof.self_verified);
        assert_eq!(proof.public_instances_hex.len(), 10);
        // Canned proof is submit-shaped (SHPLONK aggregator calldata).
        assert!(proof.proof_bytes().is_ok());
        let pi = proof.public_inputs().unwrap();
        assert_eq!(pi.final_root, alloy::primitives::U256::from(9u64));
    }

    #[tokio::test]
    async fn mock_failing_surfaces_error() {
        let prover = MockWithdrawalProver::failing();
        assert!(prover.prove(Path::new("/x.json")).await.is_err());
    }

    #[test]
    fn subprocess_args_carry_fixture_seqno_and_outdir() {
        let mut cfg = SubprocessWithdrawalProverConfig::new(".");
        cfg.seq_no = 7;
        cfg.out_dir = Some(PathBuf::from("/tmp/out"));
        let prover = SubprocessWithdrawalProver::new(cfg);
        let args = prover.prover_args(Path::new("/w/witness.json"));
        assert_eq!(args, vec![
            "--fixture".to_string(),
            "/w/witness.json".to_string(),
            "--seq-no".to_string(),
            "7".to_string(),
            "--out-dir".to_string(),
            "/tmp/out".to_string(),
        ]);
    }

    #[test]
    fn subprocess_args_omit_outdir_when_unset() {
        let prover = SubprocessWithdrawalProver::new(SubprocessWithdrawalProverConfig::new("."));
        let args = prover.prover_args(Path::new("w.json"));
        assert_eq!(args, vec!["--fixture", "w.json", "--seq-no", "0"]);
    }
}
