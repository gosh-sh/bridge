//! The three directories the service writes to (bridge-audit#132).
//!
//! Every long-running command takes them as flags with environment variables
//! and writes nowhere else: not the system temp directory, not `$HOME`, not
//! the current directory.
//!
//! - **state**: what the daemon needs to resume after a restart. The daemon
//!   names its files inside it ([`STATE_FILE_NAME`]); the lock file and the
//!   temporary file of the atomic write sit beside them.
//! - **work**: the provers' inputs and outputs. Everything here can be computed
//!   again; one subdirectory per proof, removed when the proof is read back.
//! - **log**: the daemon's own log (a file per day, beside the stdout stream)
//!   and the output of every prover it starts, one file per invocation.

use std::path::{Path, PathBuf};

use crate::error::RelayerError;

/// The daemon's state file inside the state directory.
pub const STATE_FILE_NAME: &str = "deposit-relayer-state.json";

/// The daemon's own log file inside the log directory; `tracing-appender`
/// adds the day to the name.
pub const LOG_FILE_PREFIX: &str = "deposit-relayer.log";

/// State, work and log directories of one service instance.
#[derive(Clone, Debug)]
pub struct ServiceDirs {
    pub state: PathBuf,
    pub work: PathBuf,
    pub log: PathBuf,
}

impl ServiceDirs {
    /// Create the three directories when missing and refuse any that is not
    /// writable, so a misconfigured mount fails at startup and not at the
    /// first deposit.
    pub fn prepare(&self) -> Result<(), RelayerError> {
        for (what, dir) in [
            ("state", &self.state),
            ("work", &self.work),
            ("log", &self.log),
        ] {
            prepare_dir(what, dir)?;
        }
        Ok(())
    }

    /// The state file path: [`STATE_FILE_NAME`] inside the state directory.
    pub fn state_file(&self) -> PathBuf {
        self.state.join(STATE_FILE_NAME)
    }
}

/// Create `dir` when missing and check that a file can be created in it.
pub fn prepare_dir(what: &str, dir: &Path) -> Result<(), RelayerError> {
    let fail =
        |why: String| RelayerError::other(format!("{what} directory {}: {why}", dir.display()));
    std::fs::create_dir_all(dir).map_err(|e| fail(e.to_string()))?;
    if !dir.is_dir() {
        return Err(fail("not a directory".into()));
    }
    let probe = dir.join(format!(".write-probe-{}", std::process::id()));
    std::fs::write(&probe, b"").map_err(|e| fail(format!("not writable: {e}")))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_creates_missing_directories_and_names_the_state_file() {
        let root = tempfile::tempdir().unwrap();
        let dirs = ServiceDirs {
            state: root.path().join("a/state"),
            work: root.path().join("b/work"),
            log: root.path().join("c/log"),
        };
        dirs.prepare().unwrap();
        assert!(dirs.state.is_dir() && dirs.work.is_dir() && dirs.log.is_dir());
        assert_eq!(
            dirs.state_file(),
            root.path().join("a/state").join(STATE_FILE_NAME)
        );
        assert!(
            std::fs::read_dir(&dirs.state).unwrap().next().is_none(),
            "no probe left behind"
        );
    }

    #[cfg(unix)]
    #[test]
    fn prepare_refuses_a_read_only_directory() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let ro = root.path().join("ro");
        std::fs::create_dir(&ro).unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
        // root ignores file modes; the check is meaningful for a service user.
        if std::fs::write(ro.join("x"), b"").is_ok() {
            std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        let err = prepare_dir("work", &ro).unwrap_err().to_string();
        assert!(err.contains("not writable"), "{err}");
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}
