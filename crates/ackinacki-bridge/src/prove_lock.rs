//! `<snark-dir>/.prove.lock`: withdrawals that share a `--snark-dir` prove one
//! at a time.
//!
//! Stage 5 writes the Circuit-4 snark and its instances into the snark
//! directory under fixed names, and the aggregator reads them back. Two
//! withdrawals proving there at once overwrite each other's files; the
//! aggregator's binding check then refuses one of them, after its burn. Two
//! proofs at once also take twice the memory.
//!
//! `flock(2)`, so a process that dies lets go. Waiting polls a non-blocking
//! `flock`, as the deposit locks do, so a signal during the wait is not held
//! up by a thread nobody can cancel. On a filesystem without file locks the
//! proof runs unlocked, with a warning — the way the withdrawal lock behaves
//! there.

use std::path::{Path, PathBuf};

use crate::errors::{CliError, CliResult};

/// The held lock; dropping it lets the next withdrawal prove.
#[derive(Debug)]
pub(crate) struct ProveLock {
    _file: std::fs::File,
}

/// Where the lock lives for `snark_dir`.
pub(crate) fn lock_path(snark_dir: &Path) -> PathBuf {
    snark_dir.join(".prove.lock")
}

/// Take the lock on `snark_dir`, waiting while another withdrawal proves
/// there. `Ok(None)`: the filesystem has no file locks and the proof runs
/// unlocked. Every failure is exit 12 — this runs after the burn.
pub(crate) async fn take(snark_dir: &Path) -> CliResult<Option<ProveLock>> {
    use std::os::unix::io::AsRawFd;

    use crate::idempotency::{classify_flock_error, FlockVerdict};

    let path = lock_path(snark_dir);
    let refuse = |what: String| CliError::ProofFailed {
        reason: format!("the prove lock {}: {what}", path.display()),
        source: None,
    };
    std::fs::create_dir_all(snark_dir)
        .map_err(|e| refuse(format!("cannot create --snark-dir: {e}")))?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| refuse(format!("cannot open: {e}")))?;
    let mut waiting = false;
    loop {
        // SAFETY: `file` outlives the call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            if waiting {
                tracing::info!(snark_dir = %snark_dir.display(), "the prove lock is ours; proving");
            }
            return Ok(Some(ProveLock {
                _file: file,
            }));
        }
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        match classify_flock_error(e.raw_os_error()) {
            FlockVerdict::Contended => {
                if !waiting {
                    tracing::info!(
                        snark_dir = %snark_dir.display(),
                        "another withdrawal is proving in this --snark-dir; waiting for it to \
                         finish",
                    );
                    waiting = true;
                }
                tokio::time::sleep(crate::deposit::locks::WAIT_POLL).await;
            },
            FlockVerdict::Unsupported => {
                tracing::warn!(
                    path = %path.display(),
                    "this filesystem has no file locks; proving without the prove lock, so do \
                     not run two withdrawals with this --snark-dir at once",
                );
                return Ok(None);
            },
            FlockVerdict::Fatal => return Err(refuse(format!("cannot lock: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn a_second_withdrawal_waits_for_the_first_to_finish_proving() {
        let _relocking = crate::test_forks::relocking();
        let dir = tempfile::tempdir().unwrap();
        let first = take(dir.path())
            .await
            .unwrap()
            .expect("a local filesystem locks");

        let path = dir.path().to_path_buf();
        let second = tokio::spawn(async move { take(&path).await });
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            !second.is_finished(),
            "the second proof must wait while the first holds the lock"
        );

        drop(first);
        let second = tokio::time::timeout(Duration::from_secs(5), second)
            .await
            .expect("the wait ends once the holder lets go")
            .unwrap()
            .unwrap();
        assert!(second.is_some());
        assert!(lock_path(dir.path()).exists());
    }

    #[tokio::test]
    async fn a_snark_dir_that_cannot_hold_the_lock_is_exit_12() {
        let dir = tempfile::tempdir().unwrap();
        let not_a_dir = dir.path().join("file");
        std::fs::write(&not_a_dir, b"x").unwrap();
        let err = take(&not_a_dir)
            .await
            .expect_err("a file is not a snark directory");
        assert_eq!(
            err.exit_code().as_i32(),
            12,
            "the burn is on the wire: {err}"
        );
        assert!(format!("{err}").contains(".prove.lock"), "{err}");
    }

    #[test]
    fn stage_5_proves_under_the_lock() {
        let src = crate::source_guard::production_source(
            "orchestrator.rs",
            include_str!("orchestrator.rs"),
        );
        let lock = src
            .find("prove_lock::take(")
            .expect("stage 5 takes the lock");
        let prove = src.find("run_once_with_state(").expect("the proof");
        assert!(lock < prove, "the lock is taken before the proof starts");
    }
}
