//! One check for every binary the relayer runs as a subprocess: the path it
//! was configured with exists and can execute. Done at startup, so a wrong
//! `--aggregate-proof-bin` is a configuration error found now, not a failed
//! proof hours later.

use std::path::{Path, PathBuf};

use crate::error::RelayerError;

/// `path` as an absolute path, refused unless it is an executable file.
/// `what` names the binary in the error (`aggregate-proof`, say).
pub fn require_executable(path: &Path, what: &str) -> Result<PathBuf, RelayerError> {
    let refuse =
        |why: String| RelayerError::other(format!("{what} binary {}: {why}", path.display()));
    let meta = std::fs::metadata(path).map_err(|e| refuse(e.to_string()))?;
    if !meta.is_file() {
        return Err(refuse("not a file".into()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return Err(refuse("not executable (chmod +x)".into()));
        }
    }
    path.canonicalize().map_err(|e| refuse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_executable_refuses_what_cannot_run() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let missing = dir.path().join("missing");
        let err = require_executable(&missing, "aggregate-proof")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("aggregate-proof binary") && err.contains("missing"),
            "{err}"
        );
        let err = require_executable(dir.path(), "aggregate-proof")
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a file"), "{err}");
        let plain = dir.path().join("plain");
        std::fs::write(&plain, "x").unwrap();
        let err = require_executable(&plain, "aggregate-proof")
            .unwrap_err()
            .to_string();
        assert!(err.contains("not executable"), "{err}");
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            require_executable(&plain, "aggregate-proof").unwrap(),
            plain.canonicalize().unwrap()
        );
    }
}
