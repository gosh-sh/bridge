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
use std::time::Instant;

use anyhow::Context;
use halo2_base::gates::circuit::builder::BaseCircuitBuilder;
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::{
        bn256::{Bn256, Fr, G1Affine},
        serde::SerdeObject,
    },
    plonk::{keygen_pk, keygen_vk, Circuit, ProvingKey, VerifyingKey},
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
/// Resolution order:
/// 1. Exact `kzg_bn254_{k}.srs` whose header `k` matches (or is larger →
///    downsize in place and rewrite).
/// 2. Largest on-disk `kzg_bn254_*.srs` with header degree ≥ `k`, downsized
///    and written to the exact path for the next load.
pub(crate) fn load_srs(params_dir: &Path, k: u32) -> ParamsKZG<Bn256> {
    let exact_path = params_dir.join(format!("kzg_bn254_{k}.srs"));

    if exact_path.exists() {
        match read_srs_file(&exact_path) {
            Ok(srs) if srs.k() == k => {
                assert_hermez_ceremony(&exact_path, &srs);
                return srs;
            }
            Ok(mut srs) if srs.k() > k => {
                warn!(
                    target: "bridge_prover_lib::keys",
                    path = %exact_path.display(),
                    file_k = srs.k(),
                    circuit_k = k,
                    "SRS file degree exceeds requested k; downsizing (likely a misnamed larger ceremony)"
                );
                assert_hermez_ceremony(&exact_path, &srs);
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
        assert_hermez_ceremony(&src_path, &srs);
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
            "provisioned circuit SRS by downsizing parent Hermez ceremony"
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

    panic!(
        "no Hermez Perpetual Powers of Tau SRS (≥ k={k}) under {} — \
         run scripts/bootstrap_hermez_srs.sh (and convert k=21 from \
         powersOfTau28_hez_final_21.ptau). Chain-ceremony / gen_srs \
         fallbacks are disabled.",
        params_dir.display()
    );
}

/// Hermez `s_g2` head (`928fafb3d0cc…`). Rejects Acki Nacki chain ceremony
/// (`c6028acf…`) and any synthetic `gen_srs` trapdoor. Re-exported from
/// `bridge_prover_lib::keys` so the `bootstrap_hermez_srs` binary can share
/// the same anchor byte-string.
pub const HERMEZ_S_G2_HEAD: [u8; 6] = [0x92, 0x8f, 0xaf, 0xb3, 0xd0, 0xcc];

fn assert_hermez_ceremony(path: &Path, srs: &ParamsKZG<Bn256>) {
    let mut buf = Vec::with_capacity(128);
    srs.s_g2()
        .write_raw(&mut buf)
        .expect("write to Vec cannot fail");
    if buf.len() < HERMEZ_S_G2_HEAD.len() {
        panic!(
            "SRS {} s_g2 encoding too small ({} bytes)",
            path.display(),
            buf.len()
        );
    }
    let head = &buf[..HERMEZ_S_G2_HEAD.len()];
    if head != HERMEZ_S_G2_HEAD {
        panic!(
            "SRS {} is not Hermez Perpetual Powers of Tau (s_g2 head {:02x?}, \
             expected {:02x?}). Quarantine chain-ceremony files and bootstrap \
             Hermez SRS — no fallbacks.",
            path.display(),
            head,
            HERMEZ_S_G2_HEAD
        );
    }
}

/// Public Result-based sibling of the private `assert_hermez_ceremony` above,
/// for callers outside `load_srs` that want to bail rather than panic (e.g.
/// the offline snark exporters in `bridge-snark-utils` which call
/// `gen_srs` directly). Same anchor bytes, same failure mode — no
/// toxic-waste SRS proceeds past this check.
pub fn assert_hermez_srs(srs: &ParamsKZG<Bn256>) -> anyhow::Result<()> {
    let mut buf = Vec::with_capacity(128);
    srs.s_g2()
        .write_raw(&mut buf)
        .expect("write to Vec cannot fail");
    if buf.len() < HERMEZ_S_G2_HEAD.len() {
        anyhow::bail!(
            "SRS s_g2 encoding too small ({} bytes) — SRS at k={} is malformed",
            buf.len(),
            srs.k(),
        );
    }
    let head = &buf[..HERMEZ_S_G2_HEAD.len()];
    if head != HERMEZ_S_G2_HEAD {
        anyhow::bail!(
            "SRS (k={}) is NOT Hermez Perpetual Powers of Tau \
             (s_g2 head {:02x?}, expected {:02x?}). PARAMS_DIR likely lacks \
             kzg_bn254_{}.srs and `gen_srs` silently generated a toxic-waste \
             SRS whose tau is known to the local process — every proof \
             produced with it is forgeable. Bootstrap via \
             `bootstrap_hermez_srs`. REFUSING to proceed.",
            srs.k(),
            head,
            HERMEZ_S_G2_HEAD,
            srs.k(),
        );
    }
    Ok(())
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
        // Skip non-Hermez ceremonies (e.g. leftover chain-ceremony files).
        let Ok(raw) = std::fs::read(&path) else { continue };
        if raw.len() < 128 || &raw[raw.len() - 128..raw.len() - 122] != HERMEZ_S_G2_HEAD {
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

// ─────────────────────────────────────────────────────────────────────
// Shared per-circuit manager state
// ─────────────────────────────────────────────────────────────────────

/// SRS + optional VK/PK/config cache with disk-backed lifecycle.
/// Composed by each per-circuit manager (`PrimaryKeyManager`, etc.) so the
/// mechanical parts (SRS load, cache-hit check, keygen timing/logging,
/// save-and-set, on-demand PK load/unload, accessor panic messages) live in
/// exactly one place. Per-circuit files only carry their circuit-specific
/// bits: witness construction inside `ensure_keys`, degree constants, and
/// (for `LayerHashesKeyManager`) any extra accessors.
pub(crate) struct KeyManagerState {
    params_dir: PathBuf,
    prefix: &'static str,
    /// Operator-facing hint appended to the `load_pk` log line (e.g.
    /// `"~3.7 GB"`). Purely cosmetic — wall-clock elapsed is always logged
    /// after the load regardless.
    pk_size_hint: Option<&'static str>,
    srs: ParamsKZG<Bn256>,
    k: u32,
    vk: Option<VerifyingKey<G1Affine>>,
    pk: Option<ProvingKey<G1Affine>>,
    config: Option<BaseCircuitParams>,
}

impl KeyManagerState {
    /// Construct: load SRS at `srs_k` (may exceed circuit `k` when the
    /// ceremony was oversized — see `LayerHashesKeyManager::KEYGEN_SRS_K`
    /// / `EventKeyManager::KEYGEN_SRS_K`), best-effort load any cached
    /// config/VK from disk; log if PK file is present (loaded on demand).
    pub(crate) fn new(
        params_dir: &Path,
        prefix: &'static str,
        k: u32,
        srs_k: u32,
        pk_size_hint: Option<&'static str>,
    ) -> Self {
        std::fs::create_dir_all(params_dir).ok();
        let srs = load_srs(params_dir, srs_k);
        let mut state = Self {
            params_dir: params_dir.to_path_buf(),
            prefix,
            pk_size_hint,
            srs,
            k,
            vk: None,
            pk: None,
            config: None,
        };
        if let Ok(config) = load_config(&state.params_dir, prefix) {
            info!("found {} config: {:?}", prefix, config);
            if let Some(vk) = try_load_vk(&state.params_dir, prefix, &config) {
                info!("loaded {} VK from cache", prefix);
                state.vk = Some(vk);
            }
            if pk_path(&state.params_dir, prefix).exists() {
                info!("{} PK found on disk (will load on demand)", prefix);
            }
            state.config = Some(config);
        }
        state
    }

    /// True iff `ensure_keys` can skip regeneration: VK in memory AND PK
    /// present on disk.
    pub(crate) fn keys_cached(&self) -> bool {
        self.vk.is_some() && pk_path(&self.params_dir, self.prefix).exists()
    }

    /// Run keygen_vk + keygen_pk with prefix-labelled timing/logging, then
    /// persist VK + PK + config to disk and install VK + config into `self`.
    /// The PK is intentionally dropped after save (freeing ~2.8–3.7 GB for
    /// the BLS circuits); callers reload on demand via [`Self::load_pk`].
    pub(crate) fn run_keygen<C>(
        &mut self,
        circuit: &C,
        base_params: BaseCircuitParams,
    ) -> anyhow::Result<()>
    where
        C: Circuit<Fr>,
    {
        info!("{} base_circuit_params: {:?}", self.prefix, base_params);

        let t = Instant::now();
        let vk = keygen_vk(&self.srs, circuit)
            .with_context(|| format!("{} keygen_vk failed", self.prefix))?;
        info!("{} keygen_vk: {:?}", self.prefix, t.elapsed());

        let t = Instant::now();
        let pk = keygen_pk(&self.srs, vk.clone(), circuit)
            .with_context(|| format!("{} keygen_pk failed", self.prefix))?;
        info!("{} keygen_pk: {:?}", self.prefix, t.elapsed());

        save_vk(&self.params_dir, self.prefix, &vk)?;
        save_pk(&self.params_dir, self.prefix, &pk)?;
        save_config(&self.params_dir, self.prefix, &base_params)?;

        self.vk = Some(vk);
        self.config = Some(base_params);
        // `pk` drops here — memory freed; reload on demand via `load_pk`.
        Ok(())
    }

    /// Load PK from disk into memory. Idempotent — no-op if already loaded.
    /// Requires `ensure_keys` (or a cached config on disk) to have populated
    /// `self.config` first.
    pub(crate) fn load_pk(&mut self) -> anyhow::Result<()> {
        if self.pk.is_some() {
            return Ok(());
        }
        let config = self.config.as_ref().ok_or_else(|| {
            anyhow::format_err!("{} config not loaded — run ensure_keys first", self.prefix)
        })?;
        match self.pk_size_hint {
            Some(hint) => {
                info!("loading {} PK from disk ({})...", self.prefix, hint)
            }
            None => info!("loading {} PK from disk...", self.prefix),
        }
        let t = Instant::now();
        let pk = try_load_pk(&self.params_dir, self.prefix, config).ok_or_else(|| {
            anyhow::format_err!(
                "failed to load {} PK from {}",
                self.prefix,
                pk_path(&self.params_dir, self.prefix).display()
            )
        })?;
        info!("{} PK loaded in {:?}", self.prefix, t.elapsed());
        self.pk = Some(pk);
        Ok(())
    }

    /// Drop PK from memory. Cheap idempotent no-op if already unloaded.
    pub(crate) fn unload_pk(&mut self) {
        if self.pk.is_some() {
            self.pk = None;
            info!("{} PK unloaded from memory", self.prefix);
        }
    }

    // ---- accessors ----

    pub(crate) fn params_dir(&self) -> &Path {
        &self.params_dir
    }
    pub(crate) fn srs(&self) -> &ParamsKZG<Bn256> {
        &self.srs
    }
    pub(crate) fn k(&self) -> u32 {
        self.k
    }
    pub(crate) fn vk_opt(&self) -> Option<&VerifyingKey<G1Affine>> {
        self.vk.as_ref()
    }
    pub(crate) fn vk(&self) -> &VerifyingKey<G1Affine> {
        self.vk
            .as_ref()
            .unwrap_or_else(|| panic!("{} VK not loaded", self.prefix))
    }
    pub(crate) fn pk(&self) -> &ProvingKey<G1Affine> {
        self.pk
            .as_ref()
            .unwrap_or_else(|| panic!("{} PK not loaded", self.prefix))
    }
    pub(crate) fn config(&self) -> &BaseCircuitParams {
        self.config
            .as_ref()
            .unwrap_or_else(|| panic!("{} config not loaded", self.prefix))
    }
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

