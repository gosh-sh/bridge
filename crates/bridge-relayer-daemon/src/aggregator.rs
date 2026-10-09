//! M7 ETH-side proof pipeline: turn a **real** Circuit 4 `PrivateWitness` into
//! the SHPLONK aggregator calldata that the deployed
//! `BridgeWithdrawalAggregatorVerifier` accepts on-chain.
//!
//! Two steps, each behind a trait so the relayer + submit path stay
//! unit-testable in microseconds (exactly like [`crate::withdraw_prover`]
//! and `deposit-relayer-daemon`'s `SubprocessProofGenerator`):
//!
//! 1. [`Circuit4SnarkProver`] — re-prove the witness with a **Poseidon**
//!    transcript and emit a snark-verifier `.snark`
//!    ([`InProcessCircuit4SnarkProver`] runs the prover in-process via
//!    `bridge-event-prover-lib` + `bridge-snark-wrap`, matching the shape of
//!    the 1A/1B/2 lane in `bridge-prover-lib::live_driver`). This is the
//!    `our_side_reprove` ETH leg: the AN-side default is Blake2b, but the
//!    aggregator only consumes Poseidon inner snarks.
//! 2. [`ProofAggregator`] — aggregate that inner snark into EVM calldata
//!    `instances ‖ proof` ([`SubprocessAggregator`] shells out to
//!    `bridge-evm-aggregator`'s `aggregate-proof`, which additionally
//!    self-checks that the regenerated verifier source is byte-identical to the
//!    committed `.sol` of the deployed verifier).
//!
//! [`Circuit4ShplonkPipeline`] composes the two and returns a
//! [`PartnerWithdrawalProof`] whose `proof_hex` is the aggregator calldata and
//! whose `public_instances_hex` are the eleven Circuit-4 public inputs (LE Fr)
//! — exactly the shape `submit-withdraw` / `daemon-withdraw` / `daemon-bridge`
//! already consume. A cross-check ([`calldata_binds_instances`]) proves the
//! calldata's re-exposed instances match the eleven public inputs before the
//! proof is surfaced, so a passing pipeline cannot forward mismatched bytes.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use alloy::primitives::U256;
use async_trait::async_trait;
use tracing::info;

use crate::{
    error::RelayerError,
    withdrawal::{
        fr_hex_to_u256, PartnerWithdrawalProof, WITHDRAWAL_CALLDATA_LEN, WITHDRAWAL_PUBLIC_INPUTS,
    },
};

/// Number of 32-byte KZG accumulator limbs the aggregator prepends to the
/// re-exposed inner public inputs (snark-verifier SHPLONK accumulator).
pub const NUM_ACCUMULATOR_INSTANCES: usize = 12;

/// BN254 scalar field modulus `r`. Fr encodings must be strictly less. A
/// Poseidon digest emitted by the aggregator (word 23 of the calldata) is
/// always a valid Fr, so any word we read there that is `>= r` cannot have
/// come from the aggregator and is rejected before we forward the proof.
pub const BN254_FR_MODULUS: U256 = U256::from_limbs([
    0x43e1_f593_f000_0001,
    0x2833_e848_79b9_7091,
    0xb850_45b6_8181_585d,
    0x3064_4e72_e131_a029,
]);

/// The committed Circuit-4 withdrawal verifier name (matches the `.sol` /
/// `.bin` pair in `contracts/ethereum/verifiers/`).
pub const WITHDRAWAL_VERIFIER_NAME: &str = "BridgeWithdrawalAggregatorVerifier";

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
    /// The eleven Circuit-4 public instances, 32-byte **little-endian** Fr each
    /// (`save_instances_binary` layout), i.e. 352 bytes total.
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

/// In-process Circuit 4 (event) Poseidon prover (NB-Q9 PR-B, 2026-08-04).
///
/// Replaces the historical `SubprocessCircuit4SnarkProver` that shelled out
/// to `bridge-snark-utils`'s `export-c4-poseidon-snark --fixture`.
/// Same pipeline flow as [`crate::live_prover`]-style in-process proving of
/// Circuits 1A/1B/2 (they were in-processed earlier in the 2026-07 refactor;
/// C4 kept the subprocess wrapper as scaffolding until this PR).
///
/// End-to-end per call:
///   1. `KeyManager::ensure_event_keys()` (keygens on first run).
///   2. Provision `params/kzg_bn254_{event_k}.srs` if missing — downsized from
///      the fallback K=21 ceremony SRS so g2/s_g2 stay Hermez-anchored.
///   3. Load event PK.
///   4. `generate_event_proof_with_transcript(&km, &witness, Poseidon)`.
///   5. Native `verify_event_proof_with_transcript` self-check.
///   6. Save 11-field-element instances (10 + `anchorLayer`) as flat LE-Fr
///      bytes (`.instances.bin`).
///   7. `bridge_snark_wrap::wrap_poseidon_snark_in_memory` → serialise
///      snark-verifier `Snark` bincode → write `.snark`.
///
/// The heavy sync work runs under `tokio::task::spawn_blocking` because
/// Halo2 keygen + proving are CPU-bound and would starve the async runtime.
pub struct InProcessCircuit4SnarkProver {
    params_dir: PathBuf,
}

impl InProcessCircuit4SnarkProver {
    pub fn new(params_dir: impl Into<PathBuf>) -> Self {
        let mut params_dir = params_dir.into();
        if let Ok(abs) = params_dir.canonicalize() {
            params_dir = abs;
        }
        Self {
            params_dir,
        }
    }
}

/// Ensure `params/kzg_bn254_{k}.srs` exists for the event circuit degree,
/// downsized from the fallback manager's K=21 ceremony SRS. Without this,
/// snark-verifier's internal `gen_srs(k)` would synthesise a *random* SRS on
/// cache miss — a different `s_g2` than the keygen ceremony, which makes the
/// aggregator unable to verify the inner proof. The downsize preserves
/// g2/s_g2 (degree-independent) so the K=19 file shares the K=21 ceremony.
/// Ported from `export_c4_poseidon_snark.rs::ensure_srs_for_event`.
fn ensure_srs_for_event(
    km: &bridge_prover_lib::keys::KeyManager,
    params_dir: &Path,
    event_k: u32,
) -> Result<(), RelayerError> {
    use std::io::Write;

    // Trait imports for `.k()` / `.downsize()` / `.write()` on ParamsKZG (same
    // trait scope the source bin `export_c4_poseidon_snark.rs::main` uses).
    use halo2_base::halo2_proofs::poly::commitment::Params;
    let srs_path = params_dir.join(format!("kzg_bn254_{event_k}.srs"));
    if srs_path.exists() {
        return Ok(());
    }
    let src = km.fallback.srs();
    let src_k = src.k();
    if src_k < event_k {
        return Err(RelayerError::other(format!(
            "shared SRS (K={src_k}) is smaller than the event circuit degree (K={event_k})"
        )));
    }
    let mut p = src.clone();
    if src_k > event_k {
        p.downsize(event_k);
    }
    let file = std::fs::File::create(&srs_path)
        .map_err(|e| RelayerError::other(format!("create SRS {}: {e}", srs_path.display())))?;
    let mut w = std::io::BufWriter::new(file);
    p.write(&mut w)
        .map_err(|e| RelayerError::other(format!("write SRS {}: {e}", srs_path.display())))?;
    w.flush()
        .map_err(|e| RelayerError::other(format!("flush SRS {}: {e}", srs_path.display())))?;
    Ok(())
}

/// Save raw Fr instances as a flat `Vec<u8>` (each Fr → 32-byte LE). Same
/// wire layout as the orchestrator's historical `save_instances_binary`
/// (kept in-tree only to remove that dep from the daemon).
fn save_instances_binary_le(
    instances: &[bridge_prover_lib::Fr],
    output_path: &Path,
) -> Result<(), RelayerError> {
    let mut bytes: Vec<u8> = Vec::with_capacity(instances.len() * 32);
    for fr in instances {
        bytes.extend_from_slice(fr.to_bytes().as_ref());
    }
    std::fs::write(output_path, bytes)
        .map_err(|e| RelayerError::other(format!("write instances {}: {e}", output_path.display())))
}

#[async_trait]
impl Circuit4SnarkProver for InProcessCircuit4SnarkProver {
    async fn prove(
        &self,
        witness_path: &Path,
        snark_dir: &Path,
        name: &str,
    ) -> Result<SnarkArtefacts, RelayerError> {
        use bridge_event_prover_lib::{
            prover::generate_event_proof_with_transcript,
            verifier::verify_event_proof_with_transcript, PrivateWitness,
        };
        use bridge_prover_lib::{keys::KeyManager, transcript::TranscriptKind};

        std::fs::create_dir_all(snark_dir)
            .map_err(|e| RelayerError::other(format!("create snark dir: {e}")))?;

        let params_dir = self.params_dir.clone();
        let witness_path = witness_path.to_path_buf();
        let snark_dir = snark_dir.to_path_buf();
        let name = name.to_string();

        let artefacts =
            tokio::task::spawn_blocking(move || -> Result<SnarkArtefacts, RelayerError> {
                // KeyManager owns four per-circuit sub-managers; the event sub-manager
                // keygens the event circuit (whose `event_config_params.json` records
                // k = 19) against the K=20 KZG SRS (`EventKeyManager::KEYGEN_SRS_K`,
                // matching the event circuit's `vk.domain.k`).
                let mut km = KeyManager::new(&params_dir);
                km.ensure_event_keys()
                    .map_err(|e| RelayerError::other(format!("ensure_event_keys: {e:#}")))?;

                let event_k = km.event_config().k as u32;
                ensure_srs_for_event(&km, &params_dir, event_k)?;

                km.load_event_pk()
                    .map_err(|e| RelayerError::other(format!("load_event_pk: {e:#}")))?;

                let raw = std::fs::read_to_string(&witness_path).map_err(|e| {
                    RelayerError::other(format!("read witness {}: {e}", witness_path.display()))
                })?;
                let witness: PrivateWitness = serde_json::from_str(&raw).map_err(|e| {
                    RelayerError::other(format!("parse witness {}: {e}", witness_path.display()))
                })?;

                let out = generate_event_proof_with_transcript(
                    &km.event,
                    &witness,
                    TranscriptKind::Poseidon,
                )
                .map_err(|e| RelayerError::other(format!("Circuit 4 Poseidon prove: {e:#}")))?;

                // Native Poseidon self-verify — refuse to hand the aggregator an
                // invalid inner snark (stale event keys are the usual culprit).
                let ok = verify_event_proof_with_transcript(
                    &km.event,
                    &out.proof_bytes,
                    &out.public_instances,
                    TranscriptKind::Poseidon,
                );
                km.unload_event_pk();
                if !ok {
                    return Err(RelayerError::other(
                        "Circuit 4 Poseidon inner snark failed native self-verification — \
                         refusing to emit an invalid snark. Regenerate event keys against the \
                         current circuit shape.",
                    ));
                }

                let instances_path = snark_dir.join(format!("{name}.instances.bin"));
                save_instances_binary_le(&out.public_instances, &instances_path)?;

                // Wrap into snark-verifier `Snark` bincode via bridge-snark-wrap.
                // Event circuit VK was keygen'd against K=20 (`EventKeyManager::
                // KEYGEN_SRS_K`) while `event_config_params.json` records k=19;
                // pass the explicit SRS override so `snark-verifier`'s `compile`
                // sees `params.k = 20 == vk.domain.k`.
                let vk_path = params_dir.join("event_vk.bin");
                let config_path = params_dir.join("event_config_params.json");
                let snark_bytes = bridge_snark_wrap::wrap_poseidon_snark_in_memory(
                    &vk_path,
                    &config_path,
                    Some(bridge_prover_lib::keys::EventKeyManager::KEYGEN_SRS_K),
                    &out.proof_bytes,
                    &out.public_instances,
                )
                .map_err(|e| RelayerError::other(format!("wrap Poseidon snark: {e:#}")))?;

                let snark_path = snark_dir.join(format!("{name}.snark"));
                std::fs::write(&snark_path, &snark_bytes).map_err(|e| {
                    RelayerError::other(format!("write snark {}: {e}", snark_path.display()))
                })?;

                Ok(SnarkArtefacts {
                    snark_path,
                    instances_path,
                })
            })
            .await
            .map_err(|e| RelayerError::other(format!("Circuit 4 blocking task join: {e}")))??;

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
    /// Directory of committed verifier files; `aggregate-proof` self-checks
    /// against the `.sol` sources. Passed as `--verifiers-dir`.
    pub verifiers_dir: PathBuf,
    /// Directory holding `kzg_bn254_21.srs` (the outer SRS). Exported as
    /// `PARAMS_DIR` for the subprocess so `gen_srs(21)` finds the ceremony
    /// file.
    pub params_dir: PathBuf,
    /// Hard timeout for the (K=21) aggregation run.
    pub timeout: Duration,
    /// The calling command's flag for `timeout`, named when it runs out;
    /// `None` where the command has none.
    pub timeout_flag: Option<&'static str>,
    /// Where `aggregate-proof` writes the stem of the pk-cache slot it used
    /// (`--slot-report`), for a caller that notes it.
    pub slot_report: Option<PathBuf>,
    /// Persistent outer PK cache directory. When set, forwarded to
    /// `aggregate-proof` as `--pk-cache-dir`, which memoises the K=21 outer
    /// keygen. The first run against a fresh slot still pays the full keygen;
    /// later ones load the key from disk. Without this, every run re-keygens
    /// from scratch — see `aggregator_cache.rs`.
    pub pk_cache_dir: Option<PathBuf>,
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
            timeout_flag: None,
            slot_report: None,
            pk_cache_dir: None,
        }
    }

    /// Enable persistent outer-PK caching across `aggregate-proof` invocations.
    pub fn with_pk_cache_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.pk_cache_dir = Some(dir.into());
        self
    }
}

/// How many of the aggregator's stderr lines its failure quotes. All of
/// them are in the log already, forwarded as they came.
const QUOTED_LINES: usize = 40;

/// How long to wait for the aggregator's pipes to close once it has exited
/// or been killed. Something it started may hold them open; its own words
/// are in by then.
const OUTPUT_GRACE: Duration = Duration::from_secs(5);

/// What the aggregator said on one stream, for its failure to quote.
#[derive(Debug, Default)]
struct Said {
    /// The last [`QUOTED_LINES`] lines.
    tail: std::collections::VecDeque<String>,
    /// The last error it printed — `Error: …` from its `main`, or a panic —
    /// up to its backtrace: with `RUST_BACKTRACE` set, the tail is frames.
    error: Vec<String>,
    collecting_error: bool,
}

impl Said {
    /// Take `line` in, and say whether it belongs to an error: the
    /// `Caused by:` lines under an `Error:` are as much the error as it is.
    fn push(&mut self, line: &str) -> bool {
        if line.starts_with("Error: ") || line.contains("panicked at") {
            self.error.clear();
            self.collecting_error = true;
        } else if line
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("stack backtrace:")
        {
            self.collecting_error = false;
        }
        if self.collecting_error && self.error.len() < QUOTED_LINES {
            self.error.push(line.to_string());
        }
        if self.tail.len() == QUOTED_LINES {
            self.tail.pop_front();
        }
        self.tail.push_back(line.to_string());
        self.collecting_error
    }

    /// The lines a failure quotes: the error, or the tail without one.
    fn quote(&self) -> String {
        if self.error.is_empty() {
            Vec::from_iter(self.tail.iter().cloned()).join("\n")
        } else {
            self.error.join("\n")
        }
    }
}

/// The level a line the aggregator printed is logged at: the one its log
/// lines start with, `error` for its `main`'s `Error:` and for a panic, and
/// `info` for anything else.
fn level_of(line: &str) -> tracing::Level {
    match line.split_whitespace().next().unwrap_or("") {
        "ERROR" | "Error:" => tracing::Level::ERROR,
        _ if line.contains("panicked at") => tracing::Level::ERROR,
        "WARN" => tracing::Level::WARN,
        "DEBUG" => tracing::Level::DEBUG,
        "TRACE" => tracing::Level::TRACE,
        _ => tracing::Level::INFO,
    }
}

/// Log a line the aggregator printed at [`level_of`] it, or at `error`
/// within an error it printed.
fn forward_line(line: &str, in_error: bool) {
    let level = if in_error {
        tracing::Level::ERROR
    } else {
        level_of(line)
    };
    match level {
        tracing::Level::ERROR => tracing::error!("{AGGREGATE_BIN}: {line}"),
        tracing::Level::WARN => tracing::warn!("{AGGREGATE_BIN}: {line}"),
        tracing::Level::DEBUG => tracing::debug!("{AGGREGATE_BIN}: {line}"),
        tracing::Level::TRACE => tracing::trace!("{AGGREGATE_BIN}: {line}"),
        _ => info!("{AGGREGATE_BIN}: {line}"),
    }
}

/// What one stream said so far, shared between its forwarding task and the
/// wait for it: a stream held open past the grace still has its lines.
type Heard = std::sync::Arc<std::sync::Mutex<Said>>;

/// Log each line of a child's `stream` as `aggregate-proof: <line>` as it
/// comes, and keep what it said in `heard`. Read to the end whatever the
/// bytes are, and drained even past a read error: a child blocked on a full
/// pipe never finishes, and one whose pipe is closed fails its next write.
async fn forward_lines<R: tokio::io::AsyncRead + Unpin>(stream: Option<R>, heard: Heard) {
    use tokio::io::AsyncBufReadExt as _;

    let Some(stream) = stream else {
        return;
    };
    let mut reader = tokio::io::BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) => break,
            Ok(_) => {},
            Err(e) => {
                tracing::warn!("reading {AGGREGATE_BIN}'s output failed: {e}; discarding the rest");
                tokio::io::copy(&mut reader, &mut tokio::io::sink())
                    .await
                    .ok();
                break;
            },
        }
        let line = String::from_utf8_lossy(&buf).trim_end().to_string();
        let in_error = heard.lock().unwrap_or_else(|e| e.into_inner()).push(&line);
        forward_line(&line, in_error);
    }
}

/// Forward `stream` on a task of its own; [`collected`] waits for it.
fn forward(
    stream: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>,
) -> (tokio::task::JoinHandle<()>, Heard) {
    let heard = Heard::default();
    (tokio::spawn(forward_lines(stream, heard.clone())), heard)
}

/// What a stream said, once its forwarding task is done or `grace` has
/// passed: the lines heard by then either way.
async fn collected((task, heard): (tokio::task::JoinHandle<()>, Heard), grace: Duration) -> Said {
    match tokio::time::timeout(grace, task).await {
        Ok(Ok(())) => {},
        Ok(Err(e)) => tracing::warn!("forwarding {AGGREGATE_BIN}'s output failed: {e}"),
        Err(_) => tracing::warn!(
            "{AGGREGATE_BIN}'s output is still open {grace:?} after it ended; something it \
             started holds it"
        ),
    }
    std::mem::take(&mut *heard.lock().unwrap_or_else(|e| e.into_inner()))
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
        // Not there yet — the aggregator writes it — so made absolute rather
        // than canonical: the subprocess runs in `aggregator_dir`, and a
        // relative path (a relative `--snark-dir`, the CLI's default) would
        // land there instead of where the caller reads it.
        if let Some(report) = config.slot_report.as_mut() {
            match std::path::absolute(&*report) {
                Ok(abs) => *report = abs,
                Err(e) => tracing::warn!(
                    path = %report.display(),
                    error = %e,
                    "cannot make the slot-report path absolute; the aggregator resolves it in \
                     its own directory",
                ),
            }
        }
        if let Some(dir) = config.pk_cache_dir.as_mut() {
            // Create if missing so canonicalize() can succeed and the
            // subprocess can write PK files on the very first bundle.
            if !dir.exists() {
                if let Err(e) = std::fs::create_dir_all(&*dir) {
                    tracing::warn!(
                        pk_cache_dir = %dir.display(),
                        error = %e,
                        "failed to create pk_cache_dir; subprocess will attempt to create it",
                    );
                }
            }
            if let Ok(abs) = dir.canonicalize() {
                *dir = abs;
            }
        }
        Self {
            config,
        }
    }

    /// The refusal for a `verifier_name` run that hit `timeout`. The
    /// measured figure is the withdrawal aggregator's, and only it gets one.
    fn timed_out(&self, verifier_name: &str) -> String {
        let measured = if verifier_name == WITHDRAWAL_VERIFIER_NAME {
            " — a cold withdrawal run, keygen and proof, took ~2 min on the test host —"
        } else {
            ""
        };
        let mut msg = format!(
            "{AGGREGATE_BIN} timed out after {:?} and was killed. A cold pk cache generates the \
             aggregator key first{measured} and waits while another process generates one in the \
             same cache.",
            self.config.timeout
        );
        if let Some(flag) = self.config.timeout_flag {
            msg.push_str(&format!(
                " A retry with the same budget can run out the same way: raise it with {flag}."
            ));
        }
        msg
    }

    fn release_bin(&self) -> Option<PathBuf> {
        let bin = self
            .config
            .aggregator_dir
            .join("target/release")
            .join(AGGREGATE_BIN);
        bin.is_file().then_some(bin)
    }

    /// The command for `args`. `RUST_LOG` is not passed on: a filter written
    /// for this process — `ackinacki_bridge=info`, say — names none of the
    /// aggregator's targets and would silence it; it logs at its own default.
    fn command(&self, args: &[String]) -> tokio::process::Command {
        use tokio::process::Command;

        let mut cmd = if let Some(bin) = self.release_bin() {
            let mut c = Command::new(bin);
            c.current_dir(&self.config.aggregator_dir).args(args);
            c
        } else {
            let mut c = Command::new("cargo");
            c.current_dir(&self.config.aggregator_dir)
                .args(["run", "--release", "--bin", AGGREGATE_BIN, "--"])
                .args(args);
            c
        };
        // Killed when the wait is abandoned: a timed-out aggregator left
        // running keeps its memory and its pk-cache writes, beside whatever
        // proof the operator starts next.
        cmd.env("PARAMS_DIR", &self.config.params_dir)
            .env_remove("RUST_LOG")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd
    }

    /// argv (program excluded) for a given inner snark, verifier name and
    /// output path — pulled out for unit-testing flag construction.
    fn args(&self, inner_snark: &Path, verifier_name: &str, out_path: &Path) -> Vec<String> {
        let mut v = vec![
            "--inner-snark".to_string(),
            inner_snark.display().to_string(),
            "--name".to_string(),
            verifier_name.to_string(),
            "--out".to_string(),
            out_path.display().to_string(),
            "--verifiers-dir".to_string(),
            self.config.verifiers_dir.display().to_string(),
        ];
        if let Some(dir) = self.config.pk_cache_dir.as_ref() {
            v.push("--pk-cache-dir".to_string());
            v.push(dir.display().to_string());
        }
        if let Some(path) = self.config.slot_report.as_ref() {
            v.push("--slot-report".to_string());
            v.push(path.display().to_string());
        }
        v
    }
}

#[async_trait]
impl ProofAggregator for SubprocessAggregator {
    async fn aggregate(
        &self,
        inner_snark: &Path,
        verifier_name: &str,
    ) -> Result<Vec<u8>, RelayerError> {
        let out_path = std::env::temp_dir().join(format!(
            "agg_calldata_{verifier_name}_{}.bin",
            std::process::id()
        ));
        // The subprocess is spawned with `current_dir = aggregator_dir`, so a
        // relative `inner_snark` (e.g. the CLI's `--snark-dir=./shplonk-snark`
        // default) would resolve against the wrong CWD and fail with ENOENT.
        // Canonicalize here — the snark was just written by
        // `Circuit4SnarkProver::prove`, so it always exists. If canonicalize
        // ever fails (e.g. permissions), fall through with the original path
        // and let the subprocess surface the real error.
        let inner_snark_abs = inner_snark
            .canonicalize()
            .unwrap_or_else(|_| inner_snark.to_path_buf());
        let args = self.args(&inner_snark_abs, verifier_name, &out_path);

        let t_agg = Instant::now();
        let mut child = self
            .command(&args)
            .spawn()
            .map_err(|e| RelayerError::other(format!("failed to spawn {AGGREGATE_BIN}: {e}")))?;
        // Into this process's log as they come: the pk cache's notices are
        // for the operator while the run is still going.
        let stdout = forward(child.stdout.take());
        let stderr = forward(child.stderr.take());
        let status = match tokio::time::timeout(self.config.timeout, child.wait()).await {
            Ok(status) => {
                status.map_err(|e| RelayerError::other(format!("wait for {AGGREGATE_BIN}: {e}")))?
            },
            Err(_) => {
                // Killed here rather than on drop, so that what it said
                // last — waiting for another keygen, say — is quoted. Under
                // the `cargo run` fallback this kills cargo, not the
                // aggregator it started; `ackinacki-bridge` requires the
                // release binary.
                child.start_kill().ok();
                tokio::time::timeout(OUTPUT_GRACE, child.wait()).await.ok();
                let said = collected(stderr, OUTPUT_GRACE).await.quote();
                return Err(RelayerError::other(format!(
                    "{}\n{AGGREGATE_BIN} said last:\n{said}",
                    self.timed_out(verifier_name)
                )));
            },
        };
        let agg_subprocess_ms = t_agg.elapsed().as_millis() as u64;
        collected(stdout, OUTPUT_GRACE).await;
        let said = collected(stderr, OUTPUT_GRACE).await;

        if !status.success() {
            return Err(RelayerError::other(format!(
                "{AGGREGATE_BIN} exited with {status}: {}",
                said.quote()
            )));
        }

        let calldata = std::fs::read(&out_path).map_err(|e| {
            RelayerError::other(format!(
                "read aggregator calldata {}: {e}",
                out_path.display()
            ))
        })?;
        std::fs::remove_file(&out_path).ok();
        info!(
            "aggregate-proof subprocess ({verifier_name}) took {} ms, calldata={} bytes",
            agg_subprocess_ms,
            calldata.len()
        );
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
    /// a [`PartnerWithdrawalProof`] carrying the calldata + eleven public
    /// inputs. `snark_dir` receives the intermediate `<name>.snark` /
    /// `.instances.bin`.
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

        // The calldata's re-exposed inner instances (words 12..22 inclusive,
        // big-endian) must equal the eleven public inputs, and the trailing
        // digest slot (word 23) must be a non-zero, in-field Fr, and the
        // blob must be exactly [`WITHDRAWAL_CALLDATA_LEN`] bytes. A
        // pre-binding blob is 3 648 B and its word 23 is a G1 coordinate,
        // which still passes a zero / `< r` check. `None` skips the pin
        // compare: the only `Some` callers are tests, and that hex is a
        // little-endian Fr, not the big-endian `bytes32` `vkDigest()`
        // returns. The CLI compares those bytes itself before the dry-run.
        calldata_binds_instances(&calldata, &instances_hex, None)?;

        Ok(PartnerWithdrawalProof {
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
    // Length validated as an exact multiple of 32 above, so `as_chunks` leaves
    // an empty remainder (and satisfies `clippy::chunks_exact_to_as_chunks`).
    let (chunks, _rest) = bytes.as_chunks::<32>();
    Ok(chunks.iter().map(hex::encode).collect())
}

/// Assert that the aggregator calldata re-exposes exactly the eleven Circuit-4
/// public inputs: `calldata[(12+i)*32 .. (13+i)*32]` (big-endian EVM word)
/// numerically equals `instances_hex[i]` (little-endian Fr repr), for all i.
///
/// Also validates the trailing inner-VK digest slot (word 23, i.e.
/// `12 + WITHDRAWAL_PUBLIC_INPUTS`):
///   * the blob must be exactly [`WITHDRAWAL_CALLDATA_LEN`] bytes. A
///     pre-binding withdrawal blob is 3 648 B; its word 23 is the first word of
///     the outer proof (a G1 coordinate, below `r`), so a zero / `< r` check
///     does not tell it from this build;
///   * the word must be non-zero and strictly less than the BN254 scalar-field
///     modulus `r`.
///
/// If `expected_vk_digest_hex` is `Some(hex)`, word 23 must equal that
/// little-endian Fr. Tests are the only callers of `Some`. An on-chain
/// `vkDigest()` is a big-endian `bytes32`; do not pass it here without
/// reversing the limbs.
pub fn calldata_binds_instances(
    calldata: &[u8],
    instances_hex: &[String],
    expected_vk_digest_hex: Option<&str>,
) -> Result<(), RelayerError> {
    if calldata.len() != WITHDRAWAL_CALLDATA_LEN {
        return Err(RelayerError::other(format!(
            "aggregator calldata is {} bytes; this build's withdrawal artefact is exactly {} \
             bytes (24 instance words + the outer proof). {} bytes is the pre-binding blob, whose \
             word 23 is a proof coordinate rather than the inner-VK digest",
            calldata.len(),
            WITHDRAWAL_CALLDATA_LEN,
            3_648
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

    // Word 23: inner-VK digest slot. Sanity-check even without an expected
    // pin, so a stale-Yul or corrupted payload never reaches submit.
    let digest_off = (NUM_ACCUMULATOR_INSTANCES + WITHDRAWAL_PUBLIC_INPUTS) * 32;
    let digest_word = &calldata[digest_off..digest_off + 32];
    let digest = U256::from_be_slice(digest_word);
    if digest == U256::ZERO {
        return Err(RelayerError::other(
            "calldata vk_digest slot (word 23) is zero — aggregator did not emit an inner-VK \
             digest; regenerate the aggregator Yul + calldata",
        ));
    }
    if digest >= BN254_FR_MODULUS {
        return Err(RelayerError::other(format!(
            "calldata vk_digest slot (word 23) = {digest} is >= BN254 field modulus r — not a \
             valid Fr; the aggregator cannot have produced this word"
        )));
    }
    if let Some(expected_hex) = expected_vk_digest_hex {
        let expected = fr_hex_to_u256(expected_hex)?;
        if digest != expected {
            return Err(RelayerError::other(format!(
                "calldata vk_digest slot (word 23) = {digest} != expected on-chain vkDigest = \
                 {expected} — the aggregator was built against a different inner VK than the \
                 deployed adapter pins"
            )));
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────
// Mocks — deterministic, no subprocess
// ─────────────────────────────────────────────────────────────────────

/// Deterministic snark prover for tests: writes an empty `<name>.snark` and a
/// 352-byte instances file (eleven ascending LE Fr) into `snark_dir`.
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

/// Deterministic aggregator for tests: returns a [`WITHDRAWAL_CALLDATA_LEN`]
/// byte blob whose re-exposed instance words (12..=22 inclusive) match
/// [`MockCircuit4SnarkProver`]'s eleven ascending LE instances, so
/// [`calldata_binds_instances`] passes. The length is the production
/// Circuit-4 size, not a shorter stand-in: a 3 648 B pre-binding blob
/// must fail the exact-length check.
#[derive(Clone, Debug, Default)]
pub struct MockAggregator {
    pub fail: bool,
}

impl MockAggregator {
    /// Build calldata that binds the given LE-instance hex strings (big-endian
    /// words at positions `12..12 + instances_hex.len()`), padded to
    /// [`WITHDRAWAL_CALLDATA_LEN`]. The trailing digest slot at
    /// `12 + instances_hex.len()` is populated with [`Self::MOCK_VK_DIGEST`]
    /// so that [`calldata_binds_instances`]'s non-zero + in-field checks
    /// pass.
    pub fn calldata_binding(instances_hex: &[String]) -> Vec<u8> {
        let total_len = WITHDRAWAL_CALLDATA_LEN;
        let mut cd = vec![0u8; total_len];
        for (i, inst) in instances_hex.iter().enumerate() {
            let val = fr_hex_to_u256(inst).unwrap_or(U256::ZERO);
            let be = val.to_be_bytes::<32>();
            let off = (NUM_ACCUMULATOR_INSTANCES + i) * 32;
            cd[off..off + 32].copy_from_slice(&be);
        }
        // Digest slot: a stand-in, non-zero, in-field value.
        let digest_off = (NUM_ACCUMULATOR_INSTANCES + instances_hex.len()) * 32;
        if digest_off + 32 <= cd.len() {
            cd[digest_off..digest_off + 32]
                .copy_from_slice(&Self::MOCK_VK_DIGEST.to_be_bytes::<32>());
        }
        cd
    }

    /// Stand-in inner-VK digest used by [`Self::calldata_binding`]. Chosen
    /// arbitrarily; it just has to be a non-zero Fr less than
    /// [`BN254_FR_MODULUS`] so the mock passes the same word-23 checks the
    /// production pipeline enforces.
    pub const MOCK_VK_DIGEST: U256 = U256::from_limbs([0x0123_4567_89ab_cdef, 0, 0, 0]);
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

// ─────────────────────────────────────────────────────────────────────
// Circuit 1A/1B/2 (attestation + layer-hashes) — in-process snark wrap
// + subprocess aggregation
// ─────────────────────────────────────────────────────────────────────
//
// The AN-side daemon proves Circuits 1A/1B/2 with the transcript flavour
// selected in [`LiveProverConfig::transcript`]. `bridge-relayer-daemon`
// requests **Poseidon** there (the flavour the ETH aggregator consumes), so
// the ETH-side leg is now just:
//
//   1. Wrap the daemon's Poseidon proof bytes + partner VK into a
//      snark-verifier `Snark` (bincode) — done **in-process** via
//      [`bridge_snark_wrap::wrap_poseidon_snark_in_memory`]. This replaces the
//      old `export-1a1b2-poseidon-snark` subprocess, which independently
//      re-fetched from GraphQL and re-proved the same witness (a full second
//      Halo2 prove per bundle).
//   2. Feed the bincode Snark into `bridge-evm-aggregator`'s `aggregate-proof`
//      subprocess (same [`SubprocessAggregator`] used for C4, different
//      verifier name). This subprocess is still required — its `snark-verifier`
//      transitive graph resolves halo2-base to axiom's crate, which is
//      incompatible with the gosh fork the daemon links (mixing them in one
//      build unit does not compile).

/// Committed R15 aggregator verifier names (match the `.sol` / `.bin` pairs in
/// `contracts/ethereum/verifiers/`). Names line up with
/// `bridge_evm_aggregator::AggregatorConfig::for_verifier_name`.
pub const PRIMARY_VERIFIER_NAME: &str = "PrimaryAggregatorVerifier";
pub const FALLBACK_VERIFIER_NAME: &str = "FallbackAggregatorVerifier";
pub const LAYER_HASHES_VERIFIER_NAME: &str = "LayerHashesAggregatorVerifier";

/// Wrap a Poseidon-transcript Circuit 1A/1B/2 proof + peek fields into an
/// aggregator-ready snark-verifier `.snark` (bincode) tempfile.
///
/// Trait-shaped so [`crate::aggregated_source::AggregatedBlockSource`] stays
/// unit-testable without a real `params_dir` (VK + SRS + config on disk):
/// prod uses [`PoseidonSnarkWrapper`], tests use [`MockSnarkWrapper`].
#[async_trait]
pub trait SnarkWrapper: Send + Sync {
    /// Wrap a Circuit 1A/1B (attestation) Poseidon proof. `finalization`
    /// picks the partner VK (`primary_vk.bin` vs `fallback_vk.bin`).
    async fn wrap_attestation(
        &self,
        finalization: crate::types::FinalizationType,
        proof_bytes: &[u8],
        block_id_be: &[u8; 32],
        bk_set_commitment_be: &[u8; 32],
        block_seq_no: u64,
        last_seen: u32,
    ) -> Result<tempfile::NamedTempFile, RelayerError>;

    /// Wrap a Circuit 2 (layer hashes) Poseidon proof.
    async fn wrap_layer(
        &self,
        proof_bytes: &[u8],
        block_id_be: &[u8; 32],
        bk_set_commitment_be: &[u8; 32],
        num_layers: u8,
        layer_hashes_be: &[[u8; 32]; bridge_prover_lib::bridge_state::MAX_LAYERS],
        prev_max_level_layer_hash_be: &[u8; 32],
    ) -> Result<tempfile::NamedTempFile, RelayerError>;
}

/// Production wrapper backed by
/// [`bridge_snark_wrap::wrap_poseidon_snark_in_memory`].
///
/// Reads partner VKs from `<params_dir>/{primary,fallback,layer}_vk.bin`
/// and matching `*_config_params.json`. For the layer circuit, forces
/// `srs_k_override = Some(20)` — the layer VK was keygen'd against the
/// shared K=20 ceremony SRS while `layer_config_params.json` records `k=17`
/// (see `bridge_snark_wrap::wrap_poseidon_snark_in_memory` docs).
pub struct PoseidonSnarkWrapper {
    pub params_dir: PathBuf,
}

impl PoseidonSnarkWrapper {
    pub fn new(params_dir: impl Into<PathBuf>) -> Self {
        Self {
            params_dir: params_dir.into(),
        }
    }

    /// Reconstruct the 4-element Circuit 1A/1B public-instance vector.
    /// Mirrors the `instances` construction in `prove_primary` /
    /// `prove_fallback` in
    /// `bridge-snark-utils/src/bin/export_1a1b2_poseidon_snark.rs`.
    fn attestation_instances(
        block_id_be: &[u8; 32],
        bk_set_commitment_be: &[u8; 32],
        block_seq_no: u64,
        last_seen: u32,
    ) -> Result<Vec<bridge_prover_lib::Fr>, RelayerError> {
        let block_id_fr = bridge_prover_lib::ipc::fold_hash_be_to_fr(block_id_be);
        // `bk_set_commitment_be` is Fr::to_repr() (LE), despite the `_be`
        // naming — see project_block_id_representation_audit.md. Reuse
        // `bridge_prover_lib::ipc::fr_from_hex` (which internally calls
        // `Fr::from_repr` on 32 LE bytes) so we don't need `PrimeField` in scope.
        let bk_set_commitment_fr = bridge_prover_lib::ipc::fr_from_hex(&hex::encode(
            bk_set_commitment_be,
        ))
        .map_err(|e| {
            RelayerError::other(format!(
                "bk_set_commitment_be {} is not a canonical Fr repr: {e}",
                hex::encode(bk_set_commitment_be),
            ))
        })?;
        Ok(vec![
            block_id_fr,
            bk_set_commitment_fr,
            bridge_prover_lib::Fr::from(block_seq_no),
            bridge_prover_lib::Fr::from(last_seen as u64),
        ])
    }

    /// Reconstruct the 14-element Circuit 2 public-instance vector.
    /// Mirrors `prove_layer` in the export CLI.
    fn layer_instances(
        block_id_be: &[u8; 32],
        bk_set_commitment_be: &[u8; 32],
        num_layers: u8,
        layer_hashes_be: &[[u8; 32]; bridge_prover_lib::bridge_state::MAX_LAYERS],
        prev_max_level_layer_hash_be: &[u8; 32],
    ) -> Result<Vec<bridge_prover_lib::Fr>, RelayerError> {
        let block_id_fr = bridge_prover_lib::ipc::fold_hash_be_to_fr(block_id_be);
        let bk_set_commitment_fr = bridge_prover_lib::ipc::fr_from_hex(&hex::encode(
            bk_set_commitment_be,
        ))
        .map_err(|e| {
            RelayerError::other(format!(
                "bk_set_commitment_be {} is not a canonical Fr repr: {e}",
                hex::encode(bk_set_commitment_be),
            ))
        })?;
        let mut instances =
            Vec::with_capacity(bridge_prover_lib::layer_prover::LAYER_HASHES_NUM_PUBLIC_INPUTS);
        instances.push(block_id_fr);
        instances.push(bk_set_commitment_fr);
        instances.push(bridge_prover_lib::Fr::from(num_layers as u64));
        for (i, h) in layer_hashes_be.iter().enumerate() {
            let fr = bridge_prover_lib::ipc::fr_from_hex(&hex::encode(h)).map_err(|e| {
                RelayerError::other(format!(
                    "layer_hashes_be[{i}] {} is not a canonical Fr repr: {e}",
                    hex::encode(h),
                ))
            })?;
            instances.push(fr);
        }
        let prev_fr =
            bridge_prover_lib::ipc::fr_from_hex(&hex::encode(prev_max_level_layer_hash_be))
                .map_err(|e| {
                    RelayerError::other(format!(
                        "prev_max_level_layer_hash_be {} is not a canonical Fr repr: {e}",
                        hex::encode(prev_max_level_layer_hash_be),
                    ))
                })?;
        instances.push(prev_fr);
        Ok(instances)
    }

    fn wrap_to_tempfile(
        &self,
        key_prefix: &str,
        srs_k_override: Option<u32>,
        proof_bytes: &[u8],
        instances: &[bridge_prover_lib::Fr],
    ) -> Result<tempfile::NamedTempFile, RelayerError> {
        let vk_path = self.params_dir.join(format!("{key_prefix}_vk.bin"));
        let config_path = self
            .params_dir
            .join(format!("{key_prefix}_config_params.json"));
        let t_wrap = Instant::now();
        let bytes = bridge_snark_wrap::wrap_poseidon_snark_in_memory(
            &vk_path,
            &config_path,
            srs_k_override,
            proof_bytes,
            instances,
        )
        .map_err(|e| {
            RelayerError::other(format!(
                "wrap_poseidon_snark_in_memory({key_prefix}): {e:?}"
            ))
        })?;
        let wrap_ms = t_wrap.elapsed().as_millis() as u64;
        info!(
            "wrap_poseidon_snark_in_memory({key_prefix}) took {} ms, snark={} bytes",
            wrap_ms,
            bytes.len()
        );
        let file = tempfile::Builder::new()
            .prefix(&format!("{key_prefix}_snark_"))
            .suffix(".snark")
            .tempfile()
            .map_err(|e| RelayerError::other(format!("tempfile: {e}")))?;
        std::fs::write(file.path(), &bytes).map_err(|e| {
            RelayerError::other(format!(
                "write snark tempfile {}: {e}",
                file.path().display()
            ))
        })?;
        Ok(file)
    }
}

#[async_trait]
impl SnarkWrapper for PoseidonSnarkWrapper {
    async fn wrap_attestation(
        &self,
        finalization: crate::types::FinalizationType,
        proof_bytes: &[u8],
        block_id_be: &[u8; 32],
        bk_set_commitment_be: &[u8; 32],
        block_seq_no: u64,
        last_seen: u32,
    ) -> Result<tempfile::NamedTempFile, RelayerError> {
        let instances = Self::attestation_instances(
            block_id_be,
            bk_set_commitment_be,
            block_seq_no,
            last_seen,
        )?;
        let key_prefix = match finalization {
            crate::types::FinalizationType::Primary => "primary",
            crate::types::FinalizationType::Fallback => "fallback",
        };
        self.wrap_to_tempfile(key_prefix, None, proof_bytes, &instances)
    }

    async fn wrap_layer(
        &self,
        proof_bytes: &[u8],
        block_id_be: &[u8; 32],
        bk_set_commitment_be: &[u8; 32],
        num_layers: u8,
        layer_hashes_be: &[[u8; 32]; bridge_prover_lib::bridge_state::MAX_LAYERS],
        prev_max_level_layer_hash_be: &[u8; 32],
    ) -> Result<tempfile::NamedTempFile, RelayerError> {
        let instances = Self::layer_instances(
            block_id_be,
            bk_set_commitment_be,
            num_layers,
            layer_hashes_be,
            prev_max_level_layer_hash_be,
        )?;
        // K=20 SRS override — see PoseidonSnarkWrapper doc.
        self.wrap_to_tempfile("layer", Some(20), proof_bytes, &instances)
    }
}

/// Compose a [`SnarkWrapper`] and a [`ProofAggregator`] into the full
/// ETH-side attestation / layer-hashes path. Unlike
/// [`Circuit4ShplonkPipeline`] the return type is just the aggregator
/// calldata: the caller swaps it into the pre-existing
/// [`crate::types::AnBlockData::attestation_proof`] /
/// [`crate::types::AnBlockData::layer_hashes_proof`] fields (or
/// [`crate::types::BkSetUpdateData::attestation_proof`] on the BK-update
/// lane).
pub struct Circuit12ShplonkPipeline<W: SnarkWrapper, A: ProofAggregator> {
    pub wrapper: W,
    pub aggregator: A,
}

impl<W: SnarkWrapper, A: ProofAggregator> Circuit12ShplonkPipeline<W, A> {
    pub fn new(wrapper: W, aggregator: A) -> Self {
        Self {
            wrapper,
            aggregator,
        }
    }

    /// Wrap one attestation proof (1A or 1B), aggregate, and return the
    /// EVM calldata `instances ‖ proof`.
    pub async fn aggregate_attestation(
        &self,
        finalization: crate::types::FinalizationType,
        proof_bytes: &[u8],
        block_id_be: &[u8; 32],
        bk_set_commitment_be: &[u8; 32],
        block_seq_no: u64,
        last_seen: u32,
    ) -> Result<Vec<u8>, RelayerError> {
        let t_total = Instant::now();
        let snark = self
            .wrapper
            .wrap_attestation(
                finalization,
                proof_bytes,
                block_id_be,
                bk_set_commitment_be,
                block_seq_no,
                last_seen,
            )
            .await?;
        let verifier_name = match finalization {
            crate::types::FinalizationType::Primary => PRIMARY_VERIFIER_NAME,
            crate::types::FinalizationType::Fallback => FALLBACK_VERIFIER_NAME,
        };
        let calldata = self
            .aggregator
            .aggregate(snark.path(), verifier_name)
            .await?;
        info!(
            "aggregate_attestation ({verifier_name}) total {} ms, calldata={} bytes",
            t_total.elapsed().as_millis(),
            calldata.len()
        );
        Ok(calldata)
    }

    /// Wrap the Circuit 2 layer proof, aggregate, and return EVM calldata.
    pub async fn aggregate_layer(
        &self,
        proof_bytes: &[u8],
        block_id_be: &[u8; 32],
        bk_set_commitment_be: &[u8; 32],
        num_layers: u8,
        layer_hashes_be: &[[u8; 32]; bridge_prover_lib::bridge_state::MAX_LAYERS],
        prev_max_level_layer_hash_be: &[u8; 32],
    ) -> Result<Vec<u8>, RelayerError> {
        let t_total = Instant::now();
        let snark = self
            .wrapper
            .wrap_layer(
                proof_bytes,
                block_id_be,
                bk_set_commitment_be,
                num_layers,
                layer_hashes_be,
                prev_max_level_layer_hash_be,
            )
            .await?;
        let calldata = self
            .aggregator
            .aggregate(snark.path(), LAYER_HASHES_VERIFIER_NAME)
            .await?;
        info!(
            "aggregate_layer ({LAYER_HASHES_VERIFIER_NAME}) total {} ms, calldata={} bytes",
            t_total.elapsed().as_millis(),
            calldata.len()
        );
        Ok(calldata)
    }
}

/// Deterministic wrapper for tests: skips the real snark-verifier wrap
/// (which needs a real params_dir with VK + SRS + config) and just writes
/// a placeholder tempfile. Downstream [`MockAggregator`] ignores contents
/// and synthesizes calldata directly.
#[derive(Clone, Debug, Default)]
pub struct MockSnarkWrapper {
    pub fail: bool,
}

#[async_trait]
impl SnarkWrapper for MockSnarkWrapper {
    async fn wrap_attestation(
        &self,
        _finalization: crate::types::FinalizationType,
        _proof_bytes: &[u8],
        _block_id_be: &[u8; 32],
        _bk_set_commitment_be: &[u8; 32],
        _block_seq_no: u64,
        _last_seen: u32,
    ) -> Result<tempfile::NamedTempFile, RelayerError> {
        if self.fail {
            return Err(RelayerError::other("mock snark wrapper configured to fail"));
        }
        let file = tempfile::Builder::new()
            .prefix("mock_attestation_snark_")
            .suffix(".snark")
            .tempfile()
            .map_err(|e| RelayerError::other(format!("tempfile: {e}")))?;
        std::fs::write(file.path(), b"mock-attestation-snark")
            .map_err(|e| RelayerError::other(format!("write mock snark: {e}")))?;
        Ok(file)
    }

    async fn wrap_layer(
        &self,
        _proof_bytes: &[u8],
        _block_id_be: &[u8; 32],
        _bk_set_commitment_be: &[u8; 32],
        _num_layers: u8,
        _layer_hashes_be: &[[u8; 32]; bridge_prover_lib::bridge_state::MAX_LAYERS],
        _prev_max_level_layer_hash_be: &[u8; 32],
    ) -> Result<tempfile::NamedTempFile, RelayerError> {
        if self.fail {
            return Err(RelayerError::other("mock snark wrapper configured to fail"));
        }
        let file = tempfile::Builder::new()
            .prefix("mock_layer_snark_")
            .suffix(".snark")
            .tempfile()
            .map_err(|e| RelayerError::other(format!("tempfile: {e}")))?;
        std::fs::write(file.path(), b"mock-layer-snark")
            .map_err(|e| RelayerError::other(format!("write mock snark: {e}")))?;
        Ok(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn subprocess_aggregate_args_include_pk_cache_when_set() {
        let cfg = SubprocessAggregatorConfig::new("/agg", "/verifiers", "/params")
            .with_pk_cache_dir("/params/pk_cache");
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
            "--pk-cache-dir",
            "/params/pk_cache",
        ]);
    }

    #[test]
    fn subprocess_aggregate_args_include_the_slot_report_when_set() {
        let mut cfg = SubprocessAggregatorConfig::new("/agg", "/verifiers", "/params");
        cfg.slot_report = Some(PathBuf::from("/snark/aggregator-slot"));
        let agg = SubprocessAggregator {
            config: cfg,
        };
        let args = agg.args(
            Path::new("/s/circuit4.snark"),
            WITHDRAWAL_VERIFIER_NAME,
            Path::new("/tmp/out.bin"),
        );
        assert_eq!(&args[args.len() - 2..], [
            "--slot-report",
            "/snark/aggregator-slot"
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
        assert!(calldata_binds_instances(&cd, &instances, None).is_ok());

        // Corrupt one bound word → mismatch detected.
        let mut bad = cd.clone();
        let off = (NUM_ACCUMULATOR_INSTANCES + 3) * 32;
        bad[off + 31] ^= 0xFF;
        assert!(calldata_binds_instances(&bad, &instances, None).is_err());

        // Too-short calldata → error. 3 648 B is the pre-binding withdrawal
        // blob: longer than the instance prefix, still not this build.
        assert!(calldata_binds_instances(&[0u8; 100], &instances, None).is_err());
        assert!(calldata_binds_instances(&[0u8; 3_648], &instances, None).is_err());
    }

    /// Word 23 (the inner-VK digest slot) must be non-zero and in-field, and
    /// — when an expected digest is supplied — must match it exactly. Guards
    /// against a stale-Yul aggregator that forgot to emit the digest, a
    /// corrupted payload that landed a non-Fr word there, and a same-shape
    /// inner-VK rotation whose digest no longer matches the on-chain pin.
    #[test]
    fn calldata_binds_validates_vk_digest_slot() {
        let instances: Vec<String> = (0..WITHDRAWAL_PUBLIC_INPUTS as u8)
            .map(|i| {
                let mut le = [0u8; 32];
                le[0] = i + 1;
                hex::encode(le)
            })
            .collect();
        let cd = MockAggregator::calldata_binding(&instances);
        let digest_off = (NUM_ACCUMULATOR_INSTANCES + WITHDRAWAL_PUBLIC_INPUTS) * 32;

        // Zero digest → error.
        let mut zeroed = cd.clone();
        zeroed[digest_off..digest_off + 32].fill(0);
        assert!(calldata_binds_instances(&zeroed, &instances, None).is_err());

        // Word >= r (all 0xff) → error.
        let mut oversize = cd.clone();
        oversize[digest_off..digest_off + 32].fill(0xff);
        assert!(calldata_binds_instances(&oversize, &instances, None).is_err());

        // Word == r rejects; word == r − 1 accepts. `>=` flipped to `>` would
        // still reject 0xff and would miss this boundary. Limb order is the
        // big-endian word the aggregator writes.
        let mut at_r = cd.clone();
        at_r[digest_off..digest_off + 32].copy_from_slice(&BN254_FR_MODULUS.to_be_bytes::<32>());
        assert!(calldata_binds_instances(&at_r, &instances, None).is_err());
        let mut below_r = cd.clone();
        below_r[digest_off..digest_off + 32]
            .copy_from_slice(&(BN254_FR_MODULUS - U256::from(1u64)).to_be_bytes::<32>());
        assert!(calldata_binds_instances(&below_r, &instances, None).is_ok());

        // Exact-match against expected digest.
        let mock_digest_le: String =
            hex::encode(MockAggregator::MOCK_VK_DIGEST.to_le_bytes::<32>());
        assert!(
            calldata_binds_instances(&cd, &instances, Some(&mock_digest_le)).is_ok(),
            "matching expected digest must pass"
        );

        // Wrong expected digest → error.
        let wrong_le =
            hex::encode((MockAggregator::MOCK_VK_DIGEST ^ U256::from(1u64)).to_le_bytes::<32>());
        assert!(
            calldata_binds_instances(&cd, &instances, Some(&wrong_le)).is_err(),
            "mismatched expected digest must fail"
        );
    }

    #[tokio::test]
    async fn a_timed_out_aggregator_is_killed_not_orphaned() {
        // The CLI holds its prove lock only while it waits for this
        // process. A timeout that dropped the child without killing it left
        // the aggregator running, unlocked, beside the next run's proof.
        //
        // The fake publishes its pid and `exec`s, so the pid is the process
        // that has to be gone; `exec` matters, without it `sh` forks `sleep`
        // and only the shell would be killed.
        let dir = tempfile::TempDir::new().unwrap();
        let bin_dir = dir.path().join("target/release");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let pidfile = dir.path().join("child.pid");
        let bin = bin_dir.join(AGGREGATE_BIN);
        install_script(
            &bin,
            &format!(
                "#!/bin/sh\necho 'waiting for another keygen' >&2\necho $$ > {p}.tmp\nmv {p}.tmp \
                 {p}\nexec sleep 120\n",
                p = pidfile.display()
            ),
        );
        let mut cfg = SubprocessAggregatorConfig::new(dir.path(), dir.path(), dir.path());
        cfg.timeout = Duration::from_secs(5);
        cfg.timeout_flag = Some("--prover-timeout-s");
        let agg = SubprocessAggregator::new(cfg);

        let err = agg
            .aggregate(&dir.path().join("inner.snark"), WITHDRAWAL_VERIFIER_NAME)
            .await
            .expect_err("an aggregator that never finishes times out");
        assert!(err.to_string().contains("timed out"), "{err}");
        // A cold run keygens before it proves, and may wait for another
        // process's keygen first: a retry with the same budget times out the
        // same way.
        assert!(err.to_string().contains("--prover-timeout-s"), "{err}");

        // On a loaded host the shell may not have reached its first line
        // when the timeout fired. Killed before it wrote its pid, it was
        // not orphaned either.
        let mut pid = None;
        for _ in 0..100 {
            if let Ok(p) = std::fs::read_to_string(&pidfile) {
                pid = Some(p.trim().to_string());
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let Some(pid) = pid else {
            return;
        };
        // It said what it was doing before it wrote its pid, and that is
        // quoted.
        assert!(
            err.to_string().contains("waiting for another keygen"),
            "{err}"
        );
        // SIGKILL and reaping are not instantaneous: poll. A zombie counts as
        // gone — it holds no memory and writes nothing.
        let mut gone = false;
        for _ in 0..50 {
            match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Err(_) => gone = true,
                Ok(stat) => {
                    gone = stat
                        .rsplit(')')
                        .next()
                        .is_some_and(|rest| rest.trim_start().starts_with('Z'))
                },
            }
            if gone {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if !gone {
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid])
                .status();
        }
        assert!(
            gone,
            "pid {pid} survived the timeout: the aggregator outlives the run that started it"
        );
    }

    /// A fake `aggregate-proof` under `dir` that writes `lines` progress
    /// lines to stderr and one to stdout, then writes calldata to `--out`
    /// and exits 0 — or exits 1 without it when `fail`.
    fn chatty_aggregator(dir: &Path, lines: usize, fail: bool) {
        let bin_dir = dir.join("target/release");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin = bin_dir.join(AGGREGATE_BIN);
        let end = if fail {
            "echo 'Error: the last word' >&2\nexit 1\n"
        } else {
            "while [ \"$1\" != --out ]; do shift; done\nprintf calldata > \"$2\"\n"
        };
        install_script(
            &bin,
            &format!(
                "#!/bin/sh\ni=1\nwhile [ $i -le {lines} ]; do echo \"progress line $i\" >&2; \
                 i=$((i+1)); done\necho 'Aggregator shape match'\n{end}"
            ),
        );
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
        let copied = std::process::Command::new("cp")
            .arg(&source)
            .arg(bin)
            .status()
            .unwrap();
        assert!(copied.success());
    }

    /// What is logged on this thread, from now until the guard is dropped.
    #[derive(Clone, Default)]
    struct Sink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Sink {
        fn capture() -> (Self, tracing::subscriber::DefaultGuard) {
            let sink = Sink::default();
            let writer = sink.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(move || writer.clone())
                .with_max_level(tracing::Level::TRACE)
                .with_ansi(false)
                .finish();
            (sink, tracing::subscriber::set_default(subscriber))
        }

        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    /// Everything logged on this thread while `f` runs, which a broken
    /// forwarder turns into a failure rather than a hang.
    async fn logged<T>(f: impl std::future::Future<Output = T>) -> (T, String) {
        let (sink, _guard) = Sink::capture();
        let out = tokio::time::timeout(Duration::from_secs(60), f)
            .await
            .expect("the aggregator call ends");
        (out, sink.text())
    }

    /// A config for a fake aggregator under `dir`, with a budget a broken
    /// test runs into well before anyone waits for it.
    fn fake_config(dir: &Path) -> SubprocessAggregatorConfig {
        let mut cfg = SubprocessAggregatorConfig::new(dir, dir, dir);
        cfg.timeout = Duration::from_secs(30);
        cfg
    }

    #[tokio::test]
    async fn the_aggregators_output_reaches_the_log_whole() {
        let dir = tempfile::TempDir::new().unwrap();
        chatty_aggregator(dir.path(), 5000, false);
        let agg = SubprocessAggregator::new(fake_config(dir.path()));
        let (calldata, log) = logged(agg.aggregate(&dir.path().join("in.snark"), "V")).await;
        assert_eq!(
            calldata.unwrap(),
            b"calldata",
            "more than a pipe holds does not stall it"
        );
        assert!(log.contains("aggregate-proof: progress line 1\n"), "{log}");
        assert!(log.contains("aggregate-proof: progress line 5000\n"));
        assert!(log.contains("aggregate-proof: Aggregator shape match"));
    }

    #[tokio::test]
    async fn the_aggregators_progress_reaches_the_log_while_it_runs() {
        // The pk cache's notices — waiting for another keygen, say — are for
        // the operator while the run is still going, not once it is over.
        let dir = tempfile::TempDir::new().unwrap();
        let go = dir.path().join("go");
        install_script(
            &dir.path().join("target/release").join(AGGREGATE_BIN),
            &format!(
                "#!/bin/sh\necho ' WARN bridge_evm_aggregator::cache: waiting for another keygen' \
                 >&2\nwhile [ ! -e {go} ]; do sleep 0.05; done\nwhile [ \"$1\" != --out ]; do \
                 shift; done\nprintf calldata > \"$2\"\n",
                go = go.display()
            ),
        );
        let agg = SubprocessAggregator::new(fake_config(dir.path()));
        let snark = dir.path().join("in.snark");
        let (sink, _guard) = Sink::capture();
        let watch = async {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !sink.text().contains("waiting for another keygen") {
                if Instant::now() > deadline {
                    std::fs::write(&go, b"").unwrap();
                    panic!("nothing was forwarded while the aggregator ran");
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            std::fs::write(&go, b"").unwrap();
        };
        let (calldata, ()) = tokio::time::timeout(Duration::from_secs(60), async {
            tokio::join!(agg.aggregate(&snark, "V"), watch)
        })
        .await
        .expect("no hang");
        assert_eq!(calldata.unwrap(), b"calldata");
        assert!(
            sink.text()
                .contains("WARN bridge_relayer_daemon::aggregator: aggregate-proof:  WARN"),
            "logged at the level it was printed with: {}",
            sink.text()
        );
    }

    #[tokio::test]
    async fn a_failed_aggregator_is_quoted_by_its_error() {
        // The whole of its stderr is in the log already.
        let dir = tempfile::TempDir::new().unwrap();
        chatty_aggregator(dir.path(), 5000, true);
        let agg = SubprocessAggregator::new(fake_config(dir.path()));
        let (err, log) = logged(agg.aggregate(&dir.path().join("in.snark"), "V")).await;
        let err = err.expect_err("exit 1").to_string();
        assert!(err.contains("Error: the last word"), "{err}");
        assert!(!err.contains("progress line"), "{err}");
        assert!(
            log.contains("ERROR bridge_relayer_daemon::aggregator: aggregate-proof: Error:"),
            "{log}"
        );
    }

    #[test]
    fn the_lines_under_an_error_belong_to_it() {
        let mut said = Said::default();
        assert!(!said.push(" INFO aggregate_proof: started"));
        assert!(said.push("Error: aggregate + evm-proof"));
        assert!(said.push("Caused by:"), "logged at error, not info");
        assert!(said.push("    cached aggregator key /c/S.pk is damaged"));
        assert!(!said.push("Stack backtrace:"));
    }

    #[test]
    fn a_failure_quotes_its_error_and_not_its_backtrace() {
        let mut said = Said::default();
        for i in 0..100 {
            said.push(&format!("progress line {i}"));
        }
        assert!(
            said.quote().starts_with("progress line 60\n"),
            "no error: the last {QUOTED_LINES} lines"
        );
        for line in [
            "Error: aggregate + evm-proof (aggregate_and_prove_cached)",
            "",
            "Caused by:",
            "    cached aggregator key /c/S.pk is damaged",
            "",
            "Stack backtrace:",
        ] {
            said.push(line);
        }
        for i in 0..100 {
            said.push(&format!("  {i}: std::backtrace::Backtrace::create"));
        }
        let quote = said.quote();
        assert!(quote.contains("is damaged"), "{quote}");
        assert!(!quote.contains("Backtrace::create"), "{quote}");
    }

    #[test]
    fn the_aggregator_logs_at_its_own_level_whatever_rust_log_says_here() {
        // A RUST_LOG written for this process — `ackinacki_bridge=info` —
        // names none of the aggregator's targets and would silence it.
        let agg = SubprocessAggregator::new(fake_config(Path::new("/agg")));
        let cmd = agg.command(&[]);
        assert!(
            cmd.as_std()
                .get_envs()
                .any(|(k, v)| k == "RUST_LOG" && v.is_none()),
            "RUST_LOG is removed"
        );
    }

    #[test]
    fn a_timeout_names_the_flag_that_raises_it_only_where_there_is_one() {
        let mut cfg = SubprocessAggregatorConfig::new("/agg", "/verifiers", "/params");
        let fixed = SubprocessAggregator {
            config: cfg.clone(),
        }
        .timed_out(WITHDRAWAL_VERIFIER_NAME);
        assert!(fixed.contains("timed out after 1800s"), "{fixed}");
        assert!(fixed.contains("~2 min"), "the withdrawal figure: {fixed}");
        assert!(
            !fixed.contains("--"),
            "this command has no flag for it: {fixed}"
        );
        cfg.timeout_flag = Some("--prover-timeout-s");
        let flagged = SubprocessAggregator {
            config: cfg.clone(),
        }
        .timed_out(WITHDRAWAL_VERIFIER_NAME);
        assert!(
            flagged.contains("raise it with --prover-timeout-s"),
            "{flagged}"
        );
        let block = SubprocessAggregator {
            config: cfg,
        }
        .timed_out("BridgeBlockAggregatorVerifier");
        assert!(
            !block.contains("min"),
            "no measured figure for another aggregator: {block}"
        );
    }

    #[test]
    fn each_line_is_logged_at_the_level_it_was_printed_with() {
        use tracing::Level;
        for (line, level) in [
            (
                " INFO bridge_evm_aggregator::cache: aggregator PK cache HIT",
                Level::INFO,
            ),
            (
                " WARN bridge_evm_aggregator::cache: removed a partial",
                Level::WARN,
            ),
            ("ERROR aggregate_proof: something", Level::ERROR),
            ("DEBUG halo2: detail", Level::DEBUG),
            ("TRACE halo2: more detail", Level::TRACE),
            ("Error: aggregate + evm-proof", Level::ERROR),
            ("thread 'main' panicked at src/x.rs:1:1:", Level::ERROR),
            ("Aggregator shape match: regenerated", Level::INFO),
            ("", Level::INFO),
        ] {
            assert_eq!(level_of(line), level, "{line:?}");
        }
    }

    #[tokio::test]
    async fn output_held_open_past_the_grace_keeps_what_it_said() {
        // Something the aggregator started can hold its pipe open after it
        // ended: what it said before then is still quoted.
        use tokio::io::AsyncWriteExt as _;
        let (mut writer, reader) = tokio::io::duplex(64);
        let forwarded = forward(Some(reader));
        writer.write_all(b"Error: the last word\n").await.unwrap();
        let said = tokio::time::timeout(
            Duration::from_secs(30),
            collected(forwarded, Duration::from_millis(200)),
        )
        .await
        .expect("the grace bounds the wait");
        assert_eq!(said.quote(), "Error: the last word");
        drop(writer);
    }

    #[test]
    fn a_relative_slot_report_is_made_absolute() {
        // The aggregator runs in `aggregator_dir`: a relative `--snark-dir`
        // — the CLI's default — would put the report where nobody reads it.
        let mut cfg = SubprocessAggregatorConfig::new("/agg", "/verifiers", "/params");
        cfg.slot_report = Some(PathBuf::from("shplonk-snark/aggregator-slot"));
        let agg = SubprocessAggregator::new(cfg);
        let args = agg.args(
            Path::new("/s/circuit4.snark"),
            WITHDRAWAL_VERIFIER_NAME,
            Path::new("/tmp/out.bin"),
        );
        assert_eq!(
            PathBuf::from(&args[args.len() - 1]),
            std::env::current_dir()
                .unwrap()
                .join("shplonk-snark/aggregator-slot")
        );
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
        assert_eq!(bytes.len(), WITHDRAWAL_CALLDATA_LEN);
        // The eleven public inputs decode into a well-formed struct.
        // MockCircuit4SnarkProver writes byte `i` for slot `i` (see :636-640),
        // so slot 0 (`token_id`) decodes as 0 and slot 10 (`anchor_layer`) as 10.
        // Slot 10 is the newest addition; guarding it here means a slot-swap
        // that reordered the last two instances trips this pipeline test, not
        // just downstream JSON consumers.
        let pi = proof.public_inputs().unwrap();
        assert_eq!(pi.token_id, U256::ZERO);
        assert_eq!(pi.anchor_layer, U256::from(10u64));
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

    #[tokio::test]
    async fn mock_c12_pipeline_attestation_returns_aggregated_calldata() {
        // MockSnarkWrapper writes a placeholder snark tempfile; MockAggregator
        // ignores the file bytes and synthesizes a 3680-byte calldata. Verifies
        // the full wrap → aggregate wiring end-to-end without needing a real
        // params_dir on disk.
        let pipeline =
            Circuit12ShplonkPipeline::new(MockSnarkWrapper::default(), MockAggregator::default());
        let cd = pipeline
            .aggregate_attestation(
                crate::types::FinalizationType::Primary,
                b"proof",
                &[0u8; 32],
                &[0u8; 32],
                1_084_416,
                1_083_904,
            )
            .await
            .unwrap();
        assert_eq!(cd.len(), WITHDRAWAL_CALLDATA_LEN);
    }

    #[tokio::test]
    async fn mock_c12_pipeline_layer_returns_aggregated_calldata() {
        let pipeline =
            Circuit12ShplonkPipeline::new(MockSnarkWrapper::default(), MockAggregator::default());
        let cd = pipeline
            .aggregate_layer(
                b"proof",
                &[0u8; 32],
                &[0u8; 32],
                1,
                &[[0u8; 32]; bridge_prover_lib::bridge_state::MAX_LAYERS],
                &[0u8; 32],
            )
            .await
            .unwrap();
        assert_eq!(cd.len(), WITHDRAWAL_CALLDATA_LEN);
    }

    #[tokio::test]
    async fn mock_c12_pipeline_propagates_wrapper_failure() {
        let pipeline = Circuit12ShplonkPipeline::new(
            MockSnarkWrapper {
                fail: true,
            },
            MockAggregator::default(),
        );
        let res = pipeline
            .aggregate_attestation(
                crate::types::FinalizationType::Primary,
                b"proof",
                &[0u8; 32],
                &[0u8; 32],
                42,
                0,
            )
            .await;
        assert!(res.is_err());
    }
}
