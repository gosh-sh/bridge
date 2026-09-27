//! Offline keygen for the multi-thread `BridgeEventFinalProof` (Circuit 4).
//!
//! Purpose: make key rotation reproducible from an operator's laptop and CI
//! instead of relying on the daemon's first-start auto-keygen. First-start
//! keygen is slow (~7 min at K=19), spikes RSS (>10 GB), and stalls the
//! relayer's read-loop — this bin runs the identical
//! [`bridge_prover_lib::keys::EventKeyManager::ensure_keys`] path off the
//! hot path so a running daemon just picks up warm cache.
//!
//! Requires: `--params-dir/kzg_bn254_{K}.srs` present (provision with
//! `bootstrap_hermez_srs`). Manifest-consistent atomic writes are handled
//! by [`bridge_prover_lib::keys::state::KeyManagerState`] — safe to run
//! next to a live daemon; `flock`ed on `event_keygen.lock`.
//!
//! Idempotent: a warm cache short-circuits inside `ensure_keys` with an
//! info log and exits 0.
//!
//! See `MULTITHREAD_MIGRATION_PLAN.md` §8 for Commit-7 context.

use std::{
    fs,
    path::PathBuf,
    time::Instant,
};

use anyhow::{bail, Context, Result};
use bridge_prover_lib::keys::EventKeyManager;

struct Args {
    params_dir: PathBuf,
    k: u32,
}

fn parse_args() -> Result<Args> {
    let mut params_dir: Option<PathBuf> = None;
    let mut k: Option<u32> = None;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--params-dir" => {
                params_dir = Some(PathBuf::from(
                    it.next().context("--params-dir requires a path")?,
                ));
            },
            "--k" => {
                let v: u32 = it
                    .next()
                    .context("--k requires a value")?
                    .parse()
                    .context("--k value must be a u32")?;
                k = Some(v);
            },
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            },
            other => bail!("unknown flag: {other}"),
        }
    }

    let params_dir = params_dir.context("--params-dir is required")?;
    let k = k.unwrap_or(EventKeyManager::DEFAULT_K);
    Ok(Args { params_dir, k })
}

fn print_help() {
    println!(
        "\
keygen_bridge_final — offline keygen for BridgeEventFinalProof (Circuit 4)

USAGE:
    keygen_bridge_final --params-dir PATH [--k K]

OPTIONS:
    --params-dir PATH   Directory with kzg_bn254_{{K}}.srs; VK/PK/manifest
                        are written here alongside it. Required.
    --k K               Circuit degree (default: {default_k}).
                        Must match the circuit's arithmetic K — halo2-axiom
                        bakes params.k() into vk.domain, so an oversized SRS
                        produces an oversized PK.
    -h, --help          Show this help.

NOTES:
    - Requires kzg_bn254_{{K}}.srs in --params-dir (provision with
      bootstrap_hermez_srs).
    - Writes event_vk.bin, event_pk.bin, event_config_params.json,
      event_manifest.json (manifest LAST, atomically).
    - Idempotent: warm cache returns Ok(()) without rebuilding.
    - flock'ed on event_keygen.lock; safe to run alongside a daemon.
",
        default_k = EventKeyManager::DEFAULT_K,
    );
}

fn size_of(path: &std::path::Path) -> String {
    match fs::metadata(path) {
        Ok(m) => format!("{} bytes", m.len()),
        Err(e) => format!("<stat failed: {e}>"),
    }
}

fn main() -> Result<()> {
    let args = parse_args()?;

    let srs_path = args.params_dir.join(format!("kzg_bn254_{}.srs", args.k));
    if !srs_path.is_file() {
        bail!(
            "SRS not found at {}. Provision it first with:\n  \
             cargo run --release -p bridge-prover-lib --bin bootstrap_hermez_srs -- \
             --params-dir {} --k {}",
            srs_path.display(),
            args.params_dir.display(),
            args.k,
        );
    }

    println!(
        "[keygen_bridge_final]\n  params_dir = {}\n  k          = {}\n  srs        = {} ({})",
        args.params_dir.display(),
        args.k,
        srs_path.display(),
        size_of(&srs_path),
    );

    let start = Instant::now();
    let mut km = EventKeyManager::new_with_k(&args.params_dir, args.k);
    km.ensure_keys()?;
    let elapsed = start.elapsed();

    // Report every file the key manager writes (see keys/state.rs).
    let vk = args.params_dir.join("event_vk.bin");
    let pk = args.params_dir.join("event_pk.bin");
    let cfg = args.params_dir.join("event_config_params.json");
    let manifest = args.params_dir.join("event_manifest.json");

    println!(
        "[keygen_bridge_final] done in {:.1?}\n  {} ({})\n  {} ({})\n  {} ({})\n  {} ({})",
        elapsed,
        vk.display(),
        size_of(&vk),
        pk.display(),
        size_of(&pk),
        cfg.display(),
        size_of(&cfg),
        manifest.display(),
        size_of(&manifest),
    );
    Ok(())
}
