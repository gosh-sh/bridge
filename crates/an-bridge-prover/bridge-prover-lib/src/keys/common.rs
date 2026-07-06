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

use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

use halo2_base::gates::circuit::builder::BaseCircuitBuilder;
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Fr, G1Affine},
    plonk::{ProvingKey, VerifyingKey},
    SerdeFormat,
};

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

/// Load an SRS at degree `k` from `params_dir`, using halo2-base's disk
/// cache. On cache miss `gen_srs` synthesises a deterministic test SRS
/// (known trapdoor) — production code MUST pre-populate the ceremony file.
pub(crate) fn load_srs(
    params_dir: &Path,
    k: u32,
) -> halo2_base::halo2_proofs::poly::kzg::commitment::ParamsKZG<
    halo2_base::halo2_proofs::halo2curves::bn256::Bn256,
> {
    // gen_srs reads $PARAMS_DIR for its cache; mirror the old KeyManager
    // pattern (set/restore CWD env var, no lock — single-threaded init).
    let prev_dir = std::env::current_dir().unwrap();
    std::env::set_var("PARAMS_DIR", params_dir.to_str().unwrap());
    let srs = halo2_base::utils::fs::gen_srs(k);
    std::env::set_current_dir(&prev_dir).ok();
    srs
}
