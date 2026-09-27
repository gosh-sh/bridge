//! Offline keygen for the cross-thread `BridgeMultiHopProof`.
//!
//! Same purpose and shape as `keygen_bridge_final`: run the identical
//! [`bridge_prover_lib::keys::MultiHopKeyManager::ensure_keys`] path off
//! the hot path so the relayer starts warm. Multi-hop keygen is lighter
//! than the event circuit (K=17, ~3–4 GiB PK vs Circuit 4's K=19), but
//! the daemon still pays the cost synchronously on first start.
//!
//! Requires: `--params-dir/kzg_bn254_{K}.srs` present (provision with
//! `bootstrap_hermez_srs`). Manifest-consistent atomic writes are handled
//! by [`bridge_prover_lib::keys::state::KeyManagerState`] — safe to run
//! next to a live daemon; `flock`ed on `multi_hop_keygen.lock`.
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
use bridge_prover_lib::keys::MultiHopKeyManager;

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
    let k = k.unwrap_or(MultiHopKeyManager::DEFAULT_K);
    Ok(Args { params_dir, k })
}

fn print_help() {
    println!(
        "\
keygen_bridge_multi_hop — offline keygen for BridgeMultiHopProof

USAGE:
    keygen_bridge_multi_hop --params-dir PATH [--k K]

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
    - Writes multi_hop_vk.bin, multi_hop_pk.bin, multi_hop_config_params.json,
      multi_hop_manifest.json (manifest LAST, atomically).
    - Idempotent: warm cache returns Ok(()) without rebuilding.
    - flock'ed on multi_hop_keygen.lock; safe to run alongside a daemon.
",
        default_k = MultiHopKeyManager::DEFAULT_K,
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
        "[keygen_bridge_multi_hop]\n  params_dir = {}\n  k          = {}\n  srs        = {} ({})",
        args.params_dir.display(),
        args.k,
        srs_path.display(),
        size_of(&srs_path),
    );

    let start = Instant::now();
    let mut km = MultiHopKeyManager::new_with_k(&args.params_dir, args.k);
    km.ensure_keys()?;
    let elapsed = start.elapsed();

    let vk = args.params_dir.join("multi_hop_vk.bin");
    let pk = args.params_dir.join("multi_hop_pk.bin");
    let cfg = args.params_dir.join("multi_hop_config_params.json");
    let manifest = args.params_dir.join("multi_hop_manifest.json");

    println!(
        "[keygen_bridge_multi_hop] done in {:.1?}\n  {} ({})\n  {} ({})\n  {} ({})\n  {} ({})",
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
