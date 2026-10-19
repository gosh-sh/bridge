//! Proof generation: mock (tests / `--mock-prove`) or subprocess into
//! `eth-light-client-prover` examples.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use async_trait::async_trait;
use tokio::process::Command;

use crate::{
    error::RelayerError,
    types::{
        pack_step_public_inputs, FinalityUpdate, RotateProofBundle, StepProofBundle,
        ROTATE_INSTANCE_LEN, ROTATE_PERIOD_INDEX,
    },
};

#[async_trait]
pub trait ProofGenerator: Send + Sync {
    async fn generate_step(&self, update: &FinalityUpdate)
        -> Result<StepProofBundle, RelayerError>;

    async fn generate_rotate(
        &self,
        to_period: u64,
        chain: Option<crate::types::BeaconChainParams>,
    ) -> Result<RotateProofBundle, RelayerError>;
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

    async fn generate_rotate(
        &self,
        to_period: u64,
        chain: Option<crate::types::BeaconChainParams>,
    ) -> Result<RotateProofBundle, RelayerError> {
        if self.fail_rotate {
            return Err(RelayerError::ProofGeneration(
                "mock configured to fail rotate".into(),
            ));
        }
        let mut public_inputs = vec![0u8; ROTATE_INSTANCE_LEN * 32];
        let off = ROTATE_PERIOD_INDEX * 32;
        public_inputs[off..off + 8].copy_from_slice(&to_period.to_le_bytes());
        if let Some(chain) = chain {
            let fork_off = crate::types::ROTATE_GLOAS_FORK_SLOT_INDEX * 32;
            public_inputs[fork_off..fork_off + 8]
                .copy_from_slice(&chain.gloas_fork_slot.to_le_bytes());
        }
        Ok(RotateProofBundle {
            public_inputs,
            proof: vec![0xCD; 64],
            period: to_period,
        })
    }
}

#[derive(Clone, Debug)]
pub struct SubprocessProverConfig {
    /// The step prover: a prebuilt `export_step_vk_blob` example
    /// (`cargo build --release --example export_step_vk_blob` in
    /// `eth-light-client-prover/`).
    pub step_prover_bin: PathBuf,
    /// The rotate prover: a prebuilt `rotate_tree_n8` example (built with
    /// `--features aggregation`). `None` when the daemon runs `--no-rotate`;
    /// a period jump then fails instead of spawning anything.
    pub rotate_prover_bin: Option<PathBuf>,
    /// Working directory of both provers. `rotate_tree_n8` reads its shard
    /// snarks (`out/shard_snark`) and the outer SRS (`../params/`) there, and
    /// the period check before it reads `fixtures/mainnet/` there, so a
    /// daemon that may rotate needs it; a relative `srs_path` resolves
    /// against it. `None` runs each prover in the directory its binary is
    /// in.
    pub prover_dir: Option<PathBuf>,
    pub srs_path: PathBuf,
    pub timeout: Duration,
    /// Where to keep the prover transcript
    /// (`prover-<attested_slot>-{stdout,stderr}.log`). `None` leaves it in
    /// the per-run temp dir (lost after the run); the error message still
    /// carries the stderr tail.
    pub log_dir: Option<PathBuf>,
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
            .map_err(|_| RelayerError::ProofGeneration("step prove timed out".into()))?
            .map_err(|e| {
                RelayerError::ProofGeneration(format!(
                    "spawn {}: {e}",
                    self.cfg.step_prover_bin.display()
                ))
            })?;
        // Keep the prover transcript next to the bundle: `prove-one` copies the
        // bundle out, the daemon's tempdir is dropped, so the error also
        // carries the stderr tail.
        let log_dir = match &self.cfg.log_dir {
            Some(d) => {
                let _ = std::fs::create_dir_all(d);
                d.clone()
            },
            None => out_dir.clone(),
        };
        let stem = format!("prover-{}", update.attested_slot);
        let _ = std::fs::write(log_dir.join(format!("{stem}-stdout.log")), &output.stdout);
        let _ = std::fs::write(log_dir.join(format!("{stem}-stderr.log")), &output.stderr);
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

    async fn run_rotate(
        &self,
        to_period: u64,
        chain: Option<crate::types::BeaconChainParams>,
    ) -> Result<RotateProofBundle, RelayerError> {
        let rotate_bin = self.cfg.rotate_prover_bin.as_deref();
        let fixtures_dir = match rotate_bin {
            Some(bin) => self.cfg.work_dir_for(bin),
            None => self
                .cfg
                .prover_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from(".")),
        };
        let fixture_period = fixture_rotate_period(&fixtures_dir);
        if fixture_period != to_period {
            return Err(RelayerError::ProofGeneration(format!(
                "rotate_tree_n8 proves the committed fixture (period {fixture_period}), not live \
                 period {to_period}; prove the hand-off on n14 and submit-rotate"
            )));
        }
        let work = tempfile::tempdir()
            .map_err(|e| RelayerError::ProofGeneration(format!("tempdir: {e}")))?;
        let tree_out = work.path().join("rotate_tree");
        std::fs::create_dir_all(&tree_out)?;
        let rotate_bin = rotate_bin.ok_or_else(|| {
            RelayerError::ProofGeneration(
                "no rotate prover: pass --rotate-prover-bin (LIGHT_CLIENT_ROTATE_PROVER_BIN) or \
                 run with --no-rotate"
                    .into(),
            )
        })?;
        let mut cmd = Command::new(rotate_bin);
        cmd.current_dir(self.cfg.work_dir_for(rotate_bin))
            .env("EMIT_VKBLOB", "1")
            .env("TREE_OUT", &tree_out)
            .env("STEP_SRS", &self.cfg.srs_path);
        if let Some(chain) = &chain {
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
        let status = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .status();
        let child = tokio::time::timeout(self.cfg.timeout, status)
            .await
            .map_err(|_| RelayerError::ProofGeneration("rotate prove timed out".into()))?
            .map_err(|e| {
                RelayerError::ProofGeneration(format!("spawn {}: {e}", rotate_bin.display()))
            })?;
        if !child.success() {
            return Err(RelayerError::ProofGeneration(format!(
                "rotate_tree_n8 exited {child}"
            )));
        }
        let bundle = RotateProofBundle::from_dir(&tree_out)?;
        if bundle.period != to_period {
            return Err(RelayerError::ProofGeneration(format!(
                "rotate proof is for period {}, not {to_period}",
                bundle.period
            )));
        }
        Ok(bundle)
    }
}

/// Period `rotate_tree_n8` will prove: attested slot of the first
/// `fixtures/mainnet/update_period_*.json`, else the synthetic committee
/// at slot `9412 × 8192` (period 9413). It does not take a live update.
pub(crate) fn fixture_rotate_period(prover_dir: &std::path::Path) -> u64 {
    const SYNTHETIC_PERIOD: u64 = 9413;
    let dir = prover_dir.join("fixtures/mainnet");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return SYNTHETIC_PERIOD;
    };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("update_period_") || !name.ends_with(".json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let data = if v.is_array() {
            &v[0]["data"]
        } else {
            &v["data"]
        };
        let slot = data["attested_header"]["beacon"]["slot"]
            .as_u64()
            .or_else(|| {
                data["attested_header"]["beacon"]["slot"]
                    .as_str()
                    .and_then(|s| s.parse().ok())
            });
        if let Some(slot) = slot {
            return slot / crate::types::SLOTS_PER_SYNC_PERIOD + 1;
        }
    }
    SYNTHETIC_PERIOD
}

#[async_trait]
impl ProofGenerator for SubprocessProofGenerator {
    async fn generate_step(
        &self,
        update: &FinalityUpdate,
    ) -> Result<StepProofBundle, RelayerError> {
        self.run_step(update).await
    }

    async fn generate_rotate(
        &self,
        to_period: u64,
        chain: Option<crate::types::BeaconChainParams>,
    ) -> Result<RotateProofBundle, RelayerError> {
        self.run_rotate(to_period, chain).await
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

    fn cfg(step: &Path, rotate: Option<&Path>, dir: Option<&Path>) -> SubprocessProverConfig {
        SubprocessProverConfig {
            step_prover_bin: step.to_path_buf(),
            rotate_prover_bin: rotate.map(Path::to_path_buf),
            prover_dir: dir.map(Path::to_path_buf),
            srs_path: "srs.bin".into(),
            timeout: Duration::from_secs(10),
            log_dir: None,
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
        let gen = SubprocessProofGenerator::new(cfg(&bin, None, Some(&work)));
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
        let gen = SubprocessProofGenerator::new(cfg(&bin, None, None));
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
        let gen = SubprocessProofGenerator::new(cfg(&bin, None, None));
        let err = gen
            .generate_step(&update_with_committee())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("export_step_vk_blob exited"), "{err}");
        assert!(err.contains("no SRS here"), "{err}");
    }

    #[tokio::test]
    async fn run_rotate_without_a_rotate_binary_names_the_flag() {
        let dir = tempfile::tempdir().unwrap();
        let gen = SubprocessProofGenerator::new(cfg(Path::new("step"), None, Some(dir.path())));
        let err = gen
            .generate_rotate(9413, None)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("--rotate-prover-bin"), "{err}");
    }

    #[test]
    fn the_srs_is_read_where_the_step_prover_runs() {
        let step = Path::new("/opt/provers/export_step_vk_blob");
        let relative = cfg(step, None, Some(Path::new("/work")));
        assert_eq!(relative.step_srs_path(), PathBuf::from("/work/srs.bin"));
        let in_bin_dir = cfg(step, None, None);
        assert_eq!(
            in_bin_dir.step_srs_path(),
            PathBuf::from("/opt/provers/srs.bin")
        );
        let mut absolute = cfg(step, None, Some(Path::new("/work")));
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
        let gen = SubprocessProofGenerator::new(cfg(Path::new("step"), None, Some(Path::new("."))));
        let err = gen.generate_step(&sample_update()).await.unwrap_err();
        match err {
            RelayerError::ProofGeneration(m) => {
                assert!(m.contains("committee_json empty"), "{m}");
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn fixture_rotate_period_is_synthetic_without_an_update_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(fixture_rotate_period(dir.path()), 9413);
    }

    #[test]
    fn fixture_rotate_period_reads_the_beacon_updates_array() {
        let dir = tempfile::tempdir().unwrap();
        let mainnet = dir.path().join("fixtures/mainnet");
        std::fs::create_dir_all(&mainnet).unwrap();
        std::fs::write(
            mainnet.join("update_period_1835.json"),
            r#"[{"data":{"attested_header":{"beacon":{"slot":"15032320"}}}}]"#,
        )
        .unwrap();
        assert_eq!(fixture_rotate_period(dir.path()), 1836);
    }

    #[tokio::test]
    async fn run_rotate_refuses_a_live_period_before_spawning() {
        let gen = SubprocessProofGenerator::new(SubprocessProverConfig {
            step_prover_bin: "export_step_vk_blob".into(),
            rotate_prover_bin: None,
            prover_dir: Some(".".into()),
            srs_path: ".".into(),
            timeout: Duration::from_secs(1),
            log_dir: None,
        });
        let err = gen.generate_rotate(1, None).await.unwrap_err();
        match err {
            RelayerError::ProofGeneration(m) => {
                assert!(m.contains("committed fixture"), "{m}");
                assert!(m.contains("not live period 1"), "{m}");
            },
            other => panic!("{other:?}"),
        }
    }
}
