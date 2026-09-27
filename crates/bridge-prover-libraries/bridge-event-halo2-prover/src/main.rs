//! `bridge-event-prove` — one-shot Circuit 4 proof generator.
//!
//! `generate_withdrawals_with_live_event_proving.py` (Track D4)
//! will invoke this binary once per `WithdrawalInitiated` event.
//!
//! ### Modes
//!
//! Final `BridgeEventFinalProof` (always produced):
//! * `--fixture <path>` — read a `PrivateWitness` JSON (produced by Track D1's
//!   `bridge-event-witness-builder` binary, now shipped from the
//!   `bridge-event-witness` crate) and prove it.
//! * `--selftest` — synthesise inputs via
//!   `bridge-event-prove-circuit::test_helpers::build_synthetic_final_proof_keygen_inputs`
//!   and prove them. Useful for smoke-testing the install (keygen → prove →
//!   verify roundtrip) without needing a live node.
//!
//! Optional `BridgeMultiHopProof` bundle (cross-thread events):
//! * `--hops-fixture <path>` — read a `MultiHopBundleWitnessJson` (list of hop
//!   snark witnesses) and prove each. Same-thread callers omit this and the
//!   binary behaves identically to pre-Commit-8.
//! * `--mh-selftest` — synthesise one hop snark per
//!   `bridge-event-prove-circuit::test_helpers::build_synthetic_multi_hop_keygen_inputs`.
//!   Produces `--mh-selftest-count` snarks (default 1). Note: the synthetic
//!   final and hop instances are unrelated, so end-to-end bundle structural
//!   verification is skipped in selftest — only per-snark KZG is checked.
//!
//! ### Output
//!
//! Always prints a single-line JSON summary as the **last non-empty line**
//! of stdout. If `--out-dir` is supplied, also writes
//! `proof_event_{NNN:06}.json` to that directory using a separate seqno
//! space from `bridge-prover-daemon`'s `proof_{NNN:06}.json`.
//!
//! Same-thread output shape is unchanged. When hops are proved the JSON gains
//! a `hops` array of `{ proof_hex, public_instances_hex }` records.
//!
//! Exit code: 0 on success (proof generated AND self-verified), non-zero
//! on any failure.

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    time::Instant,
};

use anyhow::{bail, Context, Result};
use bridge_event_prove_circuit::test_helpers::{
    build_synthetic_final_proof_keygen_inputs, build_synthetic_multi_hop_keygen_inputs,
};
use bridge_event_prover_lib::{
    generate_multi_hop_proof, generate_multi_hop_proof_from_circuit, verify_multi_hop_proof,
    EventProofOutput, EventProver, MultiHopBundleWitnessJson, MultiHopProofOutput, PrivateWitness,
};
use bridge_prover_lib::keys::{EventKeyManager, MultiHopKeyManager};
use serde::{Deserialize, Serialize};
use tracing::{error, info};

const PARAMS_DIR: &str = "./params";

/// `--selftest` mode uses this fixed seed so two consecutive runs produce
/// byte-identical circuits (helpful when debugging keygen determinism).
const SELFTEST_SEED: u64 = 0xC0FFEE_5E_5E_5E_u64;

/// Base seed for `--mh-selftest`; the N-th synthetic hop uses
/// `MH_SELFTEST_SEED ^ (N as u64)`.
const MH_SELFTEST_SEED: u64 = 0x1B0D_1B0D_1B0D_1B0Du64;

#[derive(Serialize, Deserialize)]
struct CliArgs {
    fixture: Option<PathBuf>,
    out_dir: Option<PathBuf>,
    seq_no: Option<u32>,
    selftest: bool,
    hops_fixture: Option<PathBuf>,
    mh_selftest: bool,
    mh_selftest_count: usize,
}

impl CliArgs {
    fn parse() -> Result<Self> {
        let mut args = std::env::args().skip(1);
        let mut fixture = None;
        let mut out_dir = None;
        let mut seq_no = None;
        let mut selftest = false;
        let mut hops_fixture = None;
        let mut mh_selftest = false;
        let mut mh_selftest_count: usize = 1;

        while let Some(a) = args.next() {
            match a.as_str() {
                "--fixture" => {
                    let v = args.next().context("--fixture needs a path")?;
                    fixture = Some(PathBuf::from(v));
                },
                "--out-dir" => {
                    let v = args.next().context("--out-dir needs a path")?;
                    out_dir = Some(PathBuf::from(v));
                },

                "--seq-no" => {
                    let v = args.next().context("--seq-no needs a u32")?;
                    seq_no = Some(v.parse::<u32>().context("--seq-no must be a u32")?);
                },
                "--selftest" => selftest = true,
                "--hops-fixture" => {
                    let v = args.next().context("--hops-fixture needs a path")?;
                    hops_fixture = Some(PathBuf::from(v));
                },
                "--mh-selftest" => mh_selftest = true,
                "--mh-selftest-count" => {
                    let v = args.next().context("--mh-selftest-count needs a usize")?;
                    mh_selftest_count = v
                        .parse::<usize>()
                        .context("--mh-selftest-count must be a usize")?;
                },
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                },
                other => bail!("unknown argument: {other}"),
            }
        }

        if !selftest && fixture.is_none() {
            bail!("must supply either --fixture <path> or --selftest");
        }
        if selftest && fixture.is_some() {
            bail!("--selftest and --fixture are mutually exclusive");
        }
        if mh_selftest && hops_fixture.is_some() {
            bail!("--mh-selftest and --hops-fixture are mutually exclusive");
        }
        if mh_selftest && mh_selftest_count == 0 {
            bail!("--mh-selftest-count must be >= 1");
        }

        Ok(Self {
            fixture,
            out_dir,
            seq_no,
            selftest,
            hops_fixture,
            mh_selftest,
            mh_selftest_count,
        })
    }

    fn wants_hops(&self) -> bool {
        self.hops_fixture.is_some() || self.mh_selftest
    }
}

fn print_help() {
    eprintln!(
        "Usage: bridge-event-prove [--fixture <path> | --selftest] [--out-dir <path>] [--seq-no \
         <u32>] [--hops-fixture <path> | --mh-selftest [--mh-selftest-count <n>]]"
    );
    eprintln!();
    eprintln!("  --fixture <path>          PrivateWitness JSON to prove.");
    eprintln!("  --selftest                Synthesise final-proof inputs internally.");
    eprintln!("  --out-dir <path>          If set, also write proof_event_NNN.json there.");
    eprintln!("  --seq-no <u32>            Seq number for the output file (default: 0).");
    eprintln!("  --hops-fixture <path>     MultiHopBundleWitnessJson to prove per snark.");
    eprintln!("  --mh-selftest             Synthesise multi-hop inputs internally.");
    eprintln!("  --mh-selftest-count <n>   How many synthetic hop snarks (default 1).");
    eprintln!();
    eprintln!("Prints a JSON summary on the last non-empty line of stdout.");
}

#[derive(Serialize)]
struct HopArtifact {
    proof_hex: String,
    public_instances_hex: Vec<String>,
}

#[derive(Serialize)]
struct OutputSummary<'a> {
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
    /// Multi-hop bundle snarks, in order. Empty for same-thread callers, in
    /// which case the field is omitted from the serialized output.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    hops: Vec<HopArtifact>,
    /// Wall-clock time spent generating all hop snarks, in milliseconds.
    /// Omitted when `hops` is empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    multi_hop_proof_gen_ms: Option<u64>,
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
        },
    }
}

fn run() -> Result<()> {
    let args = CliArgs::parse()?;
    let mode = if args.selftest { "selftest" } else { "fixture" };
    let seq_no = args.seq_no.unwrap_or(0);

    info!("=== bridge-event-prove ({mode}) ===");
    info!("params_dir: {PARAMS_DIR}");

    // Load the multi-hop bundle upfront when a fixture was supplied — the
    // Circuit 4 proof needs its `y_block_id` endpoint to build the public
    // instances. `--mh-selftest` and same-thread modes use the empty
    // default bundle (which keeps `y_block_id == x_block_id`).
    let hop_bundle: MultiHopBundleWitnessJson =
        if let Some(hops_path) = args.hops_fixture.as_ref() {
            info!(
                "reading MultiHopBundleWitnessJson from {}",
                hops_path.display()
            );
            let raw = std::fs::read_to_string(hops_path)
                .with_context(|| format!("failed to read {}", hops_path.display()))?;
            serde_json::from_str(&raw).with_context(|| {
                format!(
                    "failed to parse MultiHopBundleWitnessJson from {}",
                    hops_path.display()
                )
            })?
        } else {
            MultiHopBundleWitnessJson::default()
        };

    // ---- Circuit 4 (final) proof ----
    let mut ekm = EventKeyManager::new(Path::new(PARAMS_DIR));
    let (event_out, event_proof_gen_ms) = {
        let mut event_prover = EventProver::new(&mut ekm);
        event_prover
            .ensure_keys()
            .context("ensure_event_keys failed")?;
        event_prover.load_pk().context("load_event_pk failed")?;

        let t_proof = Instant::now();
        let out: EventProofOutput = if args.selftest {
            info!(
                "synthesising final inputs via \
                 build_synthetic_final_proof_keygen_inputs(seed={SELFTEST_SEED:#x})"
            );
            let (circuit, instances) = build_synthetic_final_proof_keygen_inputs(SELFTEST_SEED);
            event_prover.prove_circuit(circuit, instances)?
        } else {
            let fixture_path = args.fixture.as_ref().expect("checked in parse");
            info!("reading PrivateWitness from {}", fixture_path.display());
            let raw = std::fs::read_to_string(fixture_path)
                .with_context(|| format!("failed to read {}", fixture_path.display()))?;
            let witness: PrivateWitness = serde_json::from_str(&raw).with_context(|| {
                format!(
                    "failed to parse PrivateWitness JSON from {}",
                    fixture_path.display()
                )
            })?;
            event_prover.prove(&witness, &hop_bundle)?
        };
        let elapsed = t_proof.elapsed().as_millis() as u64;
        info!("Circuit 4 proof generated in {} ms", elapsed);

        // Self-verify before unloading PK — catches gross misconfiguration.
        let ok = event_prover.verify(&out.proof_bytes, &out.public_instances);
        event_prover.unload_pk();
        if !ok {
            bail!("self-verification of the freshly generated final proof FAILED");
        }
        info!("final proof self-verification OK");
        (out, elapsed)
    };

    // ---- Optional BridgeMultiHopProof bundle ----
    // Lazily construct the multi-hop KeyManager so same-thread callers pay
    // nothing for the K=17 keygen.
    let (hops, multi_hop_proof_gen_ms) = if args.wants_hops() {
        let mut mhkm = MultiHopKeyManager::new(Path::new(PARAMS_DIR));
        mhkm.ensure_keys().context("ensure_multi_hop_keys failed")?;
        mhkm.load_pk().context("load_multi_hop_pk failed")?;

        let t_hops = Instant::now();
        let outs: Vec<MultiHopProofOutput> = if args.mh_selftest {
            info!(
                "synthesising {} multi-hop snarks via \
                 build_synthetic_multi_hop_keygen_inputs(seed={MH_SELFTEST_SEED:#x} ^ i)",
                args.mh_selftest_count
            );
            (0..args.mh_selftest_count)
                .map(|i| {
                    let (circuit, instances) =
                        build_synthetic_multi_hop_keygen_inputs(MH_SELFTEST_SEED ^ i as u64);
                    generate_multi_hop_proof_from_circuit(&mhkm, circuit, instances)
                        .with_context(|| format!("synthetic multi-hop proof #{i} failed"))
                })
                .collect::<Result<_>>()?
        } else {
            // Bundle was loaded upfront so the Circuit 4 proof could bind
            // its `y_block_id` to the last hop's `hop_end_block_id`; here we
            // just prove each snark in order.
            hop_bundle
                .snarks
                .iter()
                .enumerate()
                .map(|(i, snark)| {
                    generate_multi_hop_proof(&mhkm, snark)
                        .with_context(|| format!("multi-hop proof for snark #{i} failed"))
                })
                .collect::<Result<_>>()?
        };
        let elapsed = t_hops.elapsed().as_millis() as u64;
        info!(
            "generated {} multi-hop snark(s) in {} ms",
            outs.len(),
            elapsed
        );

        // Self-verify each snark's KZG. Structural verification is skipped
        // deliberately — the synthetic final and hop instances are unrelated,
        // and real-fixture callers get structural checks downstream via
        // `verify_bundle` in the daemon/relayer.
        for (i, out) in outs.iter().enumerate() {
            if !verify_multi_hop_proof(&mhkm, &out.proof_bytes, &out.public_instances) {
                mhkm.unload_pk();
                bail!("self-verification of multi-hop snark #{i} FAILED");
            }
        }
        mhkm.unload_pk();
        info!("per-snark multi-hop self-verification OK");

        let hop_artifacts: Vec<HopArtifact> = outs
            .into_iter()
            .map(|mh| HopArtifact {
                proof_hex: hex::encode(&mh.proof_bytes),
                public_instances_hex: mh
                    .public_instances
                    .iter()
                    .map(|fr| {
                        use halo2_base::halo2_proofs::halo2curves::group::ff::PrimeField;
                        hex::encode(fr.to_repr())
                    })
                    .collect(),
            })
            .collect();
        (hop_artifacts, Some(elapsed))
    } else {
        (Vec::new(), None)
    };

    let proof_hex = hex::encode(&event_out.proof_bytes);
    let public_instances_hex: Vec<String> = event_out
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
        let mut on_disk = serde_json::json!({
            "seq_no": seq_no,
            "proof_hex": proof_hex,
            "public_instances_hex": public_instances_hex,
            "self_verified": true,
            "event_proof_gen_ms": event_proof_gen_ms,
        });
        if !hops.is_empty() {
            on_disk["hops"] = serde_json::to_value(&hops)?;
            on_disk["multi_hop_proof_gen_ms"] =
                serde_json::to_value(multi_hop_proof_gen_ms.unwrap())?;
        }
        std::fs::write(&fname, serde_json::to_vec_pretty(&on_disk)?)
            .with_context(|| format!("failed to write {}", fname.display()))?;
        info!("wrote {}", fname.display());
        Some(fname.to_string_lossy().into_owned())
    } else {
        None
    };

    // Stdout summary — must be the last non-empty line for dex-style consumers.
    let summary = OutputSummary {
        mode,
        seq_no,
        self_verified: true,
        proof_hex,
        public_instances_hex,
        proof_file,
        event_proof_gen_ms,
        hops,
        multi_hop_proof_gen_ms,
    };
    println!("{}", serde_json::to_string(&summary)?);
    Ok(())
}
