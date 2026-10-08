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
    /// Another withdrawal held the lock when this run asked for it.
    waited: bool,
}

impl ProveLock {
    /// Whether this run queued behind another proof. Its bridge snapshot is
    /// then as old as the wait, which has no bound.
    pub(crate) fn waited(&self) -> bool {
        self.waited
    }
}

/// Where the lock lives for `snark_dir`.
pub(crate) fn lock_path(snark_dir: &Path) -> PathBuf {
    snark_dir.join(".prove.lock")
}

/// Open the lock file, creating it if needed. Another user's file that
/// this one may not write is opened read-only: `flock` needs only an open
/// file.
fn open_lock_file(path: &Path) -> std::io::Result<std::fs::File> {
    let read_write = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path);
    match read_write {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => std::fs::File::open(path),
        other => other,
    }
}

/// Stage 1's half of [`take`]: open the lock file the way stage 5 will,
/// without locking it, so a lock file this user cannot open — another
/// user's, in shared scratch storage — refuses with exit 2 before the burn
/// instead of with exit 12 after it. The `--snark-dir` itself is already
/// created and probed by `check_output_dirs`.
pub(crate) fn check_openable(snark_dir: &Path) -> CliResult<()> {
    let path = lock_path(snark_dir);
    open_lock_file(&path)
        .map(drop)
        .map_err(|e| CliError::Preflight {
            reason: format!(
                "--snark-dir: cannot open the prove lock {}: {e}. Stage 5 takes it after the \
                 burn. Fix its permissions, or point --snark-dir at a directory of your own.",
                path.display()
            ),
            source: None,
        })
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
    let file = open_lock_file(&path).map_err(|e| refuse(format!("cannot open: {e}")))?;
    let mut waiting = false;
    loop {
        // SAFETY: `file` outlives the call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            if waiting {
                tracing::info!(snark_dir = %snark_dir.display(), "the prove lock is ours; proving");
            }
            return Ok(Some(ProveLock {
                _file: file,
                waited: waiting,
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
        assert!(!first.waited(), "a free lock is taken without waiting");

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
        let second = second.expect("a local filesystem locks");
        assert!(
            second.waited(),
            "a run that queued says so: its bridge snapshot is as old as the wait"
        );
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

    #[tokio::test]
    async fn a_lock_file_this_user_cannot_write_is_still_taken() {
        // Another user's `.prove.lock` in a shared scratch directory:
        // `flock` needs only an open file, so a read-only one serves.
        use std::os::unix::fs::PermissionsExt;
        let _relocking = crate::test_forks::relocking();
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(dir.path());
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        check_openable(dir.path()).expect("a read-only lock file passes preflight");
        let held = take(dir.path())
            .await
            .expect("a read-only lock file is still a lock")
            .expect("and this filesystem locks");
        assert!(!held.waited());
    }

    #[test]
    fn a_lock_file_this_user_cannot_open_is_refused_before_the_burn() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(dir.path());
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root opens anything: nothing to learn here then.
        if std::fs::File::open(&path).is_ok() {
            return;
        }
        let err = check_openable(dir.path()).expect_err("stage 5 could not take this lock");
        assert_eq!(err.exit_code().as_i32(), 2, "nothing is sent yet: {err}");
        assert!(format!("{err}").contains(".prove.lock"), "{err}");
    }

    #[test]
    fn preflight_checks_that_the_prove_lock_opens() {
        let src =
            crate::source_guard::production_source("preflight.rs", include_str!("preflight.rs"));
        let artifacts = src
            .find("pub async fn check_prover_artifacts(")
            .expect("the artifact checks");
        let body = &src[artifacts..];
        let end = body.find("\n}\n").expect("the function ends");
        assert!(
            body[..end].contains("prove_lock::check_openable(snark_dir)"),
            "the lock stage 5 opens is opened before the burn"
        );
    }

    #[test]
    fn the_proving_key_is_rechecked_after_the_lock_wait() {
        // The wait for the lock has no bound, and a key replaced during it
        // would be read by the proof with nothing checking it.
        let src = crate::source_guard::production_source(
            "orchestrator.rs",
            include_str!("orchestrator.rs"),
        );
        let lock = src.find("prove_lock::take(").expect("the lock");
        let after_lock = &src[lock..];
        let refresh = after_lock
            .find("wait_for_coverage(")
            .expect("the snapshot refresh");
        let recheck = after_lock
            .find("recheck_proving_key(")
            .expect("the key is rechecked after the lock");
        let prove = after_lock.find("run_once_with_state(").expect("the proof");
        assert!(refresh < recheck && recheck < prove);
        assert_eq!(
            src.matches("recheck_proving_key(").count(),
            1,
            "once, right before the proof"
        );
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

    #[test]
    fn a_run_that_waited_for_the_lock_proves_against_a_fresh_snapshot() {
        // The snapshot stage 4b read is as old as the wait for the lock, and
        // the wait has no bound: an anchor can leave the window meanwhile,
        // and the proof would bind to it anyway.
        let src = crate::source_guard::production_source(
            "orchestrator.rs",
            include_str!("orchestrator.rs"),
        );
        let lock = src
            .find("prove_lock::take(")
            .expect("stage 5 takes the lock");
        let after_lock = &src[lock..];
        let reread = after_lock
            .find("wait_for_coverage(")
            .expect("the bridge state is read again after the lock");
        let prove = after_lock.find("run_once_with_state(").expect("the proof");
        assert!(reread < prove, "the fresh read comes before the proof");
        assert!(
            after_lock[..reread].contains(".waited()"),
            "the re-read is for a run that waited"
        );
    }
}
