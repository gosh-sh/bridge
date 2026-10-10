//! Step 7: the proof, built by the deposit prover's two tools running in
//! the prover directory, one proof at a time per directory.
//!
//! The prover directory's lock travels with the prover. Its descriptor
//! stays open across `exec` in both tools, so the kernel keeps the `flock`
//! for as long as a tool lives — even when this process is killed with
//! SIGKILL and runs no destructor, an orphaned prover still holds it and
//! no second prover starts on its proving-key cache. On every other way
//! out, `kill_on_drop` stops the tool. No other deposit lock reaches the
//! tools: Rust opens every file close-on-exec, and only the prover lock is
//! cleared of it, in the child alone.

use std::{
    os::unix::io::RawFd,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use alloy_primitives::{Address, B256};
use tokio::{process::Command, time::Instant};

use crate::{
    deposit::{
        limits::{
            ShapeViolation, MAX_LOG_DATA_BYTE_LEN, MAX_RECEIPT_LOGS, PROVER_DEGREE,
            PROVER_UNPROVABLE_EXIT,
        },
        locks::{after_send, ProverLock},
        pi::{verify, DepositPublicInputs, ExpectedInputs},
        prover_files::{ProverDir, FETCH_BIN, PROVE_BIN},
        retry::deadline_after,
        ui::Ui,
    },
    errors::{CliError, CliResult, ExitCode, Stage},
};

/// The witness the fetcher writes and the prover reads, in the work
/// directory.
const INPUT_FILE: &str = "input.json";
/// The proof, in the work directory.
const PROOF_FILE: &str = "proof.bin";
/// The proof's public inputs, in the work directory.
const PUBIN_FILE: &str = "public_inputs.bin";
/// What the prover writes to, next to the final names: they get those
/// names only once it has exited successfully. The exporter writes its
/// files before it checks the proof itself, and leaves them when that
/// check fails.
const PARTIAL: &str = ".partial";
/// How many lines of a failed tool's stderr the error carries.
const STDERR_TAIL_LINES: usize = 20;
/// How many times a start refused with ETXTBSY is tried in all.
const SPAWN_TRIES: u32 = 3;
/// The pause between those tries.
const SPAWN_RETRY_PAUSE: Duration = Duration::from_millis(100);

/// The deposit to prove and where to put the proof.
#[derive(Debug, Clone)]
pub struct ProveRequest {
    /// The EVM RPC endpoint. It reaches the fetcher only in its
    /// environment: provider URLs often carry an API key, and a command
    /// line is visible in the process list.
    pub rpc_url: String,
    /// The deposit transaction.
    pub tx_hash: B256,
    /// The EVM bridge that emitted the `Deposit` log.
    pub bridge: Address,
    /// The `Deposit` log's index within the transaction's receipt.
    pub receipt_log_index: u64,
    /// The dapp id, 64 hex digits.
    pub dapp_hex: String,
    /// The EVM chain id.
    pub chain_id: u64,
    /// Where the witness, the proof and its public inputs are written.
    pub work: PathBuf,
}

/// A proof and its public inputs, as the prover wrote them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProofFiles {
    /// The proof bytes.
    pub proof: Vec<u8>,
    /// The twelve public inputs, 32 bytes each.
    pub public_inputs: Vec<u8>,
}

/// Exit 32: only the proof is missing, and the operation can be resumed.
fn failed(op_id: &str, why: impl std::fmt::Display) -> CliError {
    failed_with_output(op_id, why, "")
}

/// [`failed`], with a tool's last lines of stderr after the hint.
fn failed_with_output(op_id: &str, why: impl std::fmt::Display, output: &str) -> CliError {
    let mut reason =
        format!("{why}. The deposit is on the EVM bridge; retry with --resume {op_id}");
    if !output.is_empty() {
        reason.push_str("\nlast lines of its stderr:\n");
        reason.push_str(output);
    }
    CliError::deposit(
        ExitCode::DepositProofFailed,
        Stage::Prove,
        Some(op_id),
        reason,
    )
}

/// The proof an earlier run left in `work`, if both files are there. They
/// are there only if the prover finished.
pub fn load(work: &Path) -> Option<ProofFiles> {
    Some(ProofFiles {
        proof: std::fs::read(work.join(PROOF_FILE)).ok()?,
        public_inputs: std::fs::read(work.join(PUBIN_FILE)).ok()?,
    })
}

/// `work/<name>` with the suffix the prover's output carries until it is
/// done.
fn partial(work: &Path, name: &str) -> PathBuf {
    work.join(format!("{name}{PARTIAL}"))
}

/// Removes `path`; one that is not there is fine.
fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// The last `n` lines of a tool's output, decoded lossily.
fn last_lines(bytes: &[u8], n: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Leaves `fd` open across `exec`. Runs in the child between `fork` and
/// `exec`, so it clears the flag in the child's descriptor table only.
fn keep_open_across_exec(fd: RawFd) -> std::io::Result<()> {
    // SAFETY: fcntl(2) on a descriptor number is async-signal-safe and
    // touches no memory of ours.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    // SAFETY: as above.
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Starts `cmd`, trying again on ETXTBSY and on nothing else.
///
/// Linux refuses to exec a file some process has open for writing. Apart
/// from a tool that is still being copied into place, a fork elsewhere in
/// this process can hold such a descriptor between its `fork` and `exec`;
/// the refusal then says nothing about the tool.
async fn spawn(cmd: &mut Command) -> std::io::Result<tokio::process::Child> {
    let mut tries = 1;
    loop {
        match cmd.spawn() {
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && tries < SPAWN_TRIES => {
                tries += 1;
                tokio::time::sleep(SPAWN_RETRY_PAUSE).await;
            },
            started => return started,
        }
    }
}

/// How long each prover tool gets to answer `--help` in a preflight.
pub const TOOL_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// How often a probe looks whether its tool has exited.
const PROBE_POLL: Duration = Duration::from_millis(10);

/// The environment a probe passes on: the search paths a tool needs to
/// start at all, nothing else of this process's.
const PROBE_ENV: [&str; 2] = ["PATH", "LD_LIBRARY_PATH"];

/// Runs each prover tool once with `--help` in the prover directory, as
/// the installer does: an executable file is not yet a tool that runs. A
/// binary the dynamic loader refuses — a newer glibc than this host's, a
/// loader that is not here — or a script with a bad interpreter line is
/// refused now, before the deposit, not at step 7 with the USDC in the
/// bridge. `--help` needs no network, and a tool gets no secret: only
/// [`PROBE_ENV`]. Each tool has `timeout` and is killed past it.
///
/// The probe waits by the wall clock on the blocking pool; bounded by
/// `timeout` per tool, it can delay the end of an interrupted run by that
/// much at most. The error names the tool and quotes the end of its
/// stderr.
pub async fn probe_tools(dir: &ProverDir, timeout: Duration) -> Result<(), String> {
    let root = dir.root.clone();
    tokio::task::spawn_blocking(move || {
        for tool in [FETCH_BIN, PROVE_BIN] {
            probe(&root.join(tool), &root, timeout)?;
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("the prover tools could not be tried: {e}"))?
}

/// Runs `tool --help` in `root` for at most `timeout`: `Ok` when it exits 0.
fn probe(tool: &Path, root: &Path, timeout: Duration) -> Result<(), String> {
    let mut cmd = std::process::Command::new(tool);
    cmd.arg("--help")
        .current_dir(root)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for key in PROBE_ENV {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
        }
    }
    let mut child = start_retrying(|| cmd.spawn()).map_err(|e| {
        // An executable file that exec "does not find" is a script whose
        // interpreter, or a binary whose dynamic loader, is not here.
        let why = if e.kind() == std::io::ErrorKind::NotFound && tool.is_file() {
            format!("{e}: its interpreter or dynamic loader is not on this host")
        } else {
            e.to_string()
        };
        format!("{} cannot be started on this host ({why})", tool.display())
    })?;
    // Read on a thread of its own: a tool that fills the pipe must not stall.
    let stderr = child.stderr.take().and_then(|mut out| {
        let (sent, got) = std::sync::mpsc::channel();
        let reader = std::thread::Builder::new()
            .name("prover-probe".into())
            .spawn(move || {
                let mut buf = Vec::new();
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "what could be read of it is what is quoted"
                )]
                let _ = std::io::Read::read_to_end(&mut out, &mut buf);
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "a probe that gave up no longer listens"
                )]
                let _ = sent.send(buf);
            });
        reader.ok().map(|_| got)
    });
    let end = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < end => std::thread::sleep(PROBE_POLL),
            waited => {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "a tool that exited meanwhile needs no kill"
                )]
                let _ = child.kill();
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "only reaps it; the refusal is already decided"
                )]
                let _ = child.wait();
                return Err(match waited {
                    Err(e) => format!("{}: {e}", tool.display()),
                    _ => format!(
                        "{} --help did not answer within {} s and was stopped",
                        tool.display(),
                        timeout.as_secs_f32()
                    ),
                });
            },
        }
    };
    if status.success() {
        return Ok(());
    }
    let out = stderr
        .and_then(|got| {
            got.recv_timeout(end.saturating_duration_since(std::time::Instant::now()))
                .ok()
        })
        .unwrap_or_default();
    Err(format!(
        "{} --help failed ({status}): it does not run on this host\nlast lines of its stderr:\n{}",
        tool.display(),
        last_lines(&out, STDERR_TAIL_LINES)
    ))
}

/// Starts a tool with `start`, trying again on ETXTBSY and on nothing
/// else, as [`spawn`] does, for a caller that may block.
fn start_retrying<T>(mut start: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut tries = 1;
    loop {
        match start() {
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && tries < SPAWN_TRIES => {
                tries += 1;
                std::thread::sleep(SPAWN_RETRY_PAUSE);
            },
            started => return started,
        }
    }
}

/// How a tool that ran to its end ended.
#[derive(Debug, PartialEq, Eq)]
enum Ran {
    /// It exited 0.
    Done,
    /// It exited [`PROVER_UNPROVABLE_EXIT`]: the deposit cannot be proven,
    /// for the reason it gave.
    Unprovable(String),
}

/// The reason a tool gave on its way out with [`PROVER_UNPROVABLE_EXIT`]:
/// its `unprovable: ` line, else its last line of stderr.
fn unprovable_reason(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    text.lines()
        .rev()
        .find_map(|l| l.strip_prefix("unprovable: "))
        .or_else(|| text.lines().next_back())
        .unwrap_or("no reason given")
        .trim()
        .to_string()
}

/// Runs one tool to completion by `deadline`, handing it the prover lock
/// `lock_fd`. `limit` is the whole attempt's budget, for the message.
async fn run(
    cmd: &mut Command,
    what: &str,
    lock_fd: RawFd,
    deadline: Instant,
    limit: Duration,
    op_id: &str,
) -> CliResult<Ran> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // SAFETY: the closure makes only async-signal-safe calls (fcntl) and
    // allocates nothing.
    unsafe {
        cmd.pre_exec(move || keep_open_across_exec(lock_fd));
    }
    let child = spawn(cmd)
        .await
        .map_err(|e| failed(op_id, format!("cannot start {what}: {e}")))?;
    // Dropping `child` on the timeout kills it (`kill_on_drop`).
    let out = match tokio::time::timeout_at(deadline, child.wait_with_output()).await {
        Ok(r) => r.map_err(|e| failed(op_id, format!("{what}: {e}")))?,
        Err(_) => {
            return Err(failed(
                op_id,
                format!(
                    "{what} timed out and was stopped: the proof did not finish within {} s \
                     (--prover-timeout-s)",
                    limit.as_secs()
                ),
            ))
        },
    };
    if out.status.code() == Some(PROVER_UNPROVABLE_EXIT) {
        return Ok(Ran::Unprovable(unprovable_reason(&out.stderr)));
    }
    if !out.status.success() {
        return Err(failed_with_output(
            op_id,
            format!("{what} failed ({})", out.status),
            &last_lines(&out.stderr, STDERR_TAIL_LINES),
        ));
    }
    Ok(Ran::Done)
}

/// Step 7: builds the proof of the deposit in `req` in the prover
/// directory and checks its public inputs against `want`.
///
/// The files an earlier proof left go first, and the prover writes under
/// other names, renamed to `proof.bin` and `public_inputs.bin` only after
/// it exits successfully: a pair under the final names is always one the
/// prover finished, and nothing a crash or a failed check left behind is
/// taken for a proof later.
///
/// The prover lock is taken before the fetcher starts and held until the
/// prover exits; a held lock is waited for, and the status says why.
/// `timeout` bounds both tools together, from the moment the lock is
/// taken. Every failure is exit 32, and a resume tries again. A tool that
/// finds the deposit unprovable is not a failure: that answer is final,
/// and it comes back as the inner `Err`. The empty path is a resume given
/// no prover directory, and the refusal names the flag.
pub async fn prove(
    dir: &ProverDir,
    req: &ProveRequest,
    want: &ExpectedInputs,
    timeout: Duration,
    ui: &dyn Ui,
    op_id: &str,
) -> CliResult<Result<ProofFiles, ShapeViolation>> {
    if dir.root.as_os_str().is_empty() {
        return Err(failed(
            op_id,
            "the proof has to be built again, and no --deposit-prover-dir \
             (BRIDGE_DEPOSIT_PROVER_DIR) was given",
        ));
    }
    // Both tools run with the prover directory as their working directory
    // (the prover's own paths are relative to it), so no path handed to
    // them may depend on ours.
    let root = std::path::absolute(&dir.root)
        .map_err(|e| failed(op_id, format!("{}: {e}", dir.root.display())))?;
    let work = std::path::absolute(&req.work)
        .map_err(|e| failed(op_id, format!("{}: {e}", req.work.display())))?;
    std::fs::create_dir_all(&work)
        .map_err(|e| failed(op_id, format!("cannot create {}: {e}", work.display())))?;
    for name in [PROOF_FILE, PUBIN_FILE] {
        for path in [work.join(name), partial(&work, name)] {
            remove(&path).map_err(|e| {
                failed(
                    op_id,
                    format!("cannot remove the earlier {}: {e}", path.display()),
                )
            })?;
        }
    }
    let dir = ProverDir {
        root,
    };
    // Preflight already proved the lock works here; a failure now comes
    // after the deposit, so it is exit 32 and not a refusal.
    let lock_failed = |e| after_send(e, ExitCode::DepositProofFailed, Stage::Prove, op_id);
    let lock = match ProverLock::try_take(&dir).map_err(lock_failed)? {
        Some(l) => l,
        None => {
            ui.status("the prover is busy with another deposit; waiting for it");
            ProverLock::wait(&dir).await.map_err(lock_failed)?
        },
    };
    let lock_fd = lock.0.raw_fd();
    let deadline = deadline_after(timeout);
    let input = work.join(INPUT_FILE);
    ui.status("fetching the deposit's receipt and transaction proofs");
    let fetched = run(
        Command::new(dir.bin(FETCH_BIN))
            .current_dir(&dir.root)
            .env("ETH_RPC_URL", &req.rpc_url)
            .args(["--tx-hash", &format!("{:#x}", req.tx_hash)])
            .args(["--contract", &format!("{:#x}", req.bridge)])
            .args(["--log-index", &req.receipt_log_index.to_string()])
            .args(["--dapp-id", &req.dapp_hex])
            .arg("--output")
            .arg(&input),
        FETCH_BIN,
        lock_fd,
        deadline,
        timeout,
        op_id,
    )
    .await?;
    if let Ran::Unprovable(reason) = fetched {
        return Ok(Err(ShapeViolation::ProverRefused {
            reason,
        }));
    }
    ui.status("building the proof (minutes; the first run also builds the proving key)");
    let proved = run(
        Command::new(dir.bin(PROVE_BIN))
            .current_dir(&dir.root)
            .args(["--chain-id", &req.chain_id.to_string()])
            .arg("--input")
            .arg(&input)
            .arg("--proof-out")
            .arg(partial(&work, PROOF_FILE))
            .arg("--pubin-out")
            .arg(partial(&work, PUBIN_FILE))
            .args(["--degree", &PROVER_DEGREE.to_string()])
            .args(["--max-data-byte-len", &MAX_LOG_DATA_BYTE_LEN.to_string()])
            .args(["--max-log-num", &MAX_RECEIPT_LOGS.to_string()]),
        PROVE_BIN,
        lock_fd,
        deadline,
        timeout,
        op_id,
    )
    .await?;
    drop(lock);
    if let Ran::Unprovable(reason) = proved {
        return Ok(Err(ShapeViolation::ProverRefused {
            reason,
        }));
    }
    // The proof first: a crash between the two leaves no pair to load.
    for name in [PROOF_FILE, PUBIN_FILE] {
        let from = partial(&work, name);
        if from.exists() {
            std::fs::rename(&from, work.join(name)).map_err(|e| {
                failed(
                    op_id,
                    format!("cannot rename {} into place: {e}", from.display()),
                )
            })?;
        }
    }
    let files = load(&work).ok_or_else(|| {
        failed(
            op_id,
            format!("{PROVE_BIN} left no proof in {}", work.display()),
        )
    })?;
    let pi = DepositPublicInputs::decode(&files.public_inputs).map_err(|e| {
        failed(
            op_id,
            format!("{PROVE_BIN} wrote unusable public inputs: {e}"),
        )
    })?;
    verify(&pi, want)
        .map_err(|m| failed(op_id, format!("the proof does not match the deposit: {m}")))?;
    Ok(Ok(files))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use alloy_primitives::{address, U256};

    use super::*;
    use crate::deposit::{
        locks::{DirLock, OpLock},
        pi::tests_support::{expected_from_witness, PI},
        testkit::fake_prover_dir,
        ui::RecordingUi,
    };

    /// The `proof_00` fixture's deposit, which the fake prover proves.
    fn want() -> ExpectedInputs {
        expected_from_witness()
    }

    fn req(work: &Path) -> ProveRequest {
        ProveRequest {
            rpc_url: "https://rpc.example/key".into(),
            tx_hash: B256::repeat_byte(1),
            bridge: address!("cdfd6cef70f68d0849310cd970f8ef8f8e4b4fdb"),
            receipt_log_index: 1,
            dapp_hex: "00".repeat(32),
            chain_id: 11_155_111,
            work: work.to_path_buf(),
        }
    }

    /// Polls `f` for up to three seconds. A killed child lets go of its
    /// descriptors only once the kernel has delivered the SIGKILL.
    async fn eventually(mut f: impl FnMut() -> bool) -> bool {
        for _ in 0..150 {
            if f() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    /// Puts a wrapper in front of the tool `name` that records its
    /// arguments (one per line) in `data/<name>.argv` and its
    /// `ETH_RPC_URL` in `data/<name>.env`, then runs the tool.
    fn record_calls(dir: &ProverDir, name: &str) {
        let real = dir.root.join(format!("{name}.real"));
        std::fs::rename(dir.bin(name), &real).unwrap();
        let body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > data/{name}.argv\nprintf '%s' \"$ETH_RPC_URL\" > \
             data/{name}.env\nexec \"$0.real\" \"$@\"\n"
        );
        std::fs::write(dir.bin(name), body).unwrap();
        std::fs::set_permissions(dir.bin(name), std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[tokio::test]
    async fn a_proof_whose_inputs_match_the_deposit_is_returned() {
        let (_d, dir) = fake_prover_dir("");
        let w = tempfile::tempdir().unwrap();
        let ui = RecordingUi::new(true);
        let p = prove(
            &dir,
            &req(w.path()),
            &want(),
            Duration::from_secs(10),
            &ui,
            "OP",
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(p.public_inputs, PI);
        assert_eq!(load(w.path()).unwrap(), p);
        assert_eq!(load(w.path()).unwrap().proof, b"proof");
        let left: Vec<_> = std::fs::read_dir(w.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.ends_with(".partial"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[tokio::test]
    async fn a_proof_the_prover_did_not_finish_is_not_left_to_reuse() {
        let (_d, dir) = fake_prover_dir("");
        crate::deposit::testkit::prover_fails_after_writing(&dir);
        let w = tempfile::tempdir().unwrap();
        let e = prove(
            &dir,
            &req(w.path()),
            &want(),
            Duration::from_secs(10),
            &RecordingUi::new(true),
            "OP",
        )
        .await
        .unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::DepositProofFailed);
        assert!(e.to_string().contains("does not verify"), "{e}");
        assert_eq!(load(w.path()), None, "a proof the prover rejected");
    }

    #[tokio::test]
    async fn a_new_proof_starts_without_the_files_an_earlier_one_left() {
        // Dies before it writes anything, as a killed prover may.
        let (_d, dir) = fake_prover_dir("exit 1");
        let w = tempfile::tempdir().unwrap();
        std::fs::write(w.path().join(PROOF_FILE), b"old proof").unwrap();
        std::fs::write(w.path().join(PUBIN_FILE), PI).unwrap();
        prove(
            &dir,
            &req(w.path()),
            &want(),
            Duration::from_secs(10),
            &RecordingUi::new(true),
            "OP",
        )
        .await
        .unwrap_err();
        assert_eq!(load(w.path()), None, "the old pair is gone");
    }

    #[tokio::test]
    async fn the_probe_passes_tools_that_answer_help_and_runs_no_proof() {
        let (_d, dir) = fake_prover_dir("");
        probe_tools(&dir, TOOL_PROBE_TIMEOUT).await.unwrap();
        assert!(!dir.data_dir().join("runs.log").exists(), "a proof ran");
    }

    #[tokio::test]
    async fn a_tool_that_hangs_on_help_is_refused_within_the_timeout() {
        let (_d, dir) = fake_prover_dir("");
        std::fs::write(dir.bin(PROVE_BIN), "#!/bin/sh\nexec sleep 30\n").unwrap();
        let t0 = std::time::Instant::now();
        let e = probe_tools(&dir, Duration::from_millis(500))
            .await
            .unwrap_err();
        assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
        assert!(e.contains(PROVE_BIN) && e.contains("did not answer"), "{e}");
    }

    #[tokio::test]
    async fn a_tool_gets_no_secret_from_the_environment() {
        // `--help` needs nothing but the search paths.
        let (_d, dir) = fake_prover_dir("");
        std::fs::write(
            dir.bin(FETCH_BIN),
            "#!/bin/sh\n[ -z \"$HOME$RPC_URL$ETH_RPC_URL\" ] || { env >&2; exit 9; }\n",
        )
        .unwrap();
        probe_tools(&dir, TOOL_PROBE_TIMEOUT)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn nothing_on_disk_loads_as_nothing() {
        let w = tempfile::tempdir().unwrap();
        assert_eq!(load(w.path()), None);
        std::fs::write(w.path().join("proof.bin"), b"proof").unwrap();
        assert_eq!(load(w.path()), None, "a proof without its public inputs");
    }

    #[tokio::test]
    async fn the_tools_get_the_deposit_and_the_fixed_bounds_and_the_rpc_url_only_in_the_environment(
    ) {
        let (_d, dir) = fake_prover_dir("");
        record_calls(&dir, FETCH_BIN);
        record_calls(&dir, PROVE_BIN);
        let w = tempfile::tempdir().unwrap();
        let r = req(w.path());
        prove(
            &dir,
            &r,
            &want(),
            Duration::from_secs(10),
            &RecordingUi::new(true),
            "OP",
        )
        .await
        .unwrap()
        .unwrap();
        let read = |f: String| std::fs::read_to_string(dir.data_dir().join(f)).unwrap();
        let at = |f: &str| w.path().join(f).to_string_lossy().into_owned();
        let fetch = read(format!("{FETCH_BIN}.argv"));
        assert_eq!(fetch.lines().collect::<Vec<_>>(), [
            "--tx-hash",
            &format!("0x{}", "01".repeat(32)),
            "--contract",
            "0xcdfd6cef70f68d0849310cd970f8ef8f8e4b4fdb",
            "--log-index",
            "1",
            "--dapp-id",
            &"00".repeat(32),
            "--output",
            &at("input.json"),
        ]);
        assert!(!fetch.contains("rpc.example"), "{fetch}");
        assert_eq!(read(format!("{FETCH_BIN}.env")), r.rpc_url);
        let proof = read(format!("{PROVE_BIN}.argv"));
        assert_eq!(proof.lines().collect::<Vec<_>>(), [
            "--chain-id",
            "11155111",
            "--input",
            &at("input.json"),
            "--proof-out",
            &at("proof.bin.partial"),
            "--pubin-out",
            &at("public_inputs.bin.partial"),
            "--degree",
            "18",
            "--max-data-byte-len",
            "2048",
            "--max-log-num",
            "20",
        ]);
    }

    #[tokio::test]
    async fn inputs_for_another_deposit_are_exit_32_naming_the_field() {
        let (_d, dir) = fake_prover_dir("");
        let w = tempfile::tempdir().unwrap();
        let mut other = want();
        other.deposit_id = U256::from(1);
        let e = prove(
            &dir,
            &req(w.path()),
            &other,
            Duration::from_secs(10),
            &RecordingUi::new(true),
            "OP",
        )
        .await
        .unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::DepositProofFailed);
        assert!(e.to_string().contains("depositId"), "{e}");
        assert!(e.to_string().contains("--resume OP"), "{e}");
    }

    #[tokio::test]
    async fn a_failing_prover_is_exit_32_with_its_stderr() {
        let (_d, dir) = fake_prover_dir("echo 'keygen: out of memory' >&2; exit 1");
        let w = tempfile::tempdir().unwrap();
        let e = prove(
            &dir,
            &req(w.path()),
            &want(),
            Duration::from_secs(10),
            &RecordingUi::new(true),
            "OP",
        )
        .await
        .unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::DepositProofFailed);
        assert!(e.to_string().contains("out of memory"), "{e}");
        assert!(e.to_string().contains("--resume OP"), "{e}");
    }

    #[tokio::test]
    async fn a_deposit_the_prover_finds_unprovable_is_not_a_failure_to_resume() {
        let (_d, dir) = fake_prover_dir(
            "echo 'loading input' >&2; echo 'unprovable: the receipt has 9 logs; the circuit \
             parses at most 1' >&2; exit 3",
        );
        let w = tempfile::tempdir().unwrap();
        let got = prove(
            &dir,
            &req(w.path()),
            &want(),
            Duration::from_secs(10),
            &RecordingUi::new(true),
            "OP",
        )
        .await
        .unwrap();
        assert_eq!(
            got,
            Err(ShapeViolation::ProverRefused {
                reason: "the receipt has 9 logs; the circuit parses at most 1".into()
            })
        );
        assert_eq!(load(w.path()), None);
    }

    #[test]
    fn an_unprovable_exit_without_its_line_gives_the_last_line() {
        assert_eq!(unprovable_reason(b"a\nb\n"), "b");
        assert_eq!(unprovable_reason(b""), "no reason given");
    }

    #[tokio::test]
    async fn only_the_last_20_lines_of_stderr_are_kept() {
        let (_d, dir) = fake_prover_dir(
            "i=1; while [ $i -le 30 ]; do echo \"line $i\" >&2; i=$((i+1)); done; exit 1",
        );
        let w = tempfile::tempdir().unwrap();
        let e = prove(
            &dir,
            &req(w.path()),
            &want(),
            Duration::from_secs(10),
            &RecordingUi::new(true),
            "OP",
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(e.contains("line 11\n") && e.contains("line 30"), "{e}");
        assert!(!e.contains("line 10\n"), "{e}");
    }

    #[tokio::test]
    async fn a_failing_fetch_is_exit_32_and_the_prover_does_not_run() {
        // The fake fetcher exits 7 without ETH_RPC_URL: an empty URL is
        // the same as none.
        let (_d, dir) = fake_prover_dir("");
        let w = tempfile::tempdir().unwrap();
        let mut r = req(w.path());
        r.rpc_url = String::new();
        let e = prove(
            &dir,
            &r,
            &want(),
            Duration::from_secs(10),
            &RecordingUi::new(true),
            "OP",
        )
        .await
        .unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::DepositProofFailed);
        assert!(e.to_string().contains(FETCH_BIN), "{e}");
        assert!(!dir.data_dir().join("runs.log").exists(), "the prover ran");
    }

    #[tokio::test]
    async fn a_prover_past_its_timeout_is_killed_and_releases_the_lock() {
        let (_d, dir) = fake_prover_dir("exec sleep 30");
        let w = tempfile::tempdir().unwrap();
        let e = prove(
            &dir,
            &req(w.path()),
            &want(),
            Duration::from_millis(500),
            &RecordingUi::new(true),
            "OP",
        )
        .await
        .unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::DepositProofFailed);
        assert!(e.to_string().contains("timed out"), "{e}");
        assert!(eventually(|| ProverLock::try_take(&dir).unwrap().is_some()).await);
    }

    #[tokio::test]
    async fn the_prover_inherits_the_prover_lock_and_no_other_deposit_lock() {
        let state = tempfile::tempdir().unwrap();
        let _dir_lock = DirLock::try_take(state.path()).unwrap().unwrap();
        let _op_lock = OpLock::try_take(state.path(), "OP").unwrap().unwrap();
        let (_d, dir) = fake_prover_dir("ls -l /proc/$$/fd > data/fds");
        let w = tempfile::tempdir().unwrap();
        prove(
            &dir,
            &req(w.path()),
            &want(),
            Duration::from_secs(10),
            &RecordingUi::new(true),
            "OP",
        )
        .await
        .unwrap()
        .unwrap();
        let fds = std::fs::read_to_string(dir.data_dir().join("fds")).unwrap();
        let lock = std::fs::canonicalize(dir.lock_path()).unwrap();
        assert!(fds.contains(&*lock.to_string_lossy()), "{fds}");
        let state = std::fs::canonicalize(state.path()).unwrap();
        assert!(!fds.contains(&*state.to_string_lossy()), "{fds}");
    }

    /// The same path, relative to the test's working directory.
    fn relative(p: &Path) -> std::path::PathBuf {
        let cwd = std::env::current_dir().unwrap();
        let mut r = std::path::PathBuf::new();
        for _ in 1..cwd.components().count() {
            r.push("..");
        }
        r.join(p.strip_prefix("/").unwrap())
    }

    #[tokio::test]
    async fn relative_prover_and_work_directories_still_work() {
        // The shipped profile says BRIDGE_DEPOSIT_PROVER_DIR=./deposit-prover.
        let (_d, dir) = fake_prover_dir("");
        let w = tempfile::tempdir().unwrap();
        let rel = ProverDir {
            root: relative(&dir.root),
        };
        assert!(rel.root.is_relative());
        let r = req(&relative(w.path()));
        prove(
            &rel,
            &r,
            &want(),
            Duration::from_secs(10),
            &RecordingUi::new(true),
            "OP",
        )
        .await
        .unwrap()
        .unwrap();
        assert!(w.path().join("proof.bin").exists());
    }

    #[tokio::test]
    async fn a_busy_prover_is_waited_for_and_said_so() {
        let (_d, dir) = fake_prover_dir("");
        let held = ProverLock::try_take(&dir).unwrap().unwrap();
        let w = tempfile::tempdir().unwrap();
        let ui = std::sync::Arc::new(RecordingUi::new(true));
        let (dir2, ui2, r) = (dir.clone(), ui.clone(), req(w.path()));
        let task = tokio::spawn(async move {
            prove(
                &dir2,
                &r,
                &want(),
                Duration::from_secs(10),
                ui2.as_ref(),
                "OP",
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(ui.statuses().iter().any(|s| s.contains("busy")));
        assert!(!task.is_finished());
        drop(held);
        task.await.unwrap().unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_tool_still_open_for_writing_is_started_once_it_is_closed() {
        // Linux refuses to exec a file some process has open for writing
        // (ETXTBSY). A fork elsewhere in this process can hold such a
        // descriptor for a moment; the start is tried again.
        let (_d, dir) = fake_prover_dir("");
        let writer = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.bin(FETCH_BIN))
            .unwrap();
        let refused = std::process::Command::new(dir.bin(FETCH_BIN))
            .status()
            .expect_err("a tool open for writing does not start");
        assert_eq!(refused.raw_os_error(), Some(libc::ETXTBSY), "{refused}");
        // This test's runtime runs one task at a time, so the writer is
        // closed at the first pause of `prove`: the one after its first
        // start was refused. The next start finds the tool closed, however
        // long this host takes to get there.
        let closer = tokio::spawn(async move { drop(writer) });
        let w = tempfile::tempdir().unwrap();
        prove(
            &dir,
            &req(w.path()),
            &want(),
            Duration::from_secs(10),
            &RecordingUi::new(true),
            "OP",
        )
        .await
        .unwrap()
        .unwrap();
        closer.await.unwrap();
    }

    /// A fake prover that says who it is and then runs until killed. It
    /// becomes `sleep` itself: a forked `sleep` would inherit the prover
    /// lock and outlive a killed `sh`, which the real prover, having no
    /// children, cannot do.
    const RUNS_UNTIL_KILLED: &str = "echo $$ > data/pid; exec sleep 30";

    /// Whether `pid` is a running process. A killed child stays a zombie
    /// until its parent reaps it, and `kill(pid, 0)` still finds a zombie.
    fn alive(pid: i32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            // The state letter follows the command name, which is in
            // parentheses.
            stat.rsplit_once(") ")
                .is_some_and(|(_, rest)| !rest.starts_with(['Z', 'X']))
        })
    }

    /// The pid the fake prover wrote to `data/pid` once it started.
    async fn prover_pid(dir: &ProverDir) -> i32 {
        let f = dir.data_dir().join("pid");
        for _ in 0..500 {
            if let Some(pid) = std::fs::read_to_string(&f)
                .ok()
                .and_then(|s| s.trim().parse().ok())
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the fake prover never started");
    }

    #[tokio::test]
    async fn an_interrupted_run_leaves_no_prover_and_no_lock() {
        let (_d, dir) = fake_prover_dir(RUNS_UNTIL_KILLED);
        let w = tempfile::tempdir().unwrap();
        let (dir2, r) = (dir.clone(), req(w.path()));
        let task = tokio::spawn(async move {
            prove(
                &dir2,
                &r,
                &want(),
                Duration::from_secs(60),
                &RecordingUi::new(true),
                "OP",
            )
            .await
        });
        let pid = prover_pid(&dir).await;
        task.abort(); // what a signal does to the run's future
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(
            eventually(|| !alive(pid)).await,
            "the prover outlived the CLI"
        );
        assert!(eventually(|| ProverLock::try_take(&dir).unwrap().is_some()).await);
    }

    #[tokio::test]
    async fn two_deposits_on_one_cold_prover_directory_run_one_after_another() {
        // The fake refuses to run concurrently and "generates the proving
        // key" only when it is missing, logging each generation.
        let extra = "set -C; : > data/.busy || exit 99; set +C\n[ -f data/deposit_prover_k18.x.pk \
                     ] || { echo gen >> data/keygen.log; : > data/deposit_prover_k18.x.pk; \
                     }\nsleep 0.3; rm -f data/.busy";
        let (_d, dir) = fake_prover_dir(extra);
        let (w1, w2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (d1, d2, r1, r2) = (dir.clone(), dir.clone(), req(w1.path()), req(w2.path()));
        let a = tokio::spawn(async move {
            prove(
                &d1,
                &r1,
                &want(),
                Duration::from_secs(10),
                &RecordingUi::new(true),
                "A",
            )
            .await
        });
        let b = tokio::spawn(async move {
            prove(
                &d2,
                &r2,
                &want(),
                Duration::from_secs(10),
                &RecordingUi::new(true),
                "B",
            )
            .await
        });
        a.await.unwrap().unwrap().unwrap();
        b.await.unwrap().unwrap().unwrap();
        let lines = |f: &str| {
            std::fs::read_to_string(dir.data_dir().join(f))
                .unwrap()
                .lines()
                .count()
        };
        assert_eq!(lines("keygen.log"), 1);
        assert_eq!(lines("runs.log"), 2);
    }

    /// Not a test on its own: the body of the "CLI" process that the
    /// process tests below start and signal. It proves the deposit of an
    /// `Anchored` operation under `until_signal` and exits the way `main`
    /// does, with the error's code. Does nothing unless ACKI_PROVE_HELPER
    /// is set, so the test process itself never installs a signal handler.
    #[tokio::test]
    async fn helper_prove_until_signalled() {
        use crate::deposit::{signals, store::OpStage};
        let Ok(root) = std::env::var("ACKI_PROVE_HELPER") else {
            return;
        };
        let dir = ProverDir {
            root: root.into(),
        };
        let state = dir.root.join("state");
        let op = signals::tests_support::record_at(&state, OpStage::Anchored);
        let r = req(&dir.root.join("work"));
        let (want, ui) = (want(), RecordingUi::new(true));
        let run = prove(&dir, &r, &want, Duration::from_secs(120), &ui, &op);
        let e = match signals::until_signal(run).await {
            Ok(Ok(_)) => return,
            Ok(Err(e)) => e,
            Err(sig) => match signals::interrupted(sig, &state, Some(op)) {
                Ok(_) => return,
                Err(e) => e,
            },
        };
        // The run is dropped by now and its prover killed. `main` returns
        // the code; a test can only exit with it.
        eprintln!("{e}");
        std::process::exit(e.exit_code().as_i32());
    }

    /// Starts the helper above as a separate "CLI" process on `dir`.
    fn start_helper(dir: &ProverDir) -> std::process::Child {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "deposit::prover::tests::helper_prove_until_signalled",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("ACKI_PROVE_HELPER", &dir.root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    /// Waits up to ten seconds for the helper to exit; its status and what
    /// it wrote to stderr.
    async fn exit_of(mut cli: std::process::Child) -> (std::process::ExitStatus, String) {
        for _ in 0..500 {
            if let Some(status) = cli.try_wait().unwrap() {
                let mut stderr = String::new();
                std::io::Read::read_to_string(&mut cli.stderr.take().unwrap(), &mut stderr)
                    .unwrap();
                return (status, stderr);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        cli.kill().unwrap();
        cli.wait().unwrap();
        panic!("the CLI did not exit");
    }

    /// Sends `sig` to the helper process.
    fn send(cli: &std::process::Child, sig: libc::c_int) {
        let pid = i32::try_from(cli.id()).unwrap();
        // SAFETY: plain kill(2) on our own child.
        assert_eq!(unsafe { libc::kill(pid, sig) }, 0);
    }

    #[tokio::test]
    async fn a_run_that_is_not_signalled_ends_on_its_own() {
        let (_d, dir) = fake_prover_dir("");
        let (status, stderr) = exit_of(start_helper(&dir)).await;
        assert!(status.success(), "{status}: {stderr}");
        assert!(dir.root.join("work").join(PROOF_FILE).exists());
    }

    #[tokio::test]
    async fn sigterm_to_the_cli_stops_the_prover_and_frees_the_lock() {
        let (_d, dir) = fake_prover_dir(RUNS_UNTIL_KILLED);
        let cli = start_helper(&dir);
        let pid = prover_pid(&dir).await;
        send(&cli, libc::SIGTERM);
        let (status, stderr) = exit_of(cli).await;
        assert_eq!(status.code(), Some(32), "{stderr}");
        assert!(
            stderr.contains("interrupted by SIGTERM at stage Anchored;"),
            "{stderr}"
        );
        assert!(stderr.contains("--resume"), "{stderr}");
        assert!(
            eventually(|| !alive(pid)).await,
            "the prover outlived the CLI it belonged to"
        );
        assert!(eventually(|| ProverLock::try_take(&dir).unwrap().is_some()).await);
    }

    #[tokio::test]
    async fn sigint_and_sighup_end_the_run_the_same_way() {
        for (sig, name) in [(libc::SIGINT, "SIGINT"), (libc::SIGHUP, "SIGHUP")] {
            let (_d, dir) = fake_prover_dir(RUNS_UNTIL_KILLED);
            let cli = start_helper(&dir);
            let pid = prover_pid(&dir).await;
            send(&cli, sig);
            let (status, stderr) = exit_of(cli).await;
            assert_eq!(status.code(), Some(32), "{name}: {stderr}");
            assert!(
                stderr.contains(&format!("interrupted by {name} at stage Anchored;")),
                "{stderr}"
            );
            assert!(
                eventually(|| !alive(pid)).await,
                "{name}: the prover outlived the CLI"
            );
            assert!(
                eventually(|| ProverLock::try_take(&dir).unwrap().is_some()).await,
                "{name}"
            );
        }
    }

    #[tokio::test]
    async fn sigkill_to_the_cli_leaves_the_lock_with_the_running_prover() {
        let (_d, dir) = fake_prover_dir(RUNS_UNTIL_KILLED);
        let mut cli = start_helper(&dir);
        let pid = prover_pid(&dir).await;
        cli.kill().unwrap(); // SIGKILL: no handler, no destructor
        cli.wait().unwrap();
        assert!(alive(pid), "an orphaned prover keeps running...");
        assert!(
            ProverLock::try_take(&dir).unwrap().is_none(),
            "...and keeps the lock, so no second prover starts on its key cache"
        );
        let w = tempfile::tempdir().unwrap();
        let ui = RecordingUi::new(true);
        let second = tokio::time::timeout(
            Duration::from_secs(1),
            prove(
                &dir,
                &req(w.path()),
                &want(),
                Duration::from_secs(10),
                &ui,
                "B",
            ),
        )
        .await;
        assert!(
            second.is_err(),
            "a second deposit got past the orphan's lock"
        );
        assert!(ui.statuses().iter().any(|s| s.contains("busy")));
        assert!(
            !w.path().join(INPUT_FILE).exists(),
            "the second deposit's fetcher ran"
        );
        assert_eq!(prover_pid(&dir).await, pid);
        // SAFETY: plain kill(2).
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        assert!(eventually(|| ProverLock::try_take(&dir).unwrap().is_some()).await);
    }
}
