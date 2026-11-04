//! The committee hand-off P→P+1, proven in the background
//! (bridge-audit#145).
//!
//! Committee P signs the updates attested in period P, and their state's
//! `next_sync_committee` is committee P+1. Once the beacon node serves such an
//! update (`/updates?start_period=P`) and the contract still holds committee
//! P, the daemon proves the hand-off in a task of its own while it goes on
//! sending step updates: `export_shard_snark` → `export_step_snark` →
//! `rotate_tree_n8`, each a subprocess writing `<work>/rotate-<P>/<stage>/`.
//!
//! A stage is done when its directory exists. It is written as
//! `<stage>.partial/` and renamed when the prover exits 0, so a restart runs
//! again only the stage that was cut short. The inputs go to `inputs/` before
//! the first stage: all three prove the same update, and the step snark's
//! attested `state_root` is what anchors the tree's `next_sync_committee`
//! branch.

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
        BeaconChainParams, RotateProofBundle, ROTATE_ACCUMULATOR_LIMBS, ROTATE_ATTESTED_SLOT_INDEX,
        ROTATE_GLOAS_FORK_SLOT_INDEX, ROTATE_INSTANCE_LEN, ROTATE_PERIOD_INDEX,
        SLOTS_PER_SYNC_PERIOD,
    },
};

/// The `rotate_vk_blob.bin` the light client's `ROTATE_VK_BLOB` is checked
/// against (`scripts/check_rotate_vkblob_accumulator.sh`, check 4). A rotate
/// proof whose tree wrote another blob is never sent.
const ROTATE_VK_BLOB_SHA256: &str = include_str!(
    "../../../eth-light-client-prover/fixtures/rotate_vkblob/rotate_vk_blob.bin.sha256"
);

/// Index of `current_commit` in the rotate public inputs.
pub const ROTATE_CURRENT_COMMIT_INDEX: usize = ROTATE_ACCUMULATOR_LIMBS;
/// Index of `next_commit` in the rotate public inputs.
pub const ROTATE_NEXT_COMMIT_INDEX: usize = ROTATE_ACCUMULATOR_LIMBS + 1;

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
    Shard,
    StepSnark,
    Tree,
}

impl Stage {
    pub const ALL: [Stage; 3] = [Stage::Shard, Stage::StepSnark, Stage::Tree];

    /// The stage's directory under `rotate-<P>/`, and its part of the
    /// transcript names.
    pub fn name(self) -> &'static str {
        match self {
            Stage::Shard => "shard",
            Stage::StepSnark => "step",
            Stage::Tree => "tree",
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
        let before = attested_slot(&self.committee_json)
            .map_err(|e| refuse(format!("the update of the period before: {e}")))?;
        if before / SLOTS_PER_SYNC_PERIOD + 1 != self.period {
            return Err(refuse(format!(
                "the update of the period before is attested at slot {before}, period {}",
                before / SLOTS_PER_SYNC_PERIOD
            )));
        }
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

    /// The proof the tree stage wrote, and the sha256 of its VK blob.
    pub fn output(&self) -> Result<RotateOutput, RelayerError> {
        let tree = self.stage(Stage::Tree);
        let bundle = RotateProofBundle::from_dir(&tree)?;
        let blob = fs::read(tree.join("rotate_vk_blob.bin")).map_err(|e| {
            RelayerError::ProofGeneration(format!(
                "read {}/rotate_vk_blob.bin: {e}",
                tree.display()
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
    /// `current_commit`, as the 32-byte little-endian word the contract's
    /// `_currentCommittee` is compared with.
    pub fn current_commit(&self) -> [u8; 32] {
        word(&self.bundle.public_inputs, ROTATE_CURRENT_COMMIT_INDEX)
    }

    /// The Gloas fork slot the proof was made for.
    pub fn gloas_fork_slot(&self) -> u64 {
        let w = word(&self.bundle.public_inputs, ROTATE_GLOAS_FORK_SLOT_INDEX);
        u64::from_le_bytes(w[..8].try_into().expect("8 bytes"))
    }
}

fn word(public_inputs: &[u8], index: usize) -> [u8; 32] {
    let mut w = [0u8; 32];
    w.copy_from_slice(&public_inputs[index * 32..(index + 1) * 32]);
    w
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
/// the proof rotates from, the VK blob is the pinned one, the proof is for
/// the next period, and it starts from the committee the contract holds.
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
            "rotate proof period {} != {} (the rotate from {})",
            out.bundle.period,
            out.from_period + 1,
            out.from_period
        ));
    }
    let current = out.current_commit();
    if current != chain.current_committee {
        return PreSubmit::Refuse(format!(
            "rotate current_commit 0x{} != contract currentCommittee 0x{}",
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

/// The three prover binaries and where they find their SRS and keys.
#[derive(Clone, Debug)]
pub struct SubprocessRotateConfig {
    /// `export_shard_snark` (`--features aggregation`).
    pub shard_prover_bin: PathBuf,
    /// `export_step_snark` (`--features aggregation`).
    pub step_snark_prover_bin: PathBuf,
    /// `rotate_tree_n8` (`--features aggregation`).
    pub rotate_prover_bin: PathBuf,
    /// `--prover-dir`: the SRS files `kzg_bn254_21.srs` and
    /// `kzg_bn254_20.srs` (the shard stage's, cut from the first with
    /// `downsize_srs`) and the key cache `rotate-keys/`; also the provers'
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

    pub fn shard_srs(&self) -> PathBuf {
        self.prover_dir.join("kzg_bn254_20.srs")
    }

    pub fn key_dir(&self) -> PathBuf {
        self.prover_dir.join("rotate-keys")
    }

    /// The SRS files are where the stages will read them.
    pub fn check(&self) -> Result<(), RelayerError> {
        for (srs, what) in [
            (self.srs(), "the Hermez kzg_bn254_21.srs release asset"),
            (
                self.shard_srs(),
                "cut from kzg_bn254_21.srs with `downsize_srs --k 20`",
            ),
        ] {
            if !srs.is_file() {
                return Err(RelayerError::ProofGeneration(format!(
                    "rotate SRS {}: not a file ({what})",
                    srs.display()
                )));
            }
        }
        Ok(())
    }
}

/// The daemon's endpoints and key path, kept out of the stages' environment.
const STAGE_ENV_REMOVED: [&str; 6] = [
    "BEACON_URL",
    "ETH_RPC_URL",
    "AN_GRAPHQL_URL",
    "AN_KEYS_PATH",
    "PARENT_RPC_URL",
    "ARBITRUM_RPC_URL",
];

pub struct SubprocessRotateRunner {
    cfg: SubprocessRotateConfig,
}

impl SubprocessRotateRunner {
    /// The directories are made absolute: every stage runs in `prover_dir`,
    /// where a relative path would resolve a second time.
    pub fn new(mut cfg: SubprocessRotateConfig) -> Self {
        for dir in [&mut cfg.prover_dir, &mut cfg.log_dir] {
            if let Ok(abs) = std::path::absolute(&*dir) {
                *dir = abs;
            }
        }
        Self {
            cfg,
        }
    }

    fn command(&self, stage: Stage, dir: &RotateDir, inputs: &RotateInputs, out: &Path) -> Command {
        let cfg = &self.cfg;
        let mut cmd = match stage {
            Stage::Shard => {
                let mut cmd = Command::new(&cfg.shard_prover_bin);
                cmd.env("SHARD_OUT_DIR", out)
                    .env("SHARD_SRS_PATH", cfg.shard_srs());
                cmd
            },
            Stage::StepSnark => {
                let mut cmd = Command::new(&cfg.step_snark_prover_bin);
                cmd.env("STEP_OUT_DIR", out)
                    .env("STEP_SRS_PATH", cfg.srs())
                    .env("COMMITTEE_JSON_PATH", dir.committee_path());
                cmd
            },
            Stage::Tree => {
                let mut cmd = Command::new(&cfg.rotate_prover_bin);
                cmd.env("TREE_OUT", out)
                    .env("SHARD_DIR", dir.stage(Stage::Shard))
                    .env("STEP_DIR", dir.stage(Stage::StepSnark))
                    .env("OUTER_SRS", cfg.srs())
                    .env("STEP_SRS", cfg.srs())
                    .env("EMIT_VKBLOB", "1");
                cmd
            },
        };
        // The stages read none of these, and their transcripts are written
        // straight from the child's descriptors, past `redact_urls`.
        for var in STAGE_ENV_REMOVED {
            cmd.env_remove(var);
        }
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
/// tests. The tree's VK blob is [`MockRotateRunner::VK_BLOB`].
pub struct MockRotateRunner {
    /// `current_commit` the tree writes.
    pub current_commit: [u8; 32],
    pub next_commit: [u8; 32],
    /// Fail this stage once, as a prover killed half way would: the partial
    /// directory keeps what was written.
    pub fail_once: Mutex<Option<Stage>>,
    /// Hold this stage until notified.
    pub hold: Option<(Stage, std::sync::Arc<tokio::sync::Notify>)>,
    runs: Mutex<Vec<(u64, Stage)>>,
}

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
        if stage == Stage::Tree {
            let attested = attested_slot(&inputs.update_json).map_err(RelayerError::other)?;
            let mut pi = vec![0u8; ROTATE_INSTANCE_LEN * 32];
            let mut put = |index: usize, bytes: &[u8]| {
                pi[index * 32..index * 32 + bytes.len()].copy_from_slice(bytes);
            };
            put(ROTATE_CURRENT_COMMIT_INDEX, &self.current_commit);
            put(ROTATE_NEXT_COMMIT_INDEX, &self.next_commit);
            put(ROTATE_PERIOD_INDEX, &(dir.period() + 1).to_le_bytes());
            let gloas = inputs
                .chain
                .map_or(crate::types::GLOAS_NOT_SCHEDULED, |c| c.gloas_fork_slot);
            put(ROTATE_GLOAS_FORK_SLOT_INDEX, &gloas.to_le_bytes());
            put(ROTATE_ATTESTED_SLOT_INDEX, &attested.to_le_bytes());
            fs::write(out.join("rotate_public_inputs.bin"), &pi)?;
            fs::write(out.join("rotate_proof_blake2b.bin"), [0xCD; 64])?;
            fs::write(out.join("rotate_vk_blob.bin"), Self::VK_BLOB)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stages' transcripts bypass `redact_urls`, so the daemon's
    /// endpoints never reach their environment.
    #[test]
    fn a_stage_does_not_inherit_the_daemon_endpoints() {
        let dir = tempfile::tempdir().unwrap();
        let runner = SubprocessRotateRunner::new(SubprocessRotateConfig {
            shard_prover_bin: "shard".into(),
            step_snark_prover_bin: "step".into(),
            rotate_prover_bin: "tree".into(),
            prover_dir: dir.path().into(),
            log_dir: dir.path().into(),
            timeout: Duration::from_secs(1),
        });
        let rotate_dir = RotateDir::new(dir.path(), 1);
        let inputs = RotateInputs {
            period: 1,
            update_json: String::new(),
            committee_json: String::new(),
            chain: None,
        };
        for stage in [Stage::Shard, Stage::StepSnark, Stage::Tree] {
            let cmd = runner.command(stage, &rotate_dir, &inputs, dir.path());
            let envs: Vec<_> = cmd.as_std().get_envs().collect();
            for var in STAGE_ENV_REMOVED {
                assert!(
                    envs.contains(&(std::ffi::OsStr::new(var), None)),
                    "{}: {var}",
                    stage.name()
                );
            }
        }
        let shard = runner.command(Stage::Shard, &rotate_dir, &inputs, dir.path());
        let srs = shard
            .as_std()
            .get_envs()
            .find(|(k, _)| *k == "SHARD_SRS_PATH")
            .and_then(|(_, v)| v)
            .unwrap();
        assert_eq!(Path::new(srs), dir.path().join("kzg_bn254_20.srs"));
    }

    /// A relative `--prover-dir` would resolve again inside the stage, which
    /// runs in that directory.
    #[test]
    fn a_relative_prover_dir_reaches_the_stages_absolute() {
        let runner = SubprocessRotateRunner::new(SubprocessRotateConfig {
            shard_prover_bin: "shard".into(),
            step_snark_prover_bin: "step".into(),
            rotate_prover_bin: "tree".into(),
            prover_dir: "params".into(),
            log_dir: "logs".into(),
            timeout: Duration::from_secs(1),
        });
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(runner.cfg.prover_dir, cwd.join("params"));
        assert_eq!(runner.cfg.log_dir, cwd.join("logs"));
        assert_eq!(runner.cfg.shard_srs(), cwd.join("params/kzg_bn254_20.srs"));
    }
}
