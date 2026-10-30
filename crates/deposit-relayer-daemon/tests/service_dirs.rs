//! bridge-audit#132: a working cycle writes only into the state, work and
//! log directories. The system temp directory and `$HOME` point at a
//! read-only directory here, the three `deposit-prover` tools are fakes that
//! copy canned operands to the paths they are told to write, and after the
//! deposit is finalized nothing exists outside the three directories.
//!
//! Its own test binary: it sets `TMPDIR` and `HOME` for the whole process.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use alloy::primitives::{Address, B256, U256};
use deposit_relayer_daemon::{
    DepositEvent, DepositProverBins, InMemoryDepositSource, MockAnSubmitter, MockProofGenerator,
    ProofGenerator, Relayer, RelayerConfig, ServiceDirs, SubprocessProofGenerator,
    SubprocessProverConfig, TickOutcome, STATE_FILE_NAME,
};

fn deposit(id: u64) -> DepositEvent {
    DepositEvent {
        deposit_id: id,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(id * 1000 + 1),
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(1_700_000_000u64),
        tx_hash: B256::repeat_byte(0xaa),
        log_index: 0,
        block_number: 100 + id,
        block_hash: B256::repeat_byte(0xcd),
        source_contract: Address::repeat_byte(0x22),
        source_chain_id: 1,
    }
}

/// A fake tool: `cp` of the canned file to the path after `flag`, plus a
/// line on each stream so the transcript is not empty. Written through
/// `cp` so no file descriptor from this process holds it open when it
/// execs (see `prover.rs` tests).
fn install_tool(bin: &Path, flag: &str, canned: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let script = format!(
        "#!/bin/sh\nout=\"\"\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = \"{flag}\" ]; then out=$2; \
         shift; fi\n  shift\ndone\n[ -n \"$out\" ] || {{ echo \"no {flag}\" >&2; exit 2; }}\necho \
         \"fake $(basename \"$0\") -> $out\"\necho \"fake stderr\" >&2\ncp \"{canned}\" \"$out\"\n",
        canned = canned.display(),
    );
    let source = bin.with_extension("src");
    std::fs::write(&source, script).unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(std::process::Command::new("cp")
        .arg(&source)
        .arg(bin)
        .status()
        .unwrap()
        .success());
    std::fs::remove_file(&source).unwrap();
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn a_cycle_writes_only_into_the_three_directories() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();

    // Read-only temp and home for everything this process and its children
    // start from here on. As root the mode is not enforced; the directory
    // listing below catches a write either way.
    let ro = root.join("ro");
    std::fs::create_dir(&ro).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
    }
    std::env::set_var("TMPDIR", &ro);
    std::env::set_var("HOME", &ro);

    // Canned operands: what the real tools would produce for this deposit.
    let event = deposit(7);
    let canned = MockProofGenerator::new().generate(&event).await.unwrap();
    let canned_dir = root.join("canned");
    std::fs::create_dir(&canned_dir).unwrap();
    std::fs::write(canned_dir.join("input.json"), b"{}").unwrap();
    std::fs::write(canned_dir.join("vk_blob.bin"), &canned.vk_blob).unwrap();
    std::fs::write(canned_dir.join("public_inputs.bin"), &canned.public_inputs).unwrap();
    std::fs::write(canned_dir.join("proof.bin"), &canned.proof).unwrap();

    // `export_blake2b_proof` writes two files; the fake copies the proof to
    // `--proof-out` and the public inputs through a second fake step is not
    // how the tool works, so it copies the proof and then the public inputs
    // are copied by a wrapper around the same script.
    let bins_dir = root.join("bins");
    std::fs::create_dir(&bins_dir).unwrap();
    install_tool(
        &bins_dir.join("fetch_deposit_data"),
        "--output",
        &canned_dir.join("input.json"),
    );
    install_tool(
        &bins_dir.join("export_vk_blob"),
        "--output",
        &canned_dir.join("vk_blob.bin"),
    );
    install_tool(
        &bins_dir.join("proof_only"),
        "--proof-out",
        &canned_dir.join("proof.bin"),
    );
    install_tool(
        &bins_dir.join("pubin_only"),
        "--pubin-out",
        &canned_dir.join("public_inputs.bin"),
    );
    {
        use std::os::unix::fs::PermissionsExt;
        let wrapper = bins_dir.join("export_blake2b_proof");
        let script = format!(
            "#!/bin/sh\n\"{p}\" \"$@\" && \"{q}\" \"$@\"\n",
            p = bins_dir.join("proof_only").display(),
            q = bins_dir.join("pubin_only").display(),
        );
        let source = wrapper.with_extension("src");
        std::fs::write(&source, script).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(std::process::Command::new("cp")
            .arg(&source)
            .arg(&wrapper)
            .status()
            .unwrap()
            .success());
        std::fs::remove_file(&source).unwrap();
    }
    let bins = DepositProverBins {
        fetch_deposit_data: bins_dir.join("fetch_deposit_data"),
        export_vk_blob: bins_dir.join("export_vk_blob"),
        export_blake2b_proof: bins_dir.join("export_blake2b_proof"),
    }
    .validated()
    .unwrap();

    // The tools' working directory (the prover tree) stays untouched too.
    let prover_dir = root.join("prover");
    std::fs::create_dir(&prover_dir).unwrap();

    let dirs = ServiceDirs {
        state: root.join("state"),
        work: root.join("work"),
        log: root.join("logs"),
    };
    dirs.prepare().unwrap();

    let prover = SubprocessProofGenerator::new(SubprocessProverConfig::new(
        &prover_dir,
        bins,
        "http://rpc.invalid",
        &dirs.work,
        &dirs.log,
    ));
    let source = InMemoryDepositSource::new();
    source.insert(event.clone());
    let mut cfg = RelayerConfig::new(dirs.state_file());
    cfg.start_deposit_id = 7;
    let mut relayer = Relayer::new(
        cfg,
        Arc::new(source),
        Arc::new(prover),
        Arc::new(MockAnSubmitter::accepting()),
    )
    .unwrap();

    let outcome = relayer.tick().await.unwrap();
    assert!(
        matches!(outcome, TickOutcome::Finalized {
            deposit_id: 7,
            ..
        }),
        "{outcome:?}"
    );

    // State: the file, nothing else (the atomic write's `.tmp` is renamed
    // away; the lock belongs to the daemon binary, not the loop).
    assert_eq!(entries(&dirs.state), vec![STATE_FILE_NAME.to_string()]);
    // Work: the per-deposit directory is gone with the proof.
    assert_eq!(entries(&dirs.work), Vec::<String>::new());
    // Logs: one transcript per tool run, named after the deposit and the
    // tool, each holding both streams.
    let logs = entries(&dirs.log);
    assert_eq!(logs.len(), 3, "{logs:?}");
    for (tool, name) in [
        "fetch_deposit_data",
        "export_vk_blob",
        "export_blake2b_proof",
    ]
    .iter()
    .zip(&logs)
    {
        assert!(name.starts_with("deposit-7-"), "{name}");
        let text = std::fs::read_to_string(dirs.log.join(name)).unwrap();
        assert!(
            text.contains("fake stderr") && text.contains("--- exit: "),
            "{name}: {text}"
        );
        let _ = tool;
    }
    assert!(
        logs.iter().any(|n| n.contains("-export_blake2b_proof-")),
        "{logs:?}"
    );
    // Nowhere else: not the read-only temp/home, not the prover tree, not
    // beside the test's own directories.
    assert_eq!(entries(&ro), Vec::<String>::new());
    assert_eq!(entries(&prover_dir), Vec::<String>::new());
    let mut top = entries(root);
    top.retain(|n| {
        !["bins", "canned", "logs", "prover", "ro", "state", "work"].contains(&n.as_str())
    });
    assert_eq!(top, Vec::<String>::new());
    let _: PathBuf = dirs.state_file();
}
