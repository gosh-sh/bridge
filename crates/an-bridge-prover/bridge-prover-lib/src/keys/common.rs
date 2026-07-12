//! Shared VK/PK/config disk I/O helpers used by every per-circuit manager
//! under `crate::keys`. All artefacts live under a shared `params_dir`
//! (default `<crate>/params`) and share a filename convention:
//!
//! - `{prefix}_vk.bin`
//! - `{prefix}_pk.bin`
//! - `{prefix}_config_params.json`
//!
//! Serde format is `SerdeFormat::RawBytesUnchecked` — matches the pre-split
//! monolithic `KeyManager`, so cached artefacts on disk continue to load
//! after the refactor with no migration.

use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use halo2_base::gates::circuit::builder::BaseCircuitBuilder;
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::{ProvingKey, VerifyingKey},
    poly::{commitment::Params, kzg::commitment::ParamsKZG},
    SerdeFormat,
};
use tracing::{info, warn};

pub(crate) const SERDE_FMT: SerdeFormat = SerdeFormat::RawBytesUnchecked;

pub(crate) fn vk_path(params_dir: &Path, prefix: &str) -> PathBuf {
    params_dir.join(format!("{}_vk.bin", prefix))
}

pub(crate) fn pk_path(params_dir: &Path, prefix: &str) -> PathBuf {
    params_dir.join(format!("{}_pk.bin", prefix))
}

pub(crate) fn config_path(params_dir: &Path, prefix: &str) -> PathBuf {
    params_dir.join(format!("{}_config_params.json", prefix))
}

pub(crate) fn load_config(
    params_dir: &Path,
    prefix: &str,
) -> anyhow::Result<BaseCircuitParams> {
    let data = std::fs::read_to_string(config_path(params_dir, prefix))?;
    Ok(serde_json::from_str(&data)?)
}

pub(crate) fn save_config(
    params_dir: &Path,
    prefix: &str,
    config: &BaseCircuitParams,
) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(config)?;
    std::fs::write(config_path(params_dir, prefix), json)?;
    Ok(())
}

pub(crate) fn try_load_vk(
    params_dir: &Path,
    prefix: &str,
    config: &BaseCircuitParams,
) -> Option<VerifyingKey<G1Affine>> {
    let file = std::fs::File::open(vk_path(params_dir, prefix)).ok()?;
    let mut reader = BufReader::new(file);
    VerifyingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(
        &mut reader,
        SERDE_FMT,
        config.clone(),
    )
    .ok()
}

pub(crate) fn try_load_pk(
    params_dir: &Path,
    prefix: &str,
    config: &BaseCircuitParams,
) -> Option<ProvingKey<G1Affine>> {
    let file = std::fs::File::open(pk_path(params_dir, prefix)).ok()?;
    let mut reader = BufReader::new(file);
    ProvingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(
        &mut reader,
        SERDE_FMT,
        config.clone(),
    )
    .ok()
}

pub(crate) fn save_vk(
    params_dir: &Path,
    prefix: &str,
    vk: &VerifyingKey<G1Affine>,
) -> anyhow::Result<()> {
    let file = std::fs::File::create(vk_path(params_dir, prefix))?;
    let mut writer = BufWriter::new(file);
    vk.write(&mut writer, SERDE_FMT)?;
    Ok(())
}

pub(crate) fn save_pk(
    params_dir: &Path,
    prefix: &str,
    pk: &ProvingKey<G1Affine>,
) -> anyhow::Result<()> {
    let file = std::fs::File::create(pk_path(params_dir, prefix))?;
    let mut writer = BufWriter::new(file);
    pk.write(&mut writer, SERDE_FMT)?;
    Ok(())
}

/// Load a KZG SRS whose `params.k()` is exactly `k`.
///
/// halo2-axiom's `create_proof` asserts `params.n() == 1 << circuit.k()`, so
/// a shared larger-degree ceremony (partner dirs often ship only
/// `kzg_bn254_20.srs`) cannot be passed straight through — it must be
/// downsized. `ParamsKZG::downsize` keeps the same toxic waste / `s_g2`, so
/// proofs stay compatible with PKs keygen'd against the parent ceremony.
///
/// Resolution order:
/// 1. Exact `kzg_bn254_{k}.srs` whose header `k` matches (or is larger →
///    downsize in place and rewrite).
/// 2. Largest on-disk `kzg_bn254_*.srs` with header degree ≥ `k`, downsized
///    and written to the exact path for the next load.
/// 3. Last resort: `gen_srs(k)` (may synthesise a ChaCha20 trapdoor SRS —
///    **not** the chain ceremony; production must pre-populate files).
pub(crate) fn load_srs(params_dir: &Path, k: u32) -> ParamsKZG<Bn256> {
    let exact_path = params_dir.join(format!("kzg_bn254_{k}.srs"));

    if exact_path.exists() {
        match read_srs_file(&exact_path) {
            Ok(srs) if srs.k() == k => return srs,
            Ok(mut srs) if srs.k() > k => {
                warn!(
                    target: "bridge_prover_lib::keys",
                    path = %exact_path.display(),
                    file_k = srs.k(),
                    circuit_k = k,
                    "SRS file degree exceeds requested k; downsizing (likely a misnamed larger ceremony)"
                );
                srs.downsize(k);
                let _ = write_srs_file(&exact_path, &srs);
                return srs;
            }
            Ok(srs) => {
                warn!(
                    target: "bridge_prover_lib::keys",
                    path = %exact_path.display(),
                    file_k = srs.k(),
                    circuit_k = k,
                    "SRS file degree is smaller than requested k; ignoring and searching for a larger ceremony"
                );
            }
            Err(e) => {
                warn!(
                    target: "bridge_prover_lib::keys",
                    path = %exact_path.display(),
                    error = %e,
                    "failed to read exact SRS file; searching for a larger ceremony"
                );
            }
        }
    }

    if let Some((src_path, mut srs)) = find_largest_ceremony_ge(params_dir, k) {
        let src_k = srs.k();
        if src_k > k {
            srs.downsize(k);
        }
        info!(
            target: "bridge_prover_lib::keys",
            src = %src_path.display(),
            src_k,
            circuit_k = k,
            dest = %exact_path.display(),
            "provisioned circuit SRS by downsizing parent ceremony"
        );
        if let Err(e) = write_srs_file(&exact_path, &srs) {
            warn!(
                target: "bridge_prover_lib::keys",
                path = %exact_path.display(),
                error = %e,
                "failed to persist downsized SRS; continuing with in-memory params"
            );
        }
        return srs;
    }

    warn!(
        target: "bridge_prover_lib::keys",
        params_dir = %params_dir.display(),
        circuit_k = k,
        "no parent ceremony SRS found; falling back to gen_srs (may synthesise a non-chain trapdoor)"
    );
    // gen_srs reads $PARAMS_DIR for its cache; mirror the old KeyManager
    // pattern (set/restore env var, no lock — single-threaded init).
    let prev_dir = std::env::current_dir().unwrap();
    std::env::set_var("PARAMS_DIR", params_dir.to_str().unwrap());
    let srs = halo2_base::utils::fs::gen_srs(k);
    std::env::set_current_dir(&prev_dir).ok();
    debug_assert_eq!(srs.k(), k, "gen_srs({k}) returned params with k={}", srs.k());
    srs
}

fn read_srs_file(path: &Path) -> std::io::Result<ParamsKZG<Bn256>> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    ParamsKZG::<Bn256>::read(&mut reader)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

fn write_srs_file(path: &Path, srs: &ParamsKZG<Bn256>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(path)?;
    let mut writer = BufWriter::new(file);
    srs.write(&mut writer)?;
    writer.flush()?;
    Ok(())
}

/// Scan `params_dir` for `kzg_bn254_{N}.srs` files whose header degree is
/// ≥ `min_k`, returning the largest such ceremony.
fn find_largest_ceremony_ge(
    params_dir: &Path,
    min_k: u32,
) -> Option<(PathBuf, ParamsKZG<Bn256>)> {
    let entries = std::fs::read_dir(params_dir).ok()?;
    let mut best: Option<(u32, PathBuf, ParamsKZG<Bn256>)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        let Some(k_str) = name
            .strip_prefix("kzg_bn254_")
            .and_then(|s| s.strip_suffix(".srs"))
        else {
            continue;
        };
        // Filename hint only — trust the header after read.
        if k_str.parse::<u32>().is_err() {
            continue;
        }
        let srs = match read_srs_file(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if srs.k() < min_k {
            continue;
        }
        let replace = match &best {
            None => true,
            Some((best_k, _, _)) => srs.k() > *best_k,
        };
        if replace {
            best = Some((srs.k(), path, srs));
        }
    }
    best.map(|(_, path, srs)| (path, srs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_srs(k: u32) -> PathBuf {
        // Prefer repo-root params/ (workspace member lives under crates/an-bridge-prover/).
        let candidates = [
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../params"),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../params"),
            PathBuf::from("params"),
        ];
        for dir in candidates {
            let p = dir.join(format!("kzg_bn254_{k}.srs"));
            if p.exists() {
                return p;
            }
        }
        panic!("missing fixture params/kzg_bn254_{k}.srs — run from repo with params/");
    }

    #[test]
    fn load_srs_downsizes_from_parent_ceremony_when_exact_missing() {
        let src = fixture_srs(12);
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::copy(&src, tmp.path().join("kzg_bn254_12.srs")).unwrap();

        let srs = load_srs(tmp.path(), 10);
        assert_eq!(srs.k(), 10);
        assert!(tmp.path().join("kzg_bn254_10.srs").exists());

        // Same toxic waste as the parent ceremony.
        let parent = read_srs_file(&src).unwrap();
        assert_eq!(format!("{:?}", srs.s_g2()), format!("{:?}", parent.s_g2()));
    }

    #[test]
    fn load_srs_downsizes_misnamed_larger_file() {
        let src = fixture_srs(12);
        let tmp = tempfile::tempdir().expect("tempdir");
        // Partner-style footgun: k=12 bytes living under the k=10 filename.
        std::fs::copy(&src, tmp.path().join("kzg_bn254_10.srs")).unwrap();

        let srs = load_srs(tmp.path(), 10);
        assert_eq!(srs.k(), 10);
        let reread = read_srs_file(&tmp.path().join("kzg_bn254_10.srs")).unwrap();
        assert_eq!(reread.k(), 10);
    }
}

