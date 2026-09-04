//! Proof generation: mock (tests / `--mock-prove`) or subprocess into
//! `eth-light-client-prover` examples.

use std::{path::PathBuf, process::Stdio, time::Duration};

use async_trait::async_trait;
use tokio::process::Command;

use crate::{
    error::RelayerError,
    types::{
        pack_step_public_inputs, FinalityUpdate, RotateProofBundle, StepProofBundle,
        ROTATE_INSTANCE_LEN,
    },
};

#[async_trait]
pub trait ProofGenerator: Send + Sync {
    async fn generate_step(&self, update: &FinalityUpdate)
        -> Result<StepProofBundle, RelayerError>;

    async fn generate_rotate(&self, to_period: u64) -> Result<RotateProofBundle, RelayerError>;
}

#[derive(Clone, Debug, Default)]
pub struct MockProofGenerator {
    pub fail_step: bool,
    pub fail_rotate: bool,
    pub committee_commitment: [u8; 32],
}

impl MockProofGenerator {
    pub fn new() -> Self {
        Self {
            committee_commitment: [0xC0; 32],
            ..Self::default()
        }
    }
}

#[async_trait]
impl ProofGenerator for MockProofGenerator {
    async fn generate_step(
        &self,
        update: &FinalityUpdate,
    ) -> Result<StepProofBundle, RelayerError> {
        if self.fail_step {
            return Err(RelayerError::ProofGeneration(
                "mock configured to fail step".into(),
            ));
        }
        let (public_inputs, parsed) = pack_step_public_inputs(update, self.committee_commitment);
        Ok(StepProofBundle {
            public_inputs,
            proof: vec![0xAB; 64],
            parsed,
        })
    }

    async fn generate_rotate(&self, to_period: u64) -> Result<RotateProofBundle, RelayerError> {
        if self.fail_rotate {
            return Err(RelayerError::ProofGeneration(
                "mock configured to fail rotate".into(),
            ));
        }
        let mut public_inputs = vec![0u8; ROTATE_INSTANCE_LEN * 32];
        let off = (ROTATE_INSTANCE_LEN - 1) * 32;
        public_inputs[off..off + 8].copy_from_slice(&to_period.to_le_bytes());
        Ok(RotateProofBundle {
            public_inputs,
            proof: vec![0xCD; 64],
            period: to_period,
        })
    }
}

#[derive(Clone, Debug)]
pub struct SubprocessProverConfig {
    pub prover_dir: PathBuf,
    pub srs_path: PathBuf,
    pub timeout: Duration,
}

pub struct SubprocessProofGenerator {
    cfg: SubprocessProverConfig,
}

impl SubprocessProofGenerator {
    pub fn new(cfg: SubprocessProverConfig) -> Self {
        Self {
            cfg,
        }
    }

    async fn run_step(&self, update: &FinalityUpdate) -> Result<StepProofBundle, RelayerError> {
        let work = tempfile::tempdir()
            .map_err(|e| RelayerError::ProofGeneration(format!("tempdir: {e}")))?;
        let json_path = work.path().join("finality_update.json");
        std::fs::write(&json_path, &update.raw_json)?;
        if update.committee_json.is_empty() {
            return Err(RelayerError::ProofGeneration(
                "committee_json empty — need light_client/updates (not a synthetic committee)"
                    .into(),
            ));
        }
        let committee_path = work.path().join("committee.json");
        std::fs::write(&committee_path, &update.committee_json)?;
        let out_dir = work.path().join("out");
        std::fs::create_dir_all(&out_dir)?;

        let mut cmd = Command::new("cargo");
        cmd.current_dir(&self.cfg.prover_dir)
            .args(["run", "--release", "--example", "export_step_vk_blob"])
            .env("FINALITY_UPDATE_PATH", &json_path)
            .env("STEP_OUT_DIR", &out_dir)
            .env("STEP_SRS_PATH", &self.cfg.srs_path)
            .env("COMMITTEE_JSON_PATH", &committee_path);
        // Signing domain resolved by the beacon source (fork at
        // `signature_slot`, network genesis root). Without it the prover
        // falls back to its own env / mainnet default.
        if let Some(chain) = &update.chain {
            cmd.env(
                crate::types::BeaconChainParams::ENV_FORK_VERSION,
                chain.fork_version_hex(),
            )
            .env(
                crate::types::BeaconChainParams::ENV_GENESIS_VALIDATORS_ROOT,
                chain.genesis_validators_root_hex(),
            );
        }
        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output();

        let output = tokio::time::timeout(self.cfg.timeout, output)
            .await
            .map_err(|_| RelayerError::ProofGeneration("step prove timed out".into()))?
            .map_err(|e| RelayerError::ProofGeneration(format!("spawn cargo: {e}")))?;
        // Keep the prover transcript next to the bundle: `prove-one` copies the
        // bundle out, the daemon's tempdir is dropped, so the error also
        // carries the stderr tail.
        let _ = std::fs::write(out_dir.join("prover-stdout.log"), &output.stdout);
        let _ = std::fs::write(out_dir.join("prover-stderr.log"), &output.stderr);
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let tail: String = stderr
                .lines()
                .rev()
                .take(12)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            return Err(RelayerError::ProofGeneration(format!(
                "export_step_vk_blob exited {}; stderr tail:\n{tail}",
                output.status
            )));
        }

        let pi = std::fs::read(out_dir.join("step_public_inputs.bin"))
            .map_err(|e| RelayerError::ProofGeneration(format!("read public_inputs: {e}")))?;
        let proof = std::fs::read(out_dir.join("step_proof_blake2b.bin"))
            .map_err(|e| RelayerError::ProofGeneration(format!("read proof: {e}")))?;
        let (expected, parsed) = pack_step_public_inputs(update, [0u8; 32]);
        let parsed = if pi.len() == expected.len() {
            // Live prove fills committee_commitment in-circuit; keep slots/hashes
            // from the beacon update for logging.
            crate::types::StepPublicInputs {
                committee_commitment: {
                    let mut c = [0u8; 32];
                    c.copy_from_slice(&pi[5 * 32..6 * 32]);
                    c
                },
                ..parsed
            }
        } else {
            parsed
        };
        Ok(StepProofBundle {
            public_inputs: pi,
            proof,
            parsed,
        })
    }

    async fn run_rotate(&self, to_period: u64) -> Result<RotateProofBundle, RelayerError> {
        let work = tempfile::tempdir()
            .map_err(|e| RelayerError::ProofGeneration(format!("tempdir: {e}")))?;
        let tree_out = work.path().join("rotate_tree");
        std::fs::create_dir_all(&tree_out)?;
        let status = Command::new("cargo")
            .current_dir(&self.cfg.prover_dir)
            .args([
                "run",
                "--release",
                "--features",
                "aggregation",
                "--example",
                "rotate_tree_n8",
            ])
            .env("EMIT_VKBLOB", "1")
            .env("TREE_OUT", &tree_out)
            .env("STEP_SRS", &self.cfg.srs_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .status();
        let child = tokio::time::timeout(self.cfg.timeout, status)
            .await
            .map_err(|_| RelayerError::ProofGeneration("rotate prove timed out".into()))?
            .map_err(|e| RelayerError::ProofGeneration(format!("spawn cargo: {e}")))?;
        if !child.success() {
            return Err(RelayerError::ProofGeneration(format!(
                "rotate_tree_n8 exited {child}"
            )));
        }
        let mut bundle = RotateProofBundle::from_dir(&tree_out)?;
        bundle.period = to_period;
        Ok(bundle)
    }
}

#[async_trait]
impl ProofGenerator for SubprocessProofGenerator {
    async fn generate_step(
        &self,
        update: &FinalityUpdate,
    ) -> Result<StepProofBundle, RelayerError> {
        self.run_step(update).await
    }

    async fn generate_rotate(&self, to_period: u64) -> Result<RotateProofBundle, RelayerError> {
        self.run_rotate(to_period).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::parse_finality_update;

    fn sample_update() -> FinalityUpdate {
        let json = r#"{"data":{
          "attested_header":{"beacon":{"slot":"5","state_root":"0x1111111111111111111111111111111111111111111111111111111111111111"}},
          "finalized_header":{
            "beacon":{"slot":"4","state_root":"0x1111111111111111111111111111111111111111111111111111111111111111"},
            "execution":{"block_hash":"0x2222222222222222222222222222222222222222222222222222222222222222"}
          },
          "sync_aggregate":{"sync_committee_bits":"0x01"}
        }}"#;
        parse_finality_update(json).unwrap()
    }

    #[tokio::test]
    async fn subprocess_step_rejects_empty_committee() {
        let gen = SubprocessProofGenerator::new(SubprocessProverConfig {
            prover_dir: ".".into(),
            srs_path: ".".into(),
            timeout: Duration::from_secs(1),
        });
        let err = gen.generate_step(&sample_update()).await.unwrap_err();
        match err {
            RelayerError::ProofGeneration(m) => {
                assert!(m.contains("committee_json empty"), "{m}");
            },
            other => panic!("{other:?}"),
        }
    }
}
