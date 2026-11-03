//! The committee hand-off P→P+1, proven in the background
//! (bridge-audit#145).
//!
//! Committee P signs the updates attested in period P, and their state's
//! `next_sync_committee` is committee P+1. Once the beacon node serves such an
//! update (`/updates?start_period=P`) and the contract still holds committee
//! P, the daemon proves the hand-off in a task of its own while it goes on
//! sending step updates: `export_step_vk_blob` → `rotate_committee`, each a
//! subprocess writing `<work>/rotate-<P>/<stage>/`. `submitRotate` takes both
//! proofs: the step proof says committee P signed the update's attested
//! `state_root`, the rotate proof says which committee sits in that state's
//! `next_sync_committee`.
//!
//! A stage is done when its directory exists. It is written as
//! `<stage>.partial/` and renamed when the prover exits 0, so a restart runs
//! again only the stage that was cut short. The inputs go to `inputs/` before
//! the first stage: both stages prove the same update.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Mutex,
    time::Duration,
};

use async_trait::async_trait;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::process::Command;
use tracing::info;

use crate::{
    error::RelayerError,
    types::{
        word, BeaconChainParams, RotateProofBundle, PUBLIC_INPUT_BYTES, ROTATE_ATTESTED_SLOT_INDEX,
        ROTATE_GLOAS_FORK_SLOT_INDEX, ROTATE_INSTANCE_LEN, ROTATE_NEXT_COMMIT_INDEX,
        ROTATE_STATE_ROOT_INDEX, SLOTS_PER_SYNC_PERIOD, STEP_COMMITTEE_INDEX,
        STEP_GLOAS_FORK_SLOT_INDEX, STEP_STATE_ROOT_INDEX,
    },
};

/// The `rotate_vk_blob.bin` the light client's `ROTATE_VK_BLOB` is checked
/// against (`scripts/check_rotate_vkblob_accumulator.sh`, check 4). A rotate
/// proof whose prover wrote another blob is never sent.
const ROTATE_VK_BLOB_SHA256: &str = include_str!(
    "../../../eth-light-client-prover/fixtures/rotate_vkblob/rotate_vk_blob.bin.sha256"
);

/// sha256 of the pinned `rotate_vk_blob.bin`.
pub fn pinned_rotate_vk_sha256() -> [u8; 32] {
    let hex_digest = ROTATE_VK_BLOB_SHA256
        .split_whitespace()
        .next()
        .expect("rotate_vk_blob.bin.sha256 is not empty");
    let mut out = [0u8; 32];
    hex::decode_to_slice(hex_digest, &mut out).expect("rotate_vk_blob.bin.sha256 is a sha256");
    out
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Step,
    Committee,
}

impl Stage {
    pub const ALL: [Stage; 2] = [Stage::Step, Stage::Committee];

    /// The stage's directory under `rotate-<P>/`, and its part of the
    /// transcript names.
    pub fn name(self) -> &'static str {
        match self {
            Stage::Step => "step",
            Stage::Committee => "committee",
        }
    }
}

/// What the rotate from committee `period` proves: the update of that period
/// (`/updates?start_period=P`) and the update of the period before, whose
/// `next_sync_committee` is committee P — the one that signed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RotateInputs {
    pub period: u64,
    pub update_json: String,
    pub committee_json: String,
    /// Signing domain at the update's `signature_slot`, with the contract's
    /// Gloas slot. `None` leaves the provers on their own environment.
    pub chain: Option<BeaconChainParams>,
}

impl RotateInputs {
    /// Both updates are there and the update is attested in `period`: a
    /// beacon node early in a period may still answer with an older update,
    /// and that proof would be for the wrong period.
    pub fn check(&self) -> Result<(), RelayerError> {
        let refuse = |why: String| {
            RelayerError::ProofGeneration(format!("rotate from period {}: {why}", self.period))
        };
        if self.update_json.trim().is_empty() {
            return Err(refuse("the update of the period is empty".into()));
        }
        if self.committee_json.trim().is_empty() {
            return Err(refuse("the update of the period before is empty".into()));
        }
        let attested = attested_slot(&self.update_json).map_err(refuse)?;
        if attested / SLOTS_PER_SYNC_PERIOD != self.period {
            return Err(refuse(format!(
                "the update is attested at slot {attested}, period {}",
                attested / SLOTS_PER_SYNC_PERIOD
            )));
        }
        attested_slot(&self.committee_json)
            .map_err(|e| refuse(format!("the update of the period before: {e}")))?;
        Ok(())
    }
}

/// `attested_header.beacon.slot` of a `/updates` answer (an array) or of a
/// single update.
fn attested_slot(json: &str) -> Result<u64, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("not JSON: {e}"))?;
    let data = match v.as_array() {
        Some(items) => items
            .first()
            .map(|u| &u["data"])
            .ok_or_else(|| "no update in the answer".to_string())?,
        None if v.get("data").is_some() => &v["data"],
        None => &v,
    };
    let slot = &data["attested_header"]["beacon"]["slot"];
    slot.as_u64()
        .or_else(|| slot.as_str().and_then(|s| s.parse().ok()))
        .ok_or_else(|| "no attested_header.beacon.slot".to_string())
}

/// `<work>/rotate-<P>/`: the inputs and the stages of one rotate.
#[derive(Clone, Debug)]
pub struct RotateDir {
    root: PathBuf,
    period: u64,
}

impl RotateDir {
    pub fn new(work_dir: &Path, period: u64) -> Self {
        Self {
            root: work_dir.join(format!("rotate-{period}")),
            period,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn period(&self) -> u64 {
        self.period
    }

    pub fn stage(&self, stage: Stage) -> PathBuf {
        self.root.join(stage.name())
    }

    fn partial(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.partial"))
    }

    fn inputs(&self) -> PathBuf {
        self.root.join("inputs")
    }

    pub fn update_path(&self) -> PathBuf {
        self.inputs().join("update.json")
    }

    pub fn committee_path(&self) -> PathBuf {
        self.inputs().join("committee.json")
    }

    fn chain_path(&self) -> PathBuf {
        self.inputs().join("chain.json")
    }

    /// The inputs saved by an earlier run, if it got that far.
    pub fn load_inputs(&self) -> Result<Option<RotateInputs>, RelayerError> {
        if !self.inputs().is_dir() {
            return Ok(None);
        }
        let chain = match fs::read_to_string(self.chain_path()) {
            Ok(text) => Some(parse_chain(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        Ok(Some(RotateInputs {
            period: self.period,
            update_json: fs::read_to_string(self.update_path())?,
            committee_json: fs::read_to_string(self.committee_path())?,
            chain,
        }))
    }

    /// Written once, as a whole: the stages after a restart prove what the
    /// stages before it proved, whatever the beacon node serves by then.
    pub fn save_inputs(&self, inputs: &RotateInputs) -> Result<(), RelayerError> {
        fs::create_dir_all(&self.root)?;
        let partial = self.partial("inputs");
        if partial.exists() {
            fs::remove_dir_all(&partial)?;
        }
        fs::create_dir_all(&partial)?;
        fs::write(partial.join("update.json"), &inputs.update_json)?;
        fs::write(partial.join("committee.json"), &inputs.committee_json)?;
        if let Some(chain) = &inputs.chain {
            fs::write(partial.join("chain.json"), chain_json(chain).to_string())?;
        }
        fs::rename(&partial, self.inputs())?;
        Ok(())
    }

    /// The proofs both stages wrote, and the sha256 of the rotate VK blob.
    pub fn output(&self) -> Result<RotateOutput, RelayerError> {
        let committee = self.stage(Stage::Committee);
        let bundle = RotateProofBundle::from_dirs(&self.stage(Stage::Step), &committee)?;
        let blob = fs::read(committee.join("rotate_vk_blob.bin")).map_err(|e| {
            RelayerError::ProofGeneration(format!(
                "read {}/rotate_vk_blob.bin: {e}",
                committee.display()
            ))
        })?;
        Ok(RotateOutput {
            from_period: self.period,
            bundle,
            vk_sha256: sha256(&blob),
        })
    }

    pub fn remove(&self) {
        if let Err(e) = fs::remove_dir_all(&self.root) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(dir = %self.root.display(), error = %e, "rotate directory not removed");
            }
        }
    }
}

/// Remove every `rotate-<N>/` with `N < period`: a hand-off the contract is
/// past, by this daemon or by anyone else.
pub fn remove_rotate_dirs_before(work_dir: &Path, period: u64) {
    let Ok(entries) = fs::read_dir(work_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(n) = name
            .to_str()
            .and_then(|n| n.strip_prefix("rotate-"))
            .and_then(|n| n.parse::<u64>().ok())
        else {
            continue;
        };
        if n < period {
            RotateDir::new(work_dir, n).remove();
        }
    }
}

fn chain_json(chain: &BeaconChainParams) -> Value {
    serde_json::json!({
        "fork_version": chain.fork_version_hex(),
        "genesis_validators_root": chain.genesis_validators_root_hex(),
        "gloas_fork_slot": chain.gloas_fork_slot,
    })
}

fn parse_chain(text: &str) -> Result<BeaconChainParams, RelayerError> {
    let bad = |what: &str| RelayerError::other(format!("rotate inputs chain.json: {what}"));
    let v: Value = serde_json::from_str(text)?;
    let hex_field = |key: &str, out: &mut [u8]| -> Result<(), RelayerError> {
        let s = v[key].as_str().ok_or_else(|| bad(key))?;
        hex::decode_to_slice(s.trim_start_matches("0x"), out).map_err(|_| bad(key))
    };
    let mut fork_version = [0u8; 4];
    let mut genesis_validators_root = [0u8; 32];
    hex_field("fork_version", &mut fork_version)?;
    hex_field("genesis_validators_root", &mut genesis_validators_root)?;
    let gloas_fork_slot = v["gloas_fork_slot"]
        .as_u64()
        .ok_or_else(|| bad("gloas_fork_slot"))?;
    Ok(BeaconChainParams {
        fork_version,
        genesis_validators_root,
        gloas_fork_slot,
    })
}

/// A finished rotate: the proof of `from_period` → `from_period + 1`.
#[derive(Clone, Debug)]
pub struct RotateOutput {
    pub from_period: u64,
    pub bundle: RotateProofBundle,
    pub vk_sha256: [u8; 32],
}

impl RotateOutput {
    /// The step proof's signing committee, as the 32-byte little-endian word
    /// the contract's `_currentCommittee` is compared with.
    pub fn current_commit(&self) -> [u8; 32] {
        word(&self.bundle.step_public_inputs, STEP_COMMITTEE_INDEX)
    }
}

/// The contract's side of the checks before `submitRotate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitteeState {
    /// `currentCommittee` as a 32-byte little-endian word, the form of the
    /// rotate public inputs.
    pub current_committee: [u8; 32],
    pub period: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreSubmit {
    Send,
    /// The contract moved past the period this proof rotates from: an owner
    /// hop or a manual `submit-rotate` got there first.
    Stale {
        on_chain: u64,
    },
    /// The proof is not one the contract takes; the reason names both
    /// values.
    Refuse(String),
}

/// Whether `out` goes to `submitRotate`: the contract still holds the period
/// the proof rotates from, the VK blob is the pinned one, the update is
/// attested in that period and signed by the committee the contract holds,
/// and the two proofs are of the same update.
pub fn check_before_submit(
    out: &RotateOutput,
    vk_pin: &[u8; 32],
    chain: &CommitteeState,
) -> PreSubmit {
    if chain.period != out.from_period {
        return PreSubmit::Stale {
            on_chain: chain.period,
        };
    }
    if &out.vk_sha256 != vk_pin {
        return PreSubmit::Refuse(format!(
            "rotate_vk_blob.bin sha256 {} != pinned {}",
            hex::encode(out.vk_sha256),
            hex::encode(vk_pin)
        ));
    }
    if out.bundle.period != out.from_period + 1 {
        return PreSubmit::Refuse(format!(
            "rotate proof attested at slot {}, not in period {}",
            out.bundle.attested_slot, out.from_period
        ));
    }
    let (step, rotate) = (&out.bundle.step_public_inputs, &out.bundle.public_inputs);
    if step.len() != PUBLIC_INPUT_BYTES || rotate.len() != ROTATE_INSTANCE_LEN * 32 {
        return PreSubmit::Refuse("public inputs of the wrong length".into());
    }
    let pairs = [
        (
            "attested slot",
            word(step, 0),
            word(rotate, ROTATE_ATTESTED_SLOT_INDEX),
        ),
        (
            "state_root_hi",
            word(step, STEP_STATE_ROOT_INDEX),
            word(rotate, ROTATE_STATE_ROOT_INDEX),
        ),
        (
            "state_root_lo",
            word(step, STEP_STATE_ROOT_INDEX + 1),
            word(rotate, ROTATE_STATE_ROOT_INDEX + 1),
        ),
        (
            "gloas_fork_slot",
            word(step, STEP_GLOAS_FORK_SLOT_INDEX),
            word(rotate, ROTATE_GLOAS_FORK_SLOT_INDEX),
        ),
    ];
    for (what, s, r) in pairs {
        if s != r {
            return PreSubmit::Refuse(format!(
                "step and rotate proofs differ on {what}: 0x{} != 0x{}",
                hex::encode(s),
                hex::encode(r)
            ));
        }
    }
    let current = out.current_commit();
    if current != chain.current_committee {
        return PreSubmit::Refuse(format!(
            "step committee_commitment 0x{} != contract currentCommittee 0x{}",
            hex::encode(current),
            hex::encode(chain.current_committee)
        ));
    }
    PreSubmit::Send
}

#[async_trait]
pub trait RotateStageRunner: Send + Sync {
    /// Run `stage` of the rotate in `dir`, writing its whole output into
    /// `out` (empty when called). The earlier stages are in `dir`.
    async fn run_stage(
        &self,
        stage: Stage,
        dir: &RotateDir,
        inputs: &RotateInputs,
        out: &Path,
    ) -> Result<(), RelayerError>;
}

/// Run every stage not done yet, then read the proof back.
pub async fn run_rotate(
    runner: &dyn RotateStageRunner,
    dir: &RotateDir,
    inputs: &RotateInputs,
) -> Result<RotateOutput, RelayerError> {
    for stage in Stage::ALL {
        let done = dir.stage(stage);
        if done.is_dir() {
            info!(
                period = dir.period(),
                stage = stage.name(),
                "rotate stage already done"
            );
            continue;
        }
        let partial = dir.partial(stage.name());
        if partial.exists() {
            fs::remove_dir_all(&partial)?;
        }
        fs::create_dir_all(&partial)?;
        info!(
            period = dir.period(),
            stage = stage.name(),
            "rotate stage starting"
        );
        let started = std::time::Instant::now();
        runner.run_stage(stage, dir, inputs, &partial).await?;
        fs::rename(&partial, &done)?;
        info!(
            period = dir.period(),
            stage = stage.name(),
            secs = started.elapsed().as_secs(),
            "rotate stage done"
        );
    }
    dir.output()
}

/// The two prover binaries and where they find their SRS and keys.
#[derive(Clone, Debug)]
pub struct SubprocessRotateConfig {
    /// `export_step_vk_blob`, the daemon's step prover.
    pub step_prover_bin: PathBuf,
    /// `rotate_committee` (`--features aggregation`).
    pub rotate_prover_bin: PathBuf,
    /// `--prover-dir`: the SRS file `kzg_bn254_21.srs` (each stage cuts its
    /// k from it) and the key cache `rotate-keys/`; also the provers'
    /// working directory.
    pub prover_dir: PathBuf,
    pub log_dir: PathBuf,
    /// Per stage (`--rotate-timeout-secs`).
    pub timeout: Duration,
}

impl SubprocessRotateConfig {
    pub fn srs(&self) -> PathBuf {
        self.prover_dir.join("kzg_bn254_21.srs")
    }

    pub fn key_dir(&self) -> PathBuf {
        self.prover_dir.join("rotate-keys")
    }

    /// The SRS file is where the stages will read it.
    pub fn check(&self) -> Result<(), RelayerError> {
        let srs = self.srs();
        if !srs.is_file() {
            return Err(RelayerError::ProofGeneration(format!(
                "rotate SRS {}: not a file (the Hermez kzg_bn254_21.srs release asset)",
                srs.display()
            )));
        }
        Ok(())
    }
}

pub struct SubprocessRotateRunner {
    cfg: SubprocessRotateConfig,
}

impl SubprocessRotateRunner {
    pub fn new(cfg: SubprocessRotateConfig) -> Self {
        Self {
            cfg,
        }
    }

    fn command(&self, stage: Stage, dir: &RotateDir, inputs: &RotateInputs, out: &Path) -> Command {
        let cfg = &self.cfg;
        let mut cmd = match stage {
            Stage::Step => {
                let mut cmd = Command::new(&cfg.step_prover_bin);
                cmd.env("FINALITY_UPDATE_PATH", dir.update_path())
                    .env("COMMITTEE_JSON_PATH", dir.committee_path())
                    .env("STEP_OUT_DIR", out)
                    .env("STEP_SRS_PATH", cfg.srs());
                cmd
            },
            Stage::Committee => {
                let mut cmd = Command::new(&cfg.rotate_prover_bin);
                cmd.env("ROTATE_OUT", out).env("OUTER_SRS", cfg.srs());
                cmd
            },
        };
        cmd.current_dir(&cfg.prover_dir)
            .env("ROTATE_UPDATE_PATH", dir.update_path())
            .env("ROTATE_KEY_DIR", cfg.key_dir());
        if let Some(chain) = &inputs.chain {
            cmd.env(
                BeaconChainParams::ENV_FORK_VERSION,
                chain.fork_version_hex(),
            )
            .env(
                BeaconChainParams::ENV_GENESIS_VALIDATORS_ROOT,
                chain.genesis_validators_root_hex(),
            )
            .env(
                BeaconChainParams::ENV_GLOAS_FORK_SLOT,
                chain.gloas_fork_slot.to_string(),
            );
        }
        cmd
    }
}

#[async_trait]
impl RotateStageRunner for SubprocessRotateRunner {
    async fn run_stage(
        &self,
        stage: Stage,
        dir: &RotateDir,
        inputs: &RotateInputs,
        out: &Path,
    ) -> Result<(), RelayerError> {
        fs::create_dir_all(&self.cfg.log_dir)?;
        // Straight to files, not through pipes: a stage runs for hours, and
        // what it printed before a kill is what explains the kill.
        let stem = format!("rotate-{}-{}-{}", dir.period(), stage.name(), unix_now());
        let stdout_path = self.cfg.log_dir.join(format!("{stem}-stdout.log"));
        let stderr_path = self.cfg.log_dir.join(format!("{stem}-stderr.log"));
        let mut cmd = self.command(stage, dir, inputs, out);
        cmd.stdout(Stdio::from(fs::File::create(&stdout_path)?))
            .stderr(Stdio::from(fs::File::create(&stderr_path)?))
            .kill_on_drop(true);
        let bin = cmd.as_std().get_program().to_owned();
        let mut child = cmd.spawn().map_err(|e| {
            RelayerError::ProofGeneration(format!("spawn {}: {e}", Path::new(&bin).display()))
        })?;
        let status = match tokio::time::timeout(self.cfg.timeout, child.wait()).await {
            Ok(status) => status?,
            Err(_) => {
                child.kill().await.ok();
                return Err(RelayerError::ProofGeneration(format!(
                    "rotate stage {} of period {} killed after {:?} (--rotate-timeout-secs); \
                     transcript {}",
                    stage.name(),
                    dir.period(),
                    self.cfg.timeout,
                    stderr_path.display()
                )));
            },
        };
        if !status.success() {
            let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
            let tail = stderr
                .lines()
                .rev()
                .take(12)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            return Err(RelayerError::ProofGeneration(format!(
                "{} exited {status}; stderr tail:\n{}",
                Path::new(&bin).display(),
                crate::redact::redact_urls(&tail)
            )));
        }
        Ok(())
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Stages that write a well-formed rotate in no time: `--mock-prove`, and the
/// tests. The committee stage's VK blob is [`MockRotateRunner::VK_BLOB`].
pub struct MockRotateRunner {
    /// The signing committee the step stage writes.
    pub current_commit: [u8; 32],
    pub next_commit: [u8; 32],
    /// Fail this stage once, as a prover killed half way would: the partial
    /// directory keeps what was written.
    pub fail_once: Mutex<Option<Stage>>,
    /// Hold this stage until notified.
    pub hold: Option<(Stage, std::sync::Arc<tokio::sync::Notify>)>,
    runs: Mutex<Vec<(u64, Stage)>>,
}

/// Each half of the state root the mock stages agree on: 16 bytes, so the
/// word fits the 128 bits the contract allows.
const MOCK_STATE_ROOT_HALF: [u8; 16] = [0x5A; 16];

impl MockRotateRunner {
    pub const VK_BLOB: &'static [u8] = b"mock rotate vk blob";

    pub fn new(current_commit: [u8; 32]) -> Self {
        Self {
            current_commit,
            next_commit: [0xD1; 32],
            fail_once: Mutex::new(None),
            hold: None,
            runs: Mutex::new(Vec::new()),
        }
    }

    pub fn vk_pin() -> [u8; 32] {
        sha256(Self::VK_BLOB)
    }

    /// Every stage started, oldest first, with the period it ran for.
    pub fn runs(&self) -> Vec<(u64, Stage)> {
        self.runs.lock().expect("poisoned").clone()
    }
}

#[async_trait]
impl RotateStageRunner for MockRotateRunner {
    async fn run_stage(
        &self,
        stage: Stage,
        dir: &RotateDir,
        inputs: &RotateInputs,
        out: &Path,
    ) -> Result<(), RelayerError> {
        self.runs
            .lock()
            .expect("poisoned")
            .push((dir.period(), stage));
        if let Some((held, notify)) = &self.hold {
            if *held == stage {
                notify.notified().await;
            }
        }
        fs::write(out.join("started"), b"")?;
        if self
            .fail_once
            .lock()
            .expect("poisoned")
            .take_if(|s| *s == stage)
            .is_some()
        {
            return Err(RelayerError::ProofGeneration(format!(
                "mock rotate stage {} failed",
                stage.name()
            )));
        }
        let attested = attested_slot(&inputs.update_json).map_err(RelayerError::other)?;
        let gloas = inputs
            .chain
            .map_or(crate::types::GLOAS_NOT_SCHEDULED, |c| c.gloas_fork_slot);
        let put = |pi: &mut Vec<u8>, index: usize, bytes: &[u8]| {
            pi[index * 32..index * 32 + bytes.len()].copy_from_slice(bytes);
        };
        match stage {
            Stage::Step => {
                let mut pi = vec![0u8; PUBLIC_INPUT_BYTES];
                put(&mut pi, 0, &attested.to_le_bytes());
                put(&mut pi, STEP_COMMITTEE_INDEX, &self.current_commit);
                put(&mut pi, STEP_STATE_ROOT_INDEX, &MOCK_STATE_ROOT_HALF);
                put(&mut pi, STEP_STATE_ROOT_INDEX + 1, &MOCK_STATE_ROOT_HALF);
                put(&mut pi, STEP_GLOAS_FORK_SLOT_INDEX, &gloas.to_le_bytes());
                fs::write(out.join("step_public_inputs.bin"), &pi)?;
                fs::write(out.join("step_proof_blake2b.bin"), [0xAB; 64])?;
            },
            Stage::Committee => {
                let mut pi = vec![0u8; ROTATE_INSTANCE_LEN * 32];
                put(&mut pi, ROTATE_NEXT_COMMIT_INDEX, &self.next_commit);
                put(&mut pi, ROTATE_STATE_ROOT_INDEX, &MOCK_STATE_ROOT_HALF);
                put(&mut pi, ROTATE_STATE_ROOT_INDEX + 1, &MOCK_STATE_ROOT_HALF);
                put(&mut pi, ROTATE_ATTESTED_SLOT_INDEX, &attested.to_le_bytes());
                put(&mut pi, ROTATE_GLOAS_FORK_SLOT_INDEX, &gloas.to_le_bytes());
                fs::write(out.join("rotate_public_inputs.bin"), &pi)?;
                fs::write(out.join("rotate_proof_blake2b.bin"), [0xCD; 64])?;
                fs::write(out.join("rotate_vk_blob.bin"), Self::VK_BLOB)?;
            },
        }
        Ok(())
    }
}
