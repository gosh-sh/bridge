//! `bridge-event-prove` — one-shot Circuit 4 proof generator.
//!
//! `generate_withdrawals_with_live_event_proving.py` (Track D4)
//! will invoke this binary once per `WithdrawalInitiated` event.
//!
//! ### Modes
//!
//! * `--fixture <path>` — read a `PrivateWitness` JSON (produced by
//!   Track D1's `bridge-event-witness-builder` binary, now shipped from
//!   the `bridge-event-witness` crate) and prove it.
//! * `--selftest` — synthesise inputs via
//!   `bridge-event-prove-circuit::test_helpers::build_synthetic_event_keygen_inputs`
//!   and prove them. Useful for smoke-testing the install (keygen → prove
//!   → verify roundtrip) without needing a live node.
//!
//! ### Output
//!
//! Always prints a single-line JSON summary as the **last non-empty line**
//! of stdout. If `--out-dir` is supplied, also writes
//! `proof_event_{NNN:06}.json` to that directory using a separate seqno
//! space from `bridge-prover-daemon`'s `proof_{NNN:06}.json`.
//!
//! Exit code: 0 on success (proof generated AND self-verified), non-zero
//! on any failure.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use bridge_event_prove_circuit::test_helpers::build_synthetic_event_keygen_inputs;
use bridge_event_prover_lib::{EventProofOutput, EventProver, PrivateWitness};
use bridge_prover_lib::keys::KeyManager;

const PARAMS_DIR: &str = "./params";

/// `--selftest` mode uses this fixed seed so two consecutive runs produce
/// byte-identical circuits (helpful when debugging keygen determinism).
const SELFTEST_SEED: u64 = 0xC0FFEE_5E_5E_5E_u64;

/// Output JSON schema. Bump if any field semantics change.
const OUTPUT_SCHEMA_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct CliArgs {
    fixture: Option<PathBuf>,
    out_dir: Option<PathBuf>,
    seq_no: Option<u32>,
    selftest: bool,
}

impl CliArgs {
    fn parse() -> Result<Self> {
        let mut args = std::env::args().skip(1);
        let mut fixture = None;
        let mut out_dir = None;
        let mut seq_no = None;
        let mut selftest = false;

        while let Some(a) = args.next() {
            match a.as_str() {
                "--fixture" => {
                    let v = args.next().context("--fixture needs a path")?;
                    fixture = Some(PathBuf::from(v));
                }
                "--out-dir" => {
                    let v = args.next().context("--out-dir needs a path")?;
                    out_dir = Some(PathBuf::from(v));
                }
                
                "--seq-no" => {
                    let v = args.next().context("--seq-no needs a u32")?;
                    seq_no = Some(v.parse::<u32>().context("--seq-no must be a u32")?);
                }
                "--selftest" => selftest = true,
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                }
                other => bail!("unknown argument: {other}"),
            }
        }

        if !selftest && fixture.is_none() {
            bail!("must supply either --fixture <path> or --selftest");
        }
        if selftest && fixture.is_some() {
            bail!("--selftest and --fixture are mutually exclusive");
        }

        Ok(Self {
            fixture,
            out_dir,
            seq_no,
            selftest,
        })
    }
}

fn print_help() {
    eprintln!("Usage: bridge-event-prove [--fixture <path> | --selftest] [--out-dir <path>] [--seq-no <u32>]");
    eprintln!();
    eprintln!("  --fixture <path>  PrivateWitness JSON to prove.");
    eprintln!("  --selftest        Synthesise inputs internally (no fixture needed).");
    eprintln!("  --out-dir <path>  If set, also write proof_event_NNN.json there.");
    eprintln!("  --seq-no <u32>    Seq number for the output file (default: 0).");
    eprintln!();
    eprintln!("Prints a JSON summary on the last non-empty line of stdout.");
}

#[derive(Serialize)]
struct OutputSummary<'a> {
    schema_version: u32,
    mode: &'a str,
    seq_no: u32,
    self_verified: bool,
    proof_hex: String,
    public_instances_hex: Vec<String>,
    /// Path to the on-disk proof JSON if `--out-dir` was given.
    proof_file: Option<String>,
    /// Wall-clock time spent generating the Circuit 4 proof, in milliseconds.
    /// Excludes PK load/unload and self-verification.
    event_proof_gen_ms: u64,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr) // keep stdout clean for the JSON summary
        .init();

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("bridge-event-prove failed: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let args = CliArgs::parse()?;
    let mode = if args.selftest { "selftest" } else { "fixture" };
    let seq_no = args.seq_no.unwrap_or(0);

    info!("=== bridge-event-prove ({mode}) ===");
    info!("params_dir: {PARAMS_DIR}");

    let mut km = KeyManager::new(Path::new(PARAMS_DIR));
    let mut event_prover = EventProver::new(&mut km);
    event_prover.ensure_keys().context("ensure_event_keys failed")?;
    event_prover.load_pk().context("load_event_pk failed")?;

    let t_proof = Instant::now();
    let out: EventProofOutput = if args.selftest {
        info!("synthesising inputs via build_synthetic_event_keygen_inputs(seed={SELFTEST_SEED:#x})");
        let (circuit, instances) = build_synthetic_event_keygen_inputs(SELFTEST_SEED);
        event_prover.prove_circuit(circuit, instances)?
    } else {
        let fixture_path = args.fixture.as_ref().expect("checked in parse");
        info!("reading PrivateWitness from {}", fixture_path.display());
        let raw = std::fs::read_to_string(fixture_path)
            .with_context(|| format!("failed to read {}", fixture_path.display()))?;
        let witness: PrivateWitness = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse PrivateWitness JSON from {}", fixture_path.display()))?;
        event_prover.prove(&witness)?
    };
    let event_proof_gen_ms = t_proof.elapsed().as_millis() as u64;
    info!("Circuit 4 proof generated in {} ms", event_proof_gen_ms);

    // Self-verify before unloading PK — catches gross misconfiguration
    // (wrong VK, mismatched config, etc.) before the orchestrator has to.
    let ok = event_prover.verify(&out.proof_bytes, &out.public_instances);
    event_prover.unload_pk();
    if !ok {
        bail!("self-verification of the freshly generated proof FAILED");
    }
    info!("self-verification OK");

    let proof_hex = hex::encode(&out.proof_bytes);
    let public_instances_hex: Vec<String> = out
        .public_instances
        .iter()
        .map(|fr| {
            use halo2_base::halo2_proofs::halo2curves::group::ff::PrimeField;
            hex::encode(fr.to_repr())
        })
        .collect();

    // Optional on-disk artefact (separate seqno space from primary/layer).
    let proof_file = if let Some(dir) = args.out_dir.as_ref() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
        let fname = dir.join(format!("proof_event_{:06}.json", seq_no));
        let on_disk = serde_json::json!({
            "schema_version": OUTPUT_SCHEMA_VERSION,
            "seq_no": seq_no,
            "proof_hex": proof_hex,
            "public_instances_hex": public_instances_hex,
            "self_verified": ok,
            "event_proof_gen_ms": event_proof_gen_ms,
        });
        std::fs::write(&fname, serde_json::to_vec_pretty(&on_disk)?)
            .with_context(|| format!("failed to write {}", fname.display()))?;
        info!("wrote {}", fname.display());
        Some(fname.to_string_lossy().into_owned())
    } else {
        None
    };

    // Stdout summary — must be the last non-empty line for dex-style consumers.
    let summary = OutputSummary {
        schema_version: OUTPUT_SCHEMA_VERSION,
        mode,
        seq_no,
        self_verified: ok,
        proof_hex,
        public_instances_hex,
        proof_file,
        event_proof_gen_ms,
    };
    println!("{}", serde_json::to_string(&summary)?);
    Ok(())
}
