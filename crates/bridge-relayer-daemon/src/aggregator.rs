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
    time::{Duration, Instant},
};

use alloy::primitives::U256;
use async_trait::async_trait;
use tracing::info;

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
///   2. Provision `params/kzg_bn254_{event_k}.srs` if missing — downsized
///      from the fallback K=21 ceremony SRS so g2/s_g2 stay Hermez-anchored.
///   3. Load event PK.
///   4. `generate_event_proof_with_transcript(&km, &witness, Poseidon)`.
///   5. Native `verify_event_proof_with_transcript` self-check.
///   6. Save 10-field-element instances as flat LE-Fr bytes (`.instances.bin`).
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
        Self { params_dir }
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
    std::fs::write(output_path, bytes).map_err(|e| {
        RelayerError::other(format!(
            "write instances {}: {e}",
            output_path.display()
        ))
    })
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

        let artefacts = tokio::task::spawn_blocking(move || -> Result<SnarkArtefacts, RelayerError> {
            // KeyManager owns four per-circuit sub-managers; the event sub-manager
            // keygens at K=19 with its own degree-matched SRS.
            let mut km = KeyManager::new(&params_dir);
            km.ensure_event_keys()
                .map_err(|e| RelayerError::other(format!("ensure_event_keys: {e}")))?;

            let event_k = km.event_config().k as u32;
            ensure_srs_for_event(&km, &params_dir, event_k)?;

            km.load_event_pk()
                .map_err(|e| RelayerError::other(format!("load_event_pk: {e}")))?;

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
            .map_err(|e| RelayerError::other(format!("Circuit 4 Poseidon prove: {e}")))?;

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
                    "Circuit 4 Poseidon inner snark failed native self-verification — refusing \
                     to emit an invalid snark. Regenerate event keys against the current \
                     circuit shape.",
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
            .map_err(|e| RelayerError::other(format!("wrap Poseidon snark: {e}")))?;

            let snark_path = snark_dir.join(format!("{name}.snark"));
            std::fs::write(&snark_path, &snark_bytes).map_err(|e| {
                RelayerError::other(format!("write snark {}: {e}", snark_path.display()))
            })?;

            Ok(SnarkArtefacts { snark_path, instances_path })
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
    /// Directory of committed verifier `.bin` files (the self-check target).
    /// Passed as `--verifiers-dir`.
    pub verifiers_dir: PathBuf,
    /// Directory holding `kzg_bn254_21.srs` (the outer SRS). Exported as
    /// `PARAMS_DIR` for the subprocess so `gen_srs(21)` finds the ceremony
    /// file.
    pub params_dir: PathBuf,
    /// Hard timeout for the (K=21) aggregation run.
    pub timeout: Duration,
    /// Persistent outer PK cache directory. When set, forwarded to
    /// `aggregate-proof` as `--pk-cache-dir`, which memoises the K=21 outer
    /// keygen. First bundle against a fresh slot still pays the full ~3–5 min
    /// keygen; subsequent bundles hit the on-disk PK (~15–60 s). Without this,
    /// every bundle re-keygens from scratch — see `aggregator_cache.rs`.
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
            pk_cache_dir: None,
        }
    }

    /// Enable persistent outer-PK caching across `aggregate-proof` invocations.
    pub fn with_pk_cache_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.pk_cache_dir = Some(dir.into());
        self
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

        let t_agg = Instant::now();
        let output = tokio::time::timeout(self.config.timeout, cmd.output())
            .await
            .map_err(|_| {
                RelayerError::other(format!(
                    "{AGGREGATE_BIN} timed out after {:?}",
                    self.config.timeout
                ))
            })?
            .map_err(|e| RelayerError::other(format!("failed to spawn {AGGREGATE_BIN}: {e}")))?;
        let agg_subprocess_ms = t_agg.elapsed().as_millis() as u64;

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
//      [`bridge_snark_wrap::wrap_poseidon_snark_in_memory`]. This replaces
//      the old `export-1a1b2-poseidon-snark` subprocess, which independently
//      re-fetched from GraphQL and re-proved the same witness (a full second
//      Halo2 prove per bundle).
//   2. Feed the bincode Snark into `bridge-evm-aggregator`'s `aggregate-proof`
//      subprocess (same [`SubprocessAggregator`] used for C4, different
//      verifier name). This subprocess is still required — its `snark-verifier`
//      transitive graph resolves halo2-base to axiom's crate, which is
//      incompatible with the gosh fork the daemon links (mixing them in one
//      build unit does not compile).

/// Committed R15 aggregator verifier names (match the `.bin` files in
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
        layer_hashes_be: &[[u8; 32];
                 bridge_prover_lib::bridge_state::MAX_LAYERS],
        prev_max_level_layer_hash_be: &[u8; 32],
    ) -> Result<tempfile::NamedTempFile, RelayerError>;
}

/// Production wrapper backed by [`bridge_snark_wrap::wrap_poseidon_snark_in_memory`].
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
    /// `prove_fallback` in `bridge-snark-utils/src/bin/export_1a1b2_poseidon_snark.rs`.
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
        let bk_set_commitment_fr = bridge_prover_lib::ipc::fr_from_hex(
            &hex::encode(bk_set_commitment_be),
        )
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
        layer_hashes_be: &[[u8; 32];
                 bridge_prover_lib::bridge_state::MAX_LAYERS],
        prev_max_level_layer_hash_be: &[u8; 32],
    ) -> Result<Vec<bridge_prover_lib::Fr>, RelayerError> {
        let block_id_fr = bridge_prover_lib::ipc::fold_hash_be_to_fr(block_id_be);
        let bk_set_commitment_fr = bridge_prover_lib::ipc::fr_from_hex(
            &hex::encode(bk_set_commitment_be),
        )
        .map_err(|e| {
            RelayerError::other(format!(
                "bk_set_commitment_be {} is not a canonical Fr repr: {e}",
                hex::encode(bk_set_commitment_be),
            ))
        })?;
        let mut instances = Vec::with_capacity(
            bridge_prover_lib::layer_prover::LAYER_HASHES_NUM_PUBLIC_INPUTS,
        );
        instances.push(block_id_fr);
        instances.push(bk_set_commitment_fr);
        instances.push(bridge_prover_lib::Fr::from(num_layers as u64));
        for (i, h) in layer_hashes_be.iter().enumerate() {
            let fr = bridge_prover_lib::ipc::fr_from_hex(&hex::encode(h))
                .map_err(|e| {
                    RelayerError::other(format!(
                        "layer_hashes_be[{i}] {} is not a canonical Fr repr: {e}",
                        hex::encode(h),
                    ))
                })?;
            instances.push(fr);
        }
        let prev_fr = bridge_prover_lib::ipc::fr_from_hex(
            &hex::encode(prev_max_level_layer_hash_be),
        )
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
            RelayerError::other(format!("wrap_poseidon_snark_in_memory({key_prefix}): {e:?}"))
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
            RelayerError::other(format!("write snark tempfile {}: {e}", file.path().display()))
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
        layer_hashes_be: &[[u8; 32];
                 bridge_prover_lib::bridge_state::MAX_LAYERS],
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
        let calldata = self.aggregator.aggregate(snark.path(), verifier_name).await?;
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
        layer_hashes_be: &[[u8; 32];
                 bridge_prover_lib::bridge_state::MAX_LAYERS],
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
        _layer_hashes_be: &[[u8; 32];
                 bridge_prover_lib::bridge_state::MAX_LAYERS],
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

    #[tokio::test]
    async fn mock_c12_pipeline_attestation_returns_aggregated_calldata() {
        // MockSnarkWrapper writes a placeholder snark tempfile; MockAggregator
        // ignores the file bytes and synthesizes 3616-byte calldata. Verifies
        // the full wrap → aggregate wiring end-to-end without needing a real
        // params_dir on disk.
        let pipeline = Circuit12ShplonkPipeline::new(
            MockSnarkWrapper::default(),
            MockAggregator::default(),
        );
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
        assert_eq!(cd.len(), 3616);
    }

    #[tokio::test]
    async fn mock_c12_pipeline_layer_returns_aggregated_calldata() {
        let pipeline = Circuit12ShplonkPipeline::new(
            MockSnarkWrapper::default(),
            MockAggregator::default(),
        );
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
        assert_eq!(cd.len(), 3616);
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
