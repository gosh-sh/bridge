//! `scripts/install.sh` against a release served from a local directory: an
//! installation whose `aggregate-proof` predates `--slot-report` runs, but
//! this CLI refuses it, so the script has to count it as missing and replace
//! it from the release, not call it ok.

use std::{os::unix::fs::PermissionsExt as _, path::Path, process::Command};

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/install.sh");

/// An executable shell script at `path` that prints `text` and exits 0.
fn stub(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\necho '{text}'\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The parts of an installation `install.sh` looks for, laid out under
/// `root` the way it lays them out (the bundle's layout, minus `bin/`).
fn installation(root: &Path, aggregator_help: &str) {
    stub(&root.join("ackinacki-bridge"), "ackinacki-bridge 0.0.0");
    stub(&root.join("aggregate-proof"), aggregator_help);
    for f in ["fetch_deposit_data", "export_blake2b_proof"] {
        stub(&root.join("deposit-prover").join(f), "usage");
    }
    std::fs::create_dir_all(root.join("deposit-prover/configs")).unwrap();
    std::fs::write(
        root.join("deposit-prover/configs/circuit_params.json"),
        "{}",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("verifiers")).unwrap();
    for f in [
        "BridgeWithdrawalAggregatorVerifier.bin",
        "BridgeWithdrawalAggregatorVerifier.sol",
        "BridgeWithdrawalAggregatorVerifier_calldata.bin",
    ] {
        std::fs::write(root.join("verifiers").join(f), "x").unwrap();
    }
    std::fs::write(root.join("bridge_config"), "BRIDGE_NETWORK=shellnet\n").unwrap();
}

fn run(prefix: &Path, release: &Path, args: &[&str]) -> String {
    let out = Command::new("bash")
        .arg(SCRIPT)
        .args(args)
        .arg("--prefix")
        .arg(prefix)
        .env(
            "BRIDGE_RELEASE_BASE",
            format!("file://{}", release.display()),
        )
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn an_aggregate_proof_without_slot_report_is_replaced_from_the_release() {
    for tool in ["bash", "curl", "tar", "sha256sum"] {
        if Command::new("sh")
            .args(["-c", &format!("command -v {tool}")])
            .output()
            .map_or(true, |o| !o.status.success())
        {
            eprintln!("no {tool}: nothing to learn here");
            return;
        }
    }
    let dir = tempfile::tempdir().unwrap();

    // The release: the bundle, a top directory stripped on unpacking, with
    // an aggregate-proof that takes the flag.
    let release = dir.path().join("release");
    let staged = dir.path().join("staged/ackinacki-bridge");
    installation(&staged, "unused");
    // Help that goes on long after the flag: a probe that stops reading at
    // the match leaves the binary to die of SIGPIPE, which pipefail reports.
    std::fs::write(
        staged.join("aggregate-proof"),
        "#!/bin/sh\necho 'aggregate-proof --inner-snark <path> [--slot-report <path>]'\nseq 1 \
         200000\n",
    )
    .unwrap();
    std::fs::create_dir_all(&release).unwrap();
    let bundle = "ackinacki-bridge-linux-x86_64.tar.gz";
    let packed = Command::new("tar")
        .arg("-czf")
        .arg(release.join(bundle))
        .arg("-C")
        .arg(dir.path().join("staged"))
        .arg("ackinacki-bridge")
        .status()
        .unwrap();
    assert!(packed.success());
    let sums = Command::new("sha256sum")
        .arg(bundle)
        .current_dir(&release)
        .output()
        .unwrap();
    std::fs::write(release.join("SHA256SUMS"), sums.stdout).unwrap();

    // The installation: everything there, the aggregate-proof from before.
    let prefix = dir.path().join("prefix");
    let old = installation_at(&prefix);
    stub(
        &old,
        "aggregate-proof --inner-snark <path> [--pk-cache-dir <dir>]",
    );

    let checked = run(&prefix, &release, &["--check"]);
    assert!(
        checked.contains(&format!("MISSING {bundle}"))
            && checked.contains("predates --slot-report"),
        "the old aggregator is missing, and says why:\n{checked}"
    );

    run(&prefix, &release, &["--yes"]);
    let help = Command::new(&old).arg("--help").output().unwrap();
    assert!(
        String::from_utf8_lossy(&help.stdout).contains("--slot-report"),
        "the release's aggregate-proof replaced it"
    );
    let rechecked = run(&prefix, &release, &["--check"]);
    assert!(
        rechecked.contains(&format!("ok      {}", old.display())),
        "and is ok now:\n{rechecked}"
    );
}

/// Lay out an installation under `prefix` as `install.sh` does, and return
/// where its aggregate-proof is.
fn installation_at(prefix: &Path) -> std::path::PathBuf {
    let staged = prefix.join(".staged");
    installation(&staged, "unused");
    let bin = prefix.join("bin/ackinacki-bridge");
    let agg = prefix.join("aggregator/target/release/aggregate-proof");
    for (from, to) in [
        (staged.join("ackinacki-bridge"), bin.clone()),
        (staged.join("aggregate-proof"), agg.clone()),
    ] {
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::rename(from, to).unwrap();
    }
    std::fs::rename(staged.join("verifiers"), prefix.join("verifiers")).unwrap();
    std::fs::rename(staged.join("deposit-prover"), prefix.join("deposit-prover")).unwrap();
    std::fs::remove_dir_all(&staged).unwrap();
    agg
}
