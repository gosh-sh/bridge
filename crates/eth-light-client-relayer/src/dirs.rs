//! The three directories a long-running command writes to
//! (bridge-audit#132): state, work and log, each a flag with an environment
//! variable. Nothing goes to the system temp directory, `$HOME` or the
//! current directory.
//!
//! - **state**: what the command needs to resume after a restart. It names its
//!   file inside the directory ([`STATE_FILE_NAME`] for `daemon`,
//!   [`ANCHORS_STATE_FILE_NAME`] for `arbitrum-anchors`); the lock and the
//!   temporary file of the atomic write sit beside it.
//! - **work**: the provers' inputs and outputs, one subdirectory per proof,
//!   gone once the bundle is read back. Everything here can be computed again.
//!   `arbitrum-anchors` starts no prover and keeps it empty.
//! - **log**: the command's own log (a file per day, beside stdout) and the
//!   transcript of every prover it starts.

use std::path::{Path, PathBuf};

use crate::error::RelayerError;

/// `daemon`'s state file inside its state directory.
pub const STATE_FILE_NAME: &str = "eth-lc-relayer-state.json";
/// `arbitrum-anchors`' state file inside its state directory.
pub const ANCHORS_STATE_FILE_NAME: &str = "arbitrum-anchors-state.json";
/// `daemon`'s own log file inside its log directory; `tracing-appender`
/// adds the day to the name.
pub const LOG_FILE_PREFIX: &str = "eth-lc-relayer.log";
/// `arbitrum-anchors`' own log file inside its log directory.
pub const ANCHORS_LOG_FILE_PREFIX: &str = "arbitrum-anchors.log";

/// State, work and log directories of one command.
#[derive(Clone, Debug)]
pub struct ServiceDirs {
    pub state: PathBuf,
    pub work: PathBuf,
    pub log: PathBuf,
}

impl ServiceDirs {
    /// Create the three directories when missing and refuse any that is not
    /// writable, so a misconfigured mount fails at startup and not at the
    /// first proof.
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

    /// `name` inside the state directory.
    pub fn state_file(&self, name: &str) -> PathBuf {
        self.state.join(name)
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
    fn prepare_creates_missing_directories_and_names_the_state_files() {
        let root = tempfile::tempdir().unwrap();
        let dirs = ServiceDirs {
            state: root.path().join("a/state"),
            work: root.path().join("b/work"),
            log: root.path().join("c/log"),
        };
        dirs.prepare().unwrap();
        assert!(dirs.state.is_dir() && dirs.work.is_dir() && dirs.log.is_dir());
        assert_eq!(
            dirs.state_file(STATE_FILE_NAME),
            root.path()
                .join("a/state")
                .join("eth-lc-relayer-state.json")
        );
        assert_eq!(
            dirs.state_file(ANCHORS_STATE_FILE_NAME),
            root.path()
                .join("a/state")
                .join("arbitrum-anchors-state.json")
        );
        assert!(
            std::fs::read_dir(&dirs.state).unwrap().next().is_none(),
            "no probe left behind"
        );
    }
}
