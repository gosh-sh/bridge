//! Step proof generation: mock (tests / `--mock-prove`) or a subprocess into
//! `eth-light-client-prover`'s `export_step_vk_blob`. The rotate is
//! [`crate::rotate`].

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use async_trait::async_trait;
use tokio::process::Command;

use crate::{
    error::RelayerError,
    types::{pack_step_public_inputs, FinalityUpdate, StepProofBundle},
};

#[async_trait]
pub trait ProofGenerator: Send + Sync {
    async fn generate_step(&self, update: &FinalityUpdate)
        -> Result<StepProofBundle, RelayerError>;
}

#[derive(Clone, Debug, Default)]
pub struct MockProofGenerator {
    pub fail_step: bool,
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
}

#[derive(Clone, Debug)]
pub struct SubprocessProverConfig {
    /// The step prover: a prebuilt `export_step_vk_blob` example
    /// (`cargo build --release --example export_step_vk_blob` in
    /// `eth-light-client-prover/`).
    pub step_prover_bin: PathBuf,
    /// Working directory of the step prover; a relative `srs_path` resolves
    /// against it. `None` runs it in the directory its binary is in.
    pub prover_dir: Option<PathBuf>,
    pub srs_path: PathBuf,
    pub timeout: Duration,
    /// Where each prove gets its directory of inputs and outputs, removed
    /// once the bundle is read back. Never the system temp directory
    /// (bridge-audit#132).
    pub work_dir: PathBuf,
    /// Where the prover transcripts go, one pair of files per run
    /// (`step-<attested_slot>-<ts>-{stdout,stderr}.log`); the error message
    /// still carries the stderr tail.
    pub log_dir: PathBuf,
}

impl SubprocessProverConfig {
    /// The directory `bin` runs in: `prover_dir`, else the binary's own.
    fn work_dir_for(&self, bin: &Path) -> PathBuf {
        if let Some(dir) = &self.prover_dir {
            return dir.clone();
        }
        match bin.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
            _ => PathBuf::from("."),
        }
    }

    /// Where the step prover reads the SRS: `srs_path` as is when absolute,
    /// else under the directory the prover runs in.
    pub fn step_srs_path(&self) -> PathBuf {
        if self.srs_path.is_absolute() {
            self.srs_path.clone()
        } else {
            self.work_dir_for(&self.step_prover_bin)
                .join(&self.srs_path)
        }
    }
}

/// `path` as an absolute path, refused unless it is an executable file, so
/// a daemon with a wrong `--step-prover-bin` stops at startup rather than at
/// its first proof hours later.
pub fn require_executable(path: &Path, what: &str) -> Result<PathBuf, RelayerError> {
    let refuse = |why: String| {
        RelayerError::ProofGeneration(format!("{what} binary {}: {why}", path.display()))
    };
    let meta = std::fs::metadata(path).map_err(|e| refuse(e.to_string()))?;
    if !meta.is_file() {
        return Err(refuse("not a file".into()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return Err(refuse("not executable (chmod +x)".into()));
        }
    }
    path.canonicalize().map_err(|e| refuse(e.to_string()))
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

    /// One directory per prove inside the work directory, gone with the
    /// returned guard; never the system temp directory.
    fn prove_dir(&self, prefix: &str) -> Result<tempfile::TempDir, RelayerError> {
        let fail = |e: std::io::Error| {
            RelayerError::ProofGeneration(format!(
                "work directory {}: {e}",
                self.cfg.work_dir.display()
            ))
        };
        std::fs::create_dir_all(&self.cfg.work_dir).map_err(fail)?;
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in(&self.cfg.work_dir)
            .map_err(fail)
    }

    /// The transcript of one prover run, both streams, in the log
    /// directory. Written even when the prover failed: that is when it is
    /// read.
    fn write_transcript(&self, stem: &str, output: &std::process::Output) {
        // A timestamp in the name: a retry of the same slot or period must
        // not overwrite the transcript of the failed run.
        let ts = unix_now();
        self.write_log(
            format!("{stem}-{ts}-stdout.log"),
            crate::redact::redact_urls(&String::from_utf8_lossy(&output.stdout)),
        );
        self.write_log(
            format!("{stem}-{ts}-stderr.log"),
            crate::redact::redact_urls(&String::from_utf8_lossy(&output.stderr)),
        );
    }

    /// Written where the transcript would be when the prover was killed on
    /// timeout and left no output.
    fn write_timeout_note(&self, stem: &str) {
        self.write_log(
            format!("{stem}-{}-timeout.log", unix_now()),
            format!(
                "killed after {:?} (timeout); no output was kept\n",
                self.cfg.timeout
            ),
        );
    }

    fn write_log(&self, name: String, text: String) {
        let path = self.cfg.log_dir.join(name);
        if let Err(e) =
            std::fs::create_dir_all(&self.cfg.log_dir).and_then(|()| std::fs::write(&path, text))
        {
            tracing::warn!(log = %path.display(), error = %e, "prover transcript not written");
        }
    }

    async fn run_step(&self, update: &FinalityUpdate) -> Result<StepProofBundle, RelayerError> {
        let work = self.prove_dir(&format!("step-{}-", update.attested_slot))?;
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

        let step_bin = &self.cfg.step_prover_bin;
        let mut cmd = Command::new(step_bin);
        cmd.current_dir(self.cfg.work_dir_for(step_bin))
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
            )
            .env(
                crate::types::BeaconChainParams::ENV_GLOAS_FORK_SLOT,
                chain.gloas_fork_slot.to_string(),
            );
        }
        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output();

        let output = tokio::time::timeout(self.cfg.timeout, output)
            .await
            .map_err(|_| {
                self.write_timeout_note(&format!("step-{}", update.attested_slot));
                RelayerError::ProofGeneration("step prove timed out".into())
            })?
            .map_err(|e| {
                RelayerError::ProofGeneration(format!(
                    "spawn {}: {e}",
                    self.cfg.step_prover_bin.display()
                ))
            })?;
        // The transcript outlives the prove directory, which is dropped with
        // the bundle; the error also carries the stderr tail.
        self.write_transcript(&format!("step-{}", update.attested_slot), &output);
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
            // Live prove fills committee_commitment in-circuit; keep
            // slots/hashes from the beacon update for logging.
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
}

#[async_trait]
impl ProofGenerator for SubprocessProofGenerator {
    async fn generate_step(
        &self,
        update: &FinalityUpdate,
    ) -> Result<StepProofBundle, RelayerError> {
        self.run_step(update).await
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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

    fn cfg(step: &Path, dir: Option<&Path>) -> SubprocessProverConfig {
        // The tests keep the fake prover in a temp directory of their own:
        // work and log directories go beside it.
        let root = step.parent().expect("the fake prover has a directory");
        SubprocessProverConfig {
            step_prover_bin: step.to_path_buf(),
            prover_dir: dir.map(Path::to_path_buf),
            srs_path: "srs.bin".into(),
            timeout: Duration::from_secs(10),
            work_dir: root.join("work-files"),
            log_dir: root.join("logs"),
        }
    }

    /// Put an executable `script` at `bin`. Copied there by `cp`, not
    /// written from this process: a test running beside this one may fork
    /// while this process holds the file open for writing, and the child
    /// holds it until it execs — exec'ing the script meanwhile fails with
    /// `ETXTBSY`.
    fn install_script(bin: &Path, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        let source = bin.with_extension("src");
        std::fs::write(&source, script).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Gone first: `cp` over an existing file keeps that file's mode.
        std::fs::remove_file(bin).ok();
        let copied = std::process::Command::new("cp")
            .arg(&source)
            .arg(bin)
            .status()
            .unwrap();
        assert!(copied.success());
    }

    fn update_with_committee() -> FinalityUpdate {
        let mut update = sample_update();
        update.committee_json = "[]".into();
        update
    }

    /// A step prover that records where it ran and what SRS it was told,
    /// then writes a bundle of the expected shape.
    fn fake_step_prover(dir: &Path, bin: &Path) {
        let (pi, _) = pack_step_public_inputs(&update_with_committee(), [0u8; 32]);
        install_script(
            bin,
            &format!(
                "#!/bin/sh\npwd -P > {cwd}\nprintf '%s' \"$STEP_SRS_PATH\" > {srs}\nhead -c {n} \
                 /dev/zero > \"$STEP_OUT_DIR/step_public_inputs.bin\"\nprintf proof > \
                 \"$STEP_OUT_DIR/step_proof_blake2b.bin\"\n",
                cwd = dir.join("cwd").display(),
                srs = dir.join("srs").display(),
                n = pi.len(),
            ),
        );
    }

    #[tokio::test]
    async fn the_step_prover_is_the_configured_binary_run_in_the_prover_dir() {
        // The daemon executes the path it was given, in `prover_dir`, and
        // reads the bundle back from STEP_OUT_DIR; there is no cargo.
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let bin = dir.path().join("step-prover.sh");
        fake_step_prover(dir.path(), &bin);
        let gen = SubprocessProofGenerator::new(cfg(&bin, Some(&work)));
        let bundle = gen.generate_step(&update_with_committee()).await.unwrap();
        assert_eq!(bundle.proof, b"proof");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("cwd"))
                .unwrap()
                .trim(),
            work.canonicalize().unwrap().to_str().unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("srs")).unwrap(),
            "srs.bin"
        );
    }

    #[tokio::test]
    async fn without_a_prover_dir_the_step_prover_runs_in_its_own_directory() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin = bin_dir.join("step-prover.sh");
        fake_step_prover(dir.path(), &bin);
        let gen = SubprocessProofGenerator::new(cfg(&bin, None));
        gen.generate_step(&update_with_committee()).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("cwd"))
                .unwrap()
                .trim(),
            bin_dir.canonicalize().unwrap().to_str().unwrap()
        );
    }

    #[tokio::test]
    async fn a_failing_step_prover_is_named_with_its_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("step-prover.sh");
        install_script(&bin, "#!/bin/sh\necho 'no SRS here' >&2\nexit 3\n");
        let gen = SubprocessProofGenerator::new(cfg(&bin, None));
        let err = gen
            .generate_step(&update_with_committee())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("export_step_vk_blob exited"), "{err}");
        assert!(err.contains("no SRS here"), "{err}");
    }

    #[test]
    fn the_srs_is_read_where_the_step_prover_runs() {
        let step = Path::new("/opt/provers/export_step_vk_blob");
        let relative = cfg(step, Some(Path::new("/work")));
        assert_eq!(relative.step_srs_path(), PathBuf::from("/work/srs.bin"));
        let in_bin_dir = cfg(step, None);
        assert_eq!(
            in_bin_dir.step_srs_path(),
            PathBuf::from("/opt/provers/srs.bin")
        );
        let mut absolute = cfg(step, Some(Path::new("/work")));
        absolute.srs_path = "/srs/kzg_bn254_19.srs".into();
        assert_eq!(
            absolute.step_srs_path(),
            PathBuf::from("/srs/kzg_bn254_19.srs")
        );
    }

    #[test]
    fn require_executable_refuses_what_cannot_run() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let err = require_executable(&missing, "step prover")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("step prover binary") && err.contains("missing"),
            "{err}"
        );
        let err = require_executable(dir.path(), "step prover")
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a file"), "{err}");
        let plain = dir.path().join("plain");
        std::fs::write(&plain, "x").unwrap();
        let err = require_executable(&plain, "step prover")
            .unwrap_err()
            .to_string();
        assert!(err.contains("not executable"), "{err}");
        install_script(&plain, "#!/bin/sh\n");
        assert_eq!(
            require_executable(&plain, "step prover").unwrap(),
            plain.canonicalize().unwrap()
        );
    }

    #[tokio::test]
    async fn subprocess_step_rejects_empty_committee() {
        let gen = SubprocessProofGenerator::new(cfg(Path::new("step"), Some(Path::new("."))));
        let err = gen.generate_step(&sample_update()).await.unwrap_err();
        match err {
            RelayerError::ProofGeneration(m) => {
                assert!(m.contains("committee_json empty"), "{m}");
            },
            other => panic!("{other:?}"),
        }
    }
}
