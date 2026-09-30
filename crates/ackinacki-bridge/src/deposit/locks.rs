//! The three deposit locks, all `flock(2)`: the kernel drops them when
//! the process dies, so a crash never leaves a stale lock.
//!
//! * `$state_dir/deposit.lock` — held from the unresolved-operation check
//!   through the confirmed deposit; one run at a time may be between "asked the
//!   wallet" and "the deposit is on chain".
//! * `$state_dir/<op-id>.lock` — held by whichever process drives that
//!   operation, including `--resume` and `--abandon`.
//! * `<prover-dir>/data/.prove.lock` — the prover's proving-key cache is not
//!   safe for concurrent use, so proofs on one prover directory run one at a
//!   time.
//!
//! A filesystem that cannot lock is a refusal here. `withdraw` carries on
//! without the lock on such a filesystem; a deposit cannot, because its
//! guarantees rest on the directory lock.
//!
//! Waiting for a held lock polls a non-blocking `flock`. A blocking one
//! would sit on a `spawn_blocking` thread that cannot be cancelled, and the
//! runtime waits for such threads when `main` returns: a signal during the
//! wait would leave the CLI hanging until the other holder let go.

use std::{
    fs::File,
    os::unix::io::AsRawFd,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    deposit::prover_files::ProverDir,
    errors::{CliError, CliResult, ExitCode, Stage},
    idempotency::{classify_flock_error, FlockVerdict},
};

/// An open lock file holding its `flock`; dropping it releases the lock.
#[derive(Debug)]
pub struct LockFile {
    /// Kept open only for the lock it carries.
    _file: File,
    /// Where the lock file lives.
    pub path: PathBuf,
}

impl LockFile {
    /// For handing the lock to a subprocess (the prover keeps it while it
    /// runs).
    pub fn raw_fd(&self) -> std::os::unix::io::RawFd {
        self._file.as_raw_fd()
    }
}

/// Reads an errno from `flock`: `None` means the lock is merely held by
/// someone else, `Some` is a refusal with the reason worded for the operator.
pub fn refusal_for(errno: Option<i32>, path: &Path) -> Option<CliError> {
    match classify_flock_error(errno) {
        FlockVerdict::Contended => None,
        FlockVerdict::Unsupported => Some(CliError::Preflight {
            reason: format!(
                "{} is on a filesystem without file locks (flock); a deposit needs them to keep \
                 two runs from requesting the same deposit, and proofs from sharing the prover's \
                 key cache. Point --state-dir and --deposit-prover-dir at a local filesystem",
                path.display()
            ),
            source: None,
        }),
        FlockVerdict::Fatal => Some(CliError::Preflight {
            reason: format!(
                "cannot lock {}: {}",
                path.display(),
                std::io::Error::from_raw_os_error(errno.unwrap_or(0))
            ),
            source: None,
        }),
    }
}

/// A lock failure after the wallet was asked: the deposit may be on chain,
/// so it is not exit 2, and the operation is resumable.
pub fn after_send(e: CliError, exit: ExitCode, stage: Stage, op_id: &str) -> CliError {
    CliError::deposit(
        exit,
        stage,
        Some(op_id),
        format!("{e}; fix it and continue with --resume {op_id}"),
    )
}

/// Opens (creating if needed) a lock file without truncating it.
fn open(path: &Path) -> CliResult<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| CliError::Preflight {
            reason: format!("cannot open the lock {}: {e}", path.display()),
            source: None,
        })
}

/// How often a waiting run tries a held lock again.
pub const WAIT_POLL: Duration = Duration::from_millis(250);

/// `true` — taken; `false` — held by someone else.
fn try_lock(file: &File, path: &Path) -> CliResult<bool> {
    loop {
        // SAFETY: `file` outlives the call.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(true);
        }
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return match refusal_for(e.raw_os_error(), path) {
            None => Ok(false),
            Some(refusal) => Err(refusal),
        };
    }
}

/// One attempt: the lock if free, `None` if held.
fn take(path: &Path) -> CliResult<Option<LockFile>> {
    let file = open(path)?;
    Ok(try_lock(&file, path)?.then(|| LockFile {
        _file: file,
        path: path.to_path_buf(),
    }))
}

/// Polls until the lock is free. Cancelled by dropping the future.
async fn wait_for(path: PathBuf) -> CliResult<LockFile> {
    let file = open(&path)?;
    while !try_lock(&file, &path)? {
        tokio::time::sleep(WAIT_POLL).await;
    }
    Ok(LockFile {
        _file: file,
        path,
    })
}

/// `$state_dir/deposit.lock`: one run at a time between "asked the wallet"
/// and "the deposit is on chain".
#[derive(Debug)]
pub struct DirLock(pub LockFile);

impl DirLock {
    /// The lock file's path inside `state_dir`.
    pub fn path(state_dir: &Path) -> PathBuf {
        state_dir.join("deposit.lock")
    }

    /// Takes the lock if it is free.
    pub fn try_take(state_dir: &Path) -> CliResult<Option<DirLock>> {
        Ok(take(&Self::path(state_dir))?.map(DirLock))
    }

    /// Waits until the lock is free and takes it.
    pub async fn wait(state_dir: &Path) -> CliResult<DirLock> {
        wait_for(Self::path(state_dir)).await.map(DirLock)
    }
}

/// `$state_dir/<op-id>.lock`: held by whichever process drives that operation.
#[derive(Debug)]
pub struct OpLock(pub LockFile);

impl OpLock {
    /// Takes the operation's lock if it is free.
    pub fn try_take(state_dir: &Path, op: &str) -> CliResult<Option<OpLock>> {
        Ok(take(&state_dir.join(format!("{op}.lock")))?.map(OpLock))
    }
}

/// `<prover-dir>/data/.prove.lock`: proofs on one prover directory run one at a
/// time.
#[derive(Debug)]
pub struct ProverLock(pub LockFile);

impl ProverLock {
    /// Takes the prover directory's lock if it is free.
    pub fn try_take(dir: &ProverDir) -> CliResult<Option<ProverLock>> {
        Ok(take(&dir.lock_path())?.map(ProverLock))
    }

    /// Waits until the lock is free and takes it.
    pub async fn wait(dir: &ProverDir) -> CliResult<ProverLock> {
        wait_for(dir.lock_path()).await.map(ProverLock)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_taker_sees_the_lock_held() {
        let d = tempfile::tempdir().unwrap();
        let a = DirLock::try_take(d.path()).unwrap();
        assert!(a.is_some());
        assert!(DirLock::try_take(d.path()).unwrap().is_none());
        drop(a);
        assert!(
            DirLock::try_take(d.path()).unwrap().is_some(),
            "dropping releases"
        );
    }

    #[test]
    fn operation_locks_are_independent_of_each_other_and_of_the_directory() {
        let d = tempfile::tempdir().unwrap();
        let _dir = DirLock::try_take(d.path()).unwrap().unwrap();
        let a = OpLock::try_take(d.path(), "01J9ZQ4X7T8V5N6M3K2P1R0S9A").unwrap();
        let b = OpLock::try_take(d.path(), "01J9ZQ4X7T8V5N6M3K2P1R0S9B").unwrap();
        assert!(a.is_some() && b.is_some());
        assert!(OpLock::try_take(d.path(), "01J9ZQ4X7T8V5N6M3K2P1R0S9A")
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_filesystem_without_flock_is_a_refusal_not_a_free_lock() {
        // Unlike `withdraw`, a deposit cannot run without the lock: the
        // directory lock is what keeps two runs from both asking a wallet.
        for errno in [libc::ENOLCK, libc::EOPNOTSUPP, libc::ENOSYS] {
            let e = refusal_for(Some(errno), Path::new("/x/deposit.lock")).expect("a refusal");
            assert_eq!(e.exit_code(), crate::errors::ExitCode::PreflightRefused);
            assert!(e.to_string().contains("file locks"), "{e}");
        }
        assert!(refusal_for(Some(libc::EWOULDBLOCK), Path::new("/x")).is_none());
        assert!(refusal_for(Some(libc::EACCES), Path::new("/x")).is_some());
    }

    #[test]
    fn a_lock_failure_after_the_wallet_is_not_exit_2() {
        let e = refusal_for(Some(libc::ENOLCK), Path::new("/p/data/.prove.lock")).unwrap();
        assert!(
            !e.to_string().contains("nothing was sent"),
            "the caller knows whether anything was sent"
        );
        let e = after_send(
            e,
            crate::errors::ExitCode::DepositProofFailed,
            crate::errors::Stage::Prove,
            "OP",
        );
        assert_eq!(e.exit_code(), crate::errors::ExitCode::DepositProofFailed);
        assert!(e.to_string().contains("--resume OP"));
    }

    #[tokio::test]
    async fn waiting_returns_once_the_holder_lets_go() {
        let d = tempfile::tempdir().unwrap();
        let held = DirLock::try_take(d.path()).unwrap().unwrap();
        let path = d.path().to_path_buf();
        let waiter = tokio::spawn(async move { DirLock::wait(&path).await });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!waiter.is_finished());
        drop(held);
        waiter.await.unwrap().unwrap();
    }

    #[test]
    fn a_cancelled_wait_leaves_nothing_for_the_runtime_to_wait_on() {
        // A signal drops the waiting future and `main` returns, dropping the
        // runtime. A blocking flock on a pool thread would keep that drop
        // waiting until the other holder lets go.
        let d = tempfile::tempdir().unwrap();
        let _held = DirLock::try_take(d.path()).unwrap().unwrap();
        let path = d.path().to_path_buf();
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            let waited = rt.block_on(async {
                tokio::time::timeout(std::time::Duration::from_millis(300), DirLock::wait(&path))
                    .await
            });
            assert!(waited.is_err(), "the lock is held");
            drop(rt);
            done.send(()).unwrap();
        });
        finished
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the runtime must shut down while the lock is still held elsewhere");
    }
}
