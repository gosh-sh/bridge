//! bridge-audit#132: a step cycle writes only into the state, work and log
//! directories. The system temp directory and `$HOME` point at a read-only
//! directory here, the step prover is a fake that writes the bundle it is
//! told to, and after `submitUpdate` nothing exists outside the three
//! directories.
//!
//! Its own test binary: it sets `TMPDIR` and `HOME` for the whole process.

use std::{path::Path, sync::Arc};

use eth_light_client_relayer::{
    pack_step_public_inputs, parse_finality_update, InMemoryBeaconSource, MockAnSubmitter, Relayer,
    RelayerConfig, ServiceDirs, SubprocessProofGenerator, SubprocessProverConfig, TickOutcome,
    STATE_FILE_NAME,
};

fn update() -> eth_light_client_relayer::FinalityUpdate {
    let json = r#"{"data":{
      "attested_header":{"beacon":{"slot":"5","state_root":"0x1111111111111111111111111111111111111111111111111111111111111111"}},
      "finalized_header":{
        "beacon":{"slot":"4","state_root":"0x1111111111111111111111111111111111111111111111111111111111111111"},
        "execution":{"block_hash":"0x2222222222222222222222222222222222222222222222222222222222222222"}
      },
      "sync_aggregate":{"sync_committee_bits":"0x01"}
    }}"#;
    let mut update = parse_finality_update(json).unwrap();
    update.committee_json = "[]".into();
    update
}

/// A fake step prover: writes a bundle of the right shape into
/// `STEP_OUT_DIR` and a line on each stream. Installed through `cp` so no
/// file descriptor from this process holds it open when it execs.
fn install_step_prover(bin: &Path, public_inputs_len: usize) {
    use std::os::unix::fs::PermissionsExt;
    let script = format!(
        "#!/bin/sh\necho \"fake step prover in $(pwd)\"\necho \"fake stderr\" >&2\nhead -c {n} \
         /dev/zero > \"$STEP_OUT_DIR/step_public_inputs.bin\"\nprintf proof > \
         \"$STEP_OUT_DIR/step_proof_blake2b.bin\"\n",
        n = public_inputs_len,
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
async fn a_step_cycle_writes_only_into_the_three_directories() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();

    // Read-only temp and home for this process and its children from here
    // on. As root the mode is not enforced; the listing below catches a
    // write either way.
    let ro = root.join("ro");
    std::fs::create_dir(&ro).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
    }
    std::env::set_var("TMPDIR", &ro);
    std::env::set_var("HOME", &ro);

    let bins = root.join("bins");
    std::fs::create_dir(&bins).unwrap();
    let step = bins.join("export_step_vk_blob");
    let (pi, _) = pack_step_public_inputs(&update(), [0u8; 32]);
    install_step_prover(&step, pi.len());
    // The prover's working directory (the prover tree) stays untouched.
    let prover_dir = root.join("prover");
    std::fs::create_dir(&prover_dir).unwrap();

    let dirs = ServiceDirs {
        state: root.join("state"),
        work: root.join("work"),
        log: root.join("logs"),
    };
    dirs.prepare().unwrap();

    let prover = SubprocessProofGenerator::new(SubprocessProverConfig {
        step_prover_bin: step,
        rotate_prover_bin: None,
        prover_dir: Some(prover_dir.clone()),
        srs_path: "srs.bin".into(),
        timeout: std::time::Duration::from_secs(30),
        work_dir: dirs.work.clone(),
        log_dir: dirs.log.clone(),
    });
    let mut cfg = RelayerConfig::new(dirs.state_file(STATE_FILE_NAME));
    cfg.enable_rotate = false;
    cfg.flip_owner = false;
    let mut relayer = Relayer::new(
        cfg,
        Arc::new(InMemoryBeaconSource::new(vec![update()])),
        Arc::new(prover),
        Arc::new(MockAnSubmitter::accepting()),
    )
    .unwrap();

    let outcome = relayer.tick().await.unwrap();
    assert!(
        matches!(outcome, TickOutcome::SubmittedUpdate { .. }),
        "{outcome:?}"
    );

    // State: the file, nothing else (the atomic write's `.tmp` is renamed
    // away; the lock belongs to the daemon binary, not the loop).
    assert_eq!(entries(&dirs.state), vec![STATE_FILE_NAME.to_string()]);
    // Work: the per-prove directory is gone with the bundle.
    assert_eq!(entries(&dirs.work), Vec::<String>::new());
    // Logs: the prover's transcript, both streams, named after the step.
    assert_eq!(entries(&dirs.log), vec![
        "step-5-stderr.log".to_string(),
        "step-5-stdout.log".to_string()
    ]);
    assert!(std::fs::read_to_string(dirs.log.join("step-5-stderr.log"))
        .unwrap()
        .contains("fake stderr"));
    // Nowhere else.
    assert_eq!(entries(&ro), Vec::<String>::new());
    assert_eq!(entries(&prover_dir), Vec::<String>::new());
    let mut top = entries(root);
    top.retain(|n| !["bins", "logs", "prover", "ro", "state", "work"].contains(&n.as_str()));
    assert_eq!(top, Vec::<String>::new());
}
