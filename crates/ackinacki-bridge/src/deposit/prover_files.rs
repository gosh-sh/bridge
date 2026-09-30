//! The deposit prover directory: two binaries, the circuit config and
//! the SRS, laid out the way the prover expects relative to its working
//! directory. Every check here runs before a wallet is asked for
//! anything: a proof keyed on the wrong SRS or circuit shape is refused
//! on chain only after twenty minutes of proving.

use std::{
    io::{Read, Seek, SeekFrom},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use crate::deposit::limits::PROVER_DEGREE;

/// The binary that fetches the deposit witness data.
pub const FETCH_BIN: &str = "fetch_deposit_data";
/// The binary that produces the proof.
pub const PROVE_BIN: &str = "export_blake2b_proof";
/// The binary that exports the verification key blob.
pub const VK_BIN: &str = "export_vk_blob";
/// The circuit config, relative to the prover directory.
pub const CIRCUIT_PARAMS: &str = "configs/circuit_params.json";
/// The SRS file, relative to the prover directory.
pub const SRS_FILE: &str = "data/kzg_params_18.srs";
/// The lookup bits the circuit is built with.
pub const LOOKUP_BITS: u64 = 8;
/// First six bytes of the Hermez ceremony's [s]·G2, which sits in the
/// final 128 bytes of a raw halo2 SRS file.
pub const HERMEZ_SG2_HEAD: [u8; 6] = [0x92, 0x8f, 0xaf, 0xb3, 0xd0, 0xcc];
/// Last six bytes of the Hermez ceremony's [s]·G2.
pub const HERMEZ_SG2_TAIL: [u8; 6] = [0xb3, 0xbe, 0x59, 0x5c, 0x69, 0x00];

/// A prover directory that passed `check_prover_dir`.
#[derive(Debug, Clone)]
pub struct ProverDir {
    /// The directory itself.
    pub root: PathBuf,
}

impl ProverDir {
    /// Path of the named binary inside the directory.
    pub fn bin(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// The prover's data directory.
    pub fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }

    /// The lock that serialises proving in this directory.
    pub fn lock_path(&self) -> PathBuf {
        self.data_dir().join(".prove.lock")
    }
}

/// Refuses a circuit config whose shape differs from the one the prover
/// is run with.
pub fn check_circuit_params(json: &str) -> Result<(), String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("{CIRCUIT_PARAMS} is not JSON: {e}"))?;
    let k = v.pointer("/base/k").and_then(|x| x.as_u64());
    if k != Some(PROVER_DEGREE as u64) {
        return Err(format!(
            "{CIRCUIT_PARAMS}: base.k is {k:?}, expected {PROVER_DEGREE} (the degree passed to \
             the prover)"
        ));
    }
    let lb = v.pointer("/base/lookup_bits").and_then(|x| x.as_u64());
    if lb != Some(LOOKUP_BITS) {
        return Err(format!(
            "{CIRCUIT_PARAMS}: base.lookup_bits is {lb:?}, expected {LOOKUP_BITS}"
        ));
    }
    Ok(())
}

/// Refuses an SRS file that does not come from the Hermez ceremony.
pub fn check_srs_ceremony(path: &Path) -> Result<(), String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let len = f
        .metadata()
        .map_err(|e| format!("{}: {e}", path.display()))?
        .len();
    if len < 128 {
        return Err(format!(
            "{} is {len} bytes, too short to be an SRS",
            path.display()
        ));
    }
    let mut tail = [0u8; 128];
    f.seek(SeekFrom::End(-128))
        .and_then(|_| f.read_exact(&mut tail))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if tail[..6] != HERMEZ_SG2_HEAD || tail[122..] != HERMEZ_SG2_TAIL {
        return Err(format!(
            "{} is not the Hermez ceremony ([s]·G2 {}…{}); a proof keyed on it is rejected by the \
             bridge. Download it again with the installer",
            path.display(),
            hex::encode(&tail[..6]),
            hex::encode(&tail[122..])
        ));
    }
    Ok(())
}

/// Checks the binaries, the circuit config, the writable data directory
/// and the SRS, and returns the directory on success.
pub fn check_prover_dir(root: &Path) -> Result<ProverDir, String> {
    let dir = ProverDir {
        root: root.to_path_buf(),
    };
    for b in [FETCH_BIN, PROVE_BIN] {
        let p = dir.bin(b);
        let m = std::fs::metadata(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        if !m.is_file() || m.permissions().mode() & 0o111 == 0 {
            return Err(format!("{} is not an executable file", p.display()));
        }
    }
    let params = root.join(CIRCUIT_PARAMS);
    let text = std::fs::read_to_string(&params).map_err(|e| {
        format!(
            "{CIRCUIT_PARAMS} is missing from {} ({e}); the prover reads it on every run, even \
             with a cached proving key",
            root.display()
        )
    })?;
    check_circuit_params(&text)?;
    let probe = dir.data_dir().join(".write-probe");
    std::fs::write(&probe, b"")
        .map_err(|e| format!("{} is not writable: {e}", dir.data_dir().display()))?;
    #[expect(
        clippy::let_underscore_must_use,
        reason = "a leftover probe file is harmless"
    )]
    let _ = std::fs::remove_file(&probe);
    check_srs_ceremony(&root.join(SRS_FILE))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use std::{os::unix::fs::PermissionsExt, path::Path};

    use super::*;

    const REPO_PARAMS: &str =
        include_str!("../../../../deposit-prover/configs/circuit_params.json");

    fn fake_srs(tail: &[u8; 128]) -> Vec<u8> {
        let mut v = vec![0u8; 4096];
        v.extend_from_slice(tail);
        v
    }

    fn hermez_tail() -> [u8; 128] {
        let mut t = [0u8; 128];
        t[..6].copy_from_slice(&HERMEZ_SG2_HEAD);
        t[122..].copy_from_slice(&HERMEZ_SG2_TAIL);
        t
    }

    fn layout(dir: &Path, params: Option<&str>, srs: Option<Vec<u8>>) {
        for b in [FETCH_BIN, PROVE_BIN, VK_BIN] {
            let p = dir.join(b);
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::create_dir_all(dir.join("configs")).unwrap();
        std::fs::create_dir_all(dir.join("data")).unwrap();
        if let Some(p) = params {
            std::fs::write(dir.join(CIRCUIT_PARAMS), p).unwrap();
        }
        if let Some(s) = srs {
            std::fs::write(dir.join(SRS_FILE), s).unwrap();
        }
    }

    #[test]
    fn the_repository_config_passes() {
        check_circuit_params(REPO_PARAMS).unwrap();
    }

    #[test]
    fn a_config_for_another_circuit_shape_is_refused() {
        let k15 = REPO_PARAMS.replace("\"k\": 18", "\"k\": 15");
        assert!(check_circuit_params(&k15).unwrap_err().contains("k"));
        let lb = REPO_PARAMS.replace("\"lookup_bits\": 8", "\"lookup_bits\": 7");
        assert!(check_circuit_params(&lb)
            .unwrap_err()
            .contains("lookup_bits"));
    }

    #[test]
    fn a_complete_directory_passes() {
        let d = tempfile::tempdir().unwrap();
        layout(d.path(), Some(REPO_PARAMS), Some(fake_srs(&hermez_tail())));
        check_prover_dir(d.path()).unwrap();
    }

    #[test]
    fn a_missing_config_is_named_even_with_everything_else_present() {
        let d = tempfile::tempdir().unwrap();
        layout(d.path(), None, Some(fake_srs(&hermez_tail())));
        assert!(check_prover_dir(d.path())
            .unwrap_err()
            .contains(CIRCUIT_PARAMS));
    }

    #[test]
    fn another_ceremony_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let mut tail = hermez_tail();
        tail[0] ^= 1;
        layout(d.path(), Some(REPO_PARAMS), Some(fake_srs(&tail)));
        assert!(check_prover_dir(d.path()).unwrap_err().contains("Hermez"));
    }
}
