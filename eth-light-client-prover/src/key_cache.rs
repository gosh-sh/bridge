//! Proving keys kept between runs of the rotate provers (`ROTATE_KEY_DIR`).
//!
//! A key depends only on its circuit and SRS, so one stored key serves every
//! period until the prover binary changes. Each key is tied to the SHA-256 of
//! the binary that generated it: a key of another build is regenerated and
//! overwritten, never used.
//!
//! Layout: `ROTATE_KEY_DIR/<name>/` holds `params.json`, `break_points.json`,
//! `build.sha256` and `pk.bin`. Every file goes through a temporary name and a
//! rename, and `pk.bin` is written last, so its presence means the rest is
//! whole: a keygen killed half-way leaves no key.

use std::{
    fs,
    io::{BufReader, BufWriter},
    path::{Path, PathBuf},
};

use halo2_base::{
    gates::flex_gate::MultiPhaseThreadBreakPoints,
    halo2_proofs::{
        halo2curves::bn256::{Fr, G1Affine},
        plonk::{Circuit, ProvingKey},
        SerdeFormat,
    },
};
use serde::{de::DeserializeOwned, Serialize};

pub const ENV_ROTATE_KEY_DIR: &str = "ROTATE_KEY_DIR";

const PARAMS_FILE: &str = "params.json";
const BREAK_POINTS_FILE: &str = "break_points.json";
const BUILD_FILE: &str = "build.sha256";
const PK_FILE: &str = "pk.bin";

/// A stored key: the parameters the circuit was configured with, its break
/// points and the proving key.
pub struct StoredKey<P> {
    pub params: P,
    pub break_points: MultiPhaseThreadBreakPoints,
    pub pk: ProvingKey<G1Affine>,
}

/// The key directory of one build of one binary.
#[derive(Clone, Debug)]
pub struct KeyCache {
    dir: PathBuf,
    build: String,
}

impl KeyCache {
    pub fn new(dir: impl Into<PathBuf>, build: impl Into<String>) -> Self {
        Self {
            dir: dir.into(),
            build: build.into(),
        }
    }

    /// The cache `ROTATE_KEY_DIR` names, tied to this binary; `None` when the
    /// variable is unset or empty.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let Some(dir) = std::env::var_os(ENV_ROTATE_KEY_DIR).filter(|d| !d.is_empty()) else {
            return Ok(None);
        };
        Ok(Some(Self::new(PathBuf::from(dir), this_build()?)))
    }

    /// The same directory for keys that also depend on `tag`, such as the
    /// verifying key of a snark the circuit folds: a key stored under another
    /// tag reads as a key of another build.
    pub fn with_tag(&self, tag: &str) -> Self {
        Self::new(&self.dir, format!("{}+{tag}", self.build))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn key_dir(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// The key `name`, or `None` when there is none or it belongs to another
    /// build. `circuit_params` turns the stored parameters into the ones the
    /// proving key is read with.
    pub fn load<C, P>(
        &self,
        name: &str,
        circuit_params: impl FnOnce(&P) -> C::Params,
    ) -> anyhow::Result<Option<StoredKey<P>>>
    where
        C: Circuit<Fr>,
        P: DeserializeOwned,
    {
        let dir = self.key_dir(name);
        if !dir.join(PK_FILE).exists() {
            return Ok(None);
        }
        let stored = fs::read_to_string(dir.join(BUILD_FILE)).unwrap_or_default();
        if stored.trim() != self.build {
            println!(
                "key {name} in {} is of another build ({}); this build is {}: regenerating it",
                dir.display(),
                if stored.trim().is_empty() { "none recorded" } else { stored.trim() },
                self.build
            );
            return Ok(None);
        }
        let params: P = serde_json::from_slice(&fs::read(dir.join(PARAMS_FILE))?)?;
        let break_points = serde_json::from_slice(&fs::read(dir.join(BREAK_POINTS_FILE))?)?;
        let mut reader = BufReader::new(fs::File::open(dir.join(PK_FILE))?);
        let pk = ProvingKey::<G1Affine>::read::<_, C>(
            &mut reader,
            SerdeFormat::RawBytesUnchecked,
            circuit_params(&params),
        )
        .map_err(|e| anyhow::anyhow!("read {}: {e}", dir.join(PK_FILE).display()))?;
        Ok(Some(StoredKey {
            params,
            break_points,
            pk,
        }))
    }

    /// Stores the key `name` of this build, replacing whatever was there.
    pub fn store<P: Serialize>(
        &self,
        name: &str,
        params: &P,
        break_points: &MultiPhaseThreadBreakPoints,
        pk: &ProvingKey<G1Affine>,
    ) -> anyhow::Result<()> {
        let dir = self.key_dir(name);
        fs::create_dir_all(&dir)?;
        // The old key goes first: new parameters beside it would read as whole.
        match fs::remove_file(dir.join(PK_FILE)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {},
        }
        write_atomic(&dir, PARAMS_FILE, &serde_json::to_vec_pretty(params)?)?;
        write_atomic(&dir, BREAK_POINTS_FILE, &serde_json::to_vec(break_points)?)?;
        write_atomic(&dir, BUILD_FILE, self.build.as_bytes())?;
        let tmp = dir.join(format!("{PK_FILE}.tmp"));
        let mut out = BufWriter::new(fs::File::create(&tmp)?);
        pk.write(&mut out, SerdeFormat::RawBytesUnchecked)?;
        out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        fs::rename(&tmp, dir.join(PK_FILE))?;
        Ok(())
    }
}

/// SHA-256 of the running binary: what a stored key is tied to.
pub fn this_build() -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    Ok(hex::encode(Sha256::digest(fs::read(std::env::current_exe()?)?)))
}

/// Writes `bytes` to `dir/name` through a temporary file.
pub fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let tmp = dir.join(format!("{name}.tmp"));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, dir.join(name))?;
    Ok(())
}
