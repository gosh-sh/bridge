//! bridge-audit#132: a working cycle writes only into the state directory.
//! The system temp directory and `$HOME` point at a read-only directory here,
//! and after the deposit is finalized nothing exists outside the service
//! directories.
//!
//! Its own test binary: it sets `TMPDIR` and `HOME` for the whole process.

use std::{path::Path, sync::Arc};

use alloy::primitives::{Address, B256, U256};
use deposit_relayer_daemon::{
    DepositEvent, InMemoryDepositSource, MockAnSubmitter, MockWitnessBuilder, Relayer,
    RelayerConfig, ServiceDirs, TickOutcome, STATE_FILE_NAME,
};

fn deposit(id: u64) -> DepositEvent {
    DepositEvent {
        deposit_id: id,
        token: Address::repeat_byte(0x7c),
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

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn a_cycle_writes_only_into_the_service_directories() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();

    // Read-only temp and home for everything this process starts from here
    // on. As root the mode is not enforced; the directory listing below
    // catches a write either way.
    let ro = root.join("ro");
    std::fs::create_dir(&ro).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
    }
    std::env::set_var("TMPDIR", &ro);
    std::env::set_var("HOME", &ro);

    let dirs = ServiceDirs {
        state: root.join("state"),
        log: root.join("logs"),
    };
    dirs.prepare().unwrap();

    let source = InMemoryDepositSource::new();
    source.insert(deposit(7));
    let mut cfg = RelayerConfig::new(dirs.state_file());
    cfg.start_deposit_id = 7;
    let mut relayer = Relayer::new(
        cfg,
        Arc::new(source),
        Arc::new(MockWitnessBuilder::new()),
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
    // The loop itself logs to tracing; the daily file is the binary's.
    assert_eq!(entries(&dirs.log), Vec::<String>::new());
    // Nowhere else: not the read-only temp/home, not beside the service
    // directories.
    assert_eq!(entries(&ro), Vec::<String>::new());
    assert_eq!(entries(root), vec![
        "logs".to_string(),
        "ro".to_string(),
        "state".to_string()
    ]);
}
