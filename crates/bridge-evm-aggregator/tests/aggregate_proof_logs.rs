//! Where `aggregate-proof` says what it is doing. Its callers —
//! `ackinacki-bridge withdraw` and the relayer — forward its output into their
//! own logs, and the pk cache's notices (waiting for another keygen, a
//! filesystem without locks, leftovers removed, a slot loaded or generated)
//! are info and warn lines: a default that let through only errors left the
//! operator none of them. Which targets pass is pinned in `logging.rs`.

use std::process::Command;

#[test]
fn its_progress_reaches_stderr_without_rust_log() {
    let dir = std::env::temp_dir().join(format!("agg_logs_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_aggregate-proof"))
        .arg("--inner-snark")
        .arg(dir.join("missing.snark"))
        .args(["--name", "BridgeWithdrawalAggregatorVerifier", "--out"])
        .arg(dir.join("calldata.bin"))
        .arg("--pk-cache-dir")
        .arg(dir.join("pk_cache"))
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert!(!out.status.success(), "there is no inner snark");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stderr.contains("INFO") && stderr.contains("aggregate-proof started"),
        "stderr: {stderr}\nstdout: {stdout}"
    );
    assert!(!stderr.contains('\x1b'), "no colour into a pipe: {stderr}");
    // The callers log each line at the level it starts with.
    assert!(
        stderr
            .lines()
            .any(|l| l.split_whitespace().next() == Some("INFO")
                && l.contains("aggregate-proof started")),
        "a line starts with its level: {stderr}"
    );
}
