//! Materialize Hermez Perpetual Powers of Tau KZG SRS files under the
//! bridge prover's `params/` directory at the K values the bridge circuits
//! require (K=17 layer, K=19 event, K=20 primary+keygen, K=21 fallback).
//!
//! Two SRS provisioning paths:
//!
//! * **K in 1..=20** — uses `gosh-zk-snark-halo2-utils::ptau`, which pins
//!   the K=20 raw-SRS SHA-256 trust anchor. Reads
//!   `powersOfTau28_hez_final_20.ptau` (SnarkJs, ~1.2 GB) once, verifies
//!   the anchor, then downsizes to each requested K. Ptau is downloaded
//!   on cache miss to `$HOME/.cache/halo2-kzg-srs/`.
//!
//! * **K = 21** — no shared trust anchor available (utils crate is capped
//!   at K=20). Reads `powersOfTau28_hez_final_21.ptau` (SnarkJs, ~2.4 GB)
//!   directly via `halo2_kzg_srs::Srs::read_partial(reader, SnarkJs, 21)`
//!   and materializes via `write_raw`. The Hermez `s_g2 head` byte-level
//!   check still applies — `bridge_prover_lib::keys::common` will reject
//!   any file whose `s_g2` head is not `928fafb3d0cc`, so K=21 SRS files
//!   this binary writes are load-testable via `KeyManager::new` startup.
//!
//! * **K = 22** — same no-anchor path as K=21, reads
//!   `powersOfTau28_hez_final_22.ptau` (SnarkJs, ~4.8 GB) via
//!   `halo2_kzg_srs::Srs::read_partial(reader, SnarkJs, 22)`. Required
//!   only for the outer `LayerHashesAggregatorVerifier` (aggregator preset
//!   `K_outer=22`, see `bridge-evm-aggregator::aggregator::for_verifier_name`).
//!   Not part of `DEFAULT_KS` — request explicitly via `--k 22`.
//!
//! Default outputs:
//!   params/kzg_bn254_17.srs   (~16 MB)   — Circuit 2 (layer) proving
//!   params/kzg_bn254_19.srs   (~64 MB)   — Circuit 4 (event) proving
//!   params/kzg_bn254_20.srs   (~128 MB)  — Circuit 1A (primary) + all keygen
//!   params/kzg_bn254_21.srs   (~256 MB)  — Circuit 3 (fallback) proving
//!   params/kzg_bn254_22.srs   (~512 MB)  — LayerHashes outer aggregator (opt-in)
//!
//! K=21 uses its own ptau at
//! `$HOME/.cache/halo2-kzg-srs/powersOfTau28_hez_final_21.ptau` (override
//! with `--ptau21`; not auto-downloaded — fetch manually from the Polygon
//! zkEVM GCS mirror
//! `https://storage.googleapis.com/aptos-circuit-testing-setups/ptau/powersOfTau28_hez_final_21.ptau`).
//!
//! K=22 uses `$HOME/.cache/halo2-kzg-srs/powersOfTau28_hez_final_22.ptau`
//! (override with `--ptau22`; not auto-downloaded — fetch manually from
//! `powersOfTau28_hez_final_22.ptau`). NOTE: the zkevm bucket that used to
//! serve these revoked anonymous access; only K=21 is known to be mirrored,
//! at `https://storage.googleapis.com/aptos-circuit-testing-setups/ptau/`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use bridge_prover_lib::keys::HERMEZ_S_G2_HEAD;
use gosh_zk_snark_halo2_utils::ptau::{
    default_ptau_cache_path, ensure_hermez_k20_ptau, read_hermez_ptau_and_verify,
    HERMEZ_K20_RAW_SRS_SHA256,
};
use halo2_curves::bn256::Bn256;
use halo2_kzg_srs::{Srs, SrsFormat};

const DEFAULT_KS: &[u32] = &[17, 19, 20, 21];
const WIPE_STEMS: &[&str] = &["primary", "layer", "event", "fallback"];
const WIPE_SUFFIXES: &[&str] = &["_vk.bin", "_pk.bin", "_config_params.json"];

/// Default `$HOME/.cache/halo2-kzg-srs/powersOfTau28_hez_final_{k}.ptau`
/// cache path for the direct-ptau K values (K=21, K=22 — the ones the
/// utils crate can't provision because its K=20 anchor doesn't reach them).
fn default_hermez_ptau_cache_path(k: u32) -> PathBuf {
    let home = std::env::var("HOME").expect("HOME must be set");
    PathBuf::from(home)
        .join(".cache/halo2-kzg-srs")
        .join(format!("powersOfTau28_hez_final_{k}.ptau"))
}

struct Args {
    params_dir: PathBuf,
    ks: Vec<u32>,
    wipe: bool,
    ptau_path: PathBuf,
    ptau21_path: PathBuf,
    ptau22_path: PathBuf,
}

fn parse_args() -> Result<Args> {
    let mut params_dir: Option<PathBuf> = None;
    let mut ks: Vec<u32> = Vec::new();
    let mut wipe = false;
    let mut ptau_path: Option<PathBuf> = None;
    let mut ptau21_path: Option<PathBuf> = None;
    let mut ptau22_path: Option<PathBuf> = None;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--params-dir" => {
                params_dir = Some(PathBuf::from(
                    it.next().context("--params-dir requires a path")?,
                ));
            }
            "--k" => {
                let v: u32 = it
                    .next()
                    .context("--k requires a value")?
                    .parse()
                    .context("--k value must be a u32")?;
                if !(1..=22).contains(&v) {
                    bail!("--k must be in 1..=22, got {v}");
                }
                ks.push(v);
            }
            "--wipe-cached-keys" => wipe = true,
            "--ptau" => {
                ptau_path = Some(PathBuf::from(
                    it.next().context("--ptau requires a path")?,
                ));
            }
            "--ptau21" => {
                ptau21_path = Some(PathBuf::from(
                    it.next().context("--ptau21 requires a path")?,
                ));
            }
            "--ptau22" => {
                ptau22_path = Some(PathBuf::from(
                    it.next().context("--ptau22 requires a path")?,
                ));
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => bail!("unknown flag: {other}"),
        }
    }

    // Default params-dir: crate's own params/ (works when run via `cargo run`).
    let params_dir = params_dir.unwrap_or_else(|| {
        // CARGO_MANIFEST_DIR points at bridge-prover-lib; params lives one up.
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../params")
    });
    let ptau_path = ptau_path.unwrap_or_else(default_ptau_cache_path);
    let ptau21_path = ptau21_path.unwrap_or_else(|| default_hermez_ptau_cache_path(21));
    let ptau22_path = ptau22_path.unwrap_or_else(|| default_hermez_ptau_cache_path(22));
    let ks = if ks.is_empty() { DEFAULT_KS.to_vec() } else { ks };

    Ok(Args {
        params_dir,
        ks,
        wipe,
        ptau_path,
        ptau21_path,
        ptau22_path,
    })
}

fn print_help() {
    println!(
        "\
bootstrap_hermez_srs — provision Hermez PPoT KZG SRS files for the bridge prover

USAGE:
    bootstrap_hermez_srs [--params-dir PATH] [--k N]... [--wipe-cached-keys]
                        [--ptau PATH] [--ptau21 PATH] [--ptau22 PATH]

OPTIONS:
    --params-dir PATH        Where to write kzg_bn254_N.srs files
                             (default: <bridge-prover-lib>/../params)
    --k N                    Circuit K to materialize (repeatable, 1..=22)
                             (default: --k 17 --k 19 --k 20 --k 21; --k 22 is opt-in)
    --wipe-cached-keys       Delete primary/layer/event/fallback _vk.bin/_pk.bin/_config_params.json
                             (their commitments embed s_g2 → mandatory after SRS swap)
    --ptau PATH              Path to powersOfTau28_hez_final_20.ptau (used for K in 1..=20)
                             (default: $HOME/.cache/halo2-kzg-srs/powersOfTau28_hez_final_20.ptau;
                              downloaded on cache miss)
    --ptau21 PATH            Path to powersOfTau28_hez_final_21.ptau (used for K=21 only)
                             (default: $HOME/.cache/halo2-kzg-srs/powersOfTau28_hez_final_21.ptau;
                              NOT auto-downloaded — fetch manually)
    --ptau22 PATH            Path to powersOfTau28_hez_final_22.ptau (used for K=22 only)
                             (default: $HOME/.cache/halo2-kzg-srs/powersOfTau28_hez_final_22.ptau;
                              NOT auto-downloaded — fetch manually)
    -h, --help               Show this help
"
    );
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn assert_hermez_head(path: &Path, raw: &[u8]) -> Result<()> {
    if raw.len() < 128 {
        bail!(
            "SRS at {} is too short ({} bytes) to hold s_g2",
            path.display(),
            raw.len()
        );
    }
    let head = &raw[raw.len() - 128..raw.len() - 122];
    if head != HERMEZ_S_G2_HEAD {
        bail!(
            "SRS at {} does not have Hermez s_g2 head (got {}, expected {})",
            path.display(),
            hex_lower(head),
            hex_lower(&HERMEZ_S_G2_HEAD),
        );
    }
    Ok(())
}

fn wipe_cached_keys(params_dir: &Path) -> Result<()> {
    for stem in WIPE_STEMS {
        for suf in WIPE_SUFFIXES {
            let p = params_dir.join(format!("{stem}{suf}"));
            if p.exists() {
                fs::remove_file(&p)
                    .with_context(|| format!("removing {}", p.display()))?;
                println!("[WIPE] {}", p.display());
            }
        }
    }
    Ok(())
}

/// Materialize a raw SRS directly from a Hermez ptau file via
/// `halo2_kzg_srs::Srs::read_partial` + `write_raw`. Used for K values
/// where no shared trust anchor exists (utils crate caps at K=20 — so
/// K=21 fallback and K=22 outer aggregator take this no-anchor path).
/// Correctness relies on the Hermez `s_g2` head byte-level check
/// enforced by `assert_hermez_head` downstream.
fn materialize_raw_srs_from_ptau(ptau_path: &Path, k: u32) -> Result<Vec<u8>> {
    if !ptau_path.exists() {
        bail!(
            "K={k} ptau not found at {}. Download it with:\n  \
             curl -L --fail --progress-bar \\\n    \
             https://storage.googleapis.com/aptos-circuit-testing-setups/ptau/powersOfTau28_hez_final_21.ptau \\\n    \
             -o {}",
            ptau_path.display(),
            ptau_path.display(),
        );
    }
    let mut file = fs::File::open(ptau_path)
        .with_context(|| format!("opening K={k} ptau {}", ptau_path.display()))?;
    let srs = Srs::<Bn256>::read_partial(&mut file, SrsFormat::SnarkJs, k);
    let n = 1usize << k;
    let mut buf: Vec<u8> = Vec::with_capacity(4 + 2 * n * 64 + 256);
    srs.write_raw(&mut buf);
    Ok(buf)
}

fn main() -> Result<()> {
    let args = parse_args()?;

    fs::create_dir_all(&args.params_dir)
        .with_context(|| format!("creating {}", args.params_dir.display()))?;

    println!(
        "[bootstrap_hermez_srs]\n  params_dir = {}\n  ptau       = {}\n  ptau21     = {}\n  ptau22     = {}\n  ks         = {:?}\n  wipe       = {}",
        args.params_dir.display(),
        args.ptau_path.display(),
        args.ptau21_path.display(),
        args.ptau22_path.display(),
        args.ks,
        args.wipe,
    );

    // Split requested K into <=20 (utils path) and ==21 (direct path).
    let needs_k20_ptau = args.ks.iter().any(|&k| k <= 20);

    // Step 1a — ensure K=20 ptau on disk (download on cache miss) if needed.
    if needs_k20_ptau {
        ensure_hermez_k20_ptau(&args.ptau_path)
            .map_err(|e| anyhow::anyhow!("ensure_hermez_k20_ptau: {e}"))?;
        println!(
            "[trust anchor] K=20 raw-SRS SHA-256 expected = {}",
            hex_lower(&HERMEZ_K20_RAW_SRS_SHA256)
        );
    }

    // Step 2 — materialize each requested K.
    for &k in &args.ks {
        let out = args.params_dir.join(format!("kzg_bn254_{k}.srs"));
        let raw_srs: Vec<u8> = if k <= 20 {
            let mut file = fs::File::open(&args.ptau_path)
                .with_context(|| format!("opening ptau {}", args.ptau_path.display()))?;
            println!("[K={k}] reading + verifying + downsizing K=20 ptau...");
            let mat = read_hermez_ptau_and_verify(&mut file, k);
            mat.raw_srs
        } else {
            // K=21 (fallback proving) and K=22 (outer aggregator) — both take
            // the direct-ptau no-anchor path. parse_args caps K at 22.
            let ptau_path: &Path = match k {
                21 => &args.ptau21_path,
                22 => &args.ptau22_path,
                _ => unreachable!("parse_args enforces k in 1..=22"),
            };
            println!(
                "[K={k}] reading K={k} ptau via halo2_kzg_srs (no anchor — relying on s_g2 head)..."
            );
            materialize_raw_srs_from_ptau(ptau_path, k)?
        };
        // Byte-level sanity: s_g2 head must be Hermez.
        assert_hermez_head(&out, &raw_srs)?;
        fs::write(&out, &raw_srs)
            .with_context(|| format!("writing {}", out.display()))?;
        println!(
            "[K={k}] wrote {} ({} bytes)",
            out.display(),
            raw_srs.len()
        );
    }

    // Step 3 — optional stale-key wipe (VKs embed s_g2, so they must be
    // regenerated after SRS swap; PK bytes reference the same trapdoor).
    if args.wipe {
        wipe_cached_keys(&args.params_dir)?;
    }

    println!("[bootstrap_hermez_srs] done.");
    Ok(())
}
