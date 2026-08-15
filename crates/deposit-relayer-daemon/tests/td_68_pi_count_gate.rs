//! TD-68 — `NUM_PUBLIC_INPUTS == 12` cross-source gate (docs + code drift).

use std::path::PathBuf;

use deposit_relayer_daemon::types::{NUM_PUBLIC_INPUTS, PUBLIC_INPUT_BYTES};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../")
}

fn read_repo_file(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("TD-68: read {rel}: {e}"))
}

#[test]
fn td_68_relayer_num_public_inputs_is_twelve() {
    assert_eq!(NUM_PUBLIC_INPUTS, 12);
    assert_eq!(PUBLIC_INPUT_BYTES, 384, "12 × 32-byte Fr scalars");
}

#[test]
fn td_68_deposit_prover_sources_agree_on_twelve_pi() {
    let circuit = read_repo_file("deposit-prover/src/circuit_v2.rs");
    assert!(
        circuit.contains("pub const DEPOSIT_PUBLIC_INPUT_LAYOUT: [&str; 12]"),
        "DEPOSIT_PUBLIC_INPUT_LAYOUT must be 12 slots"
    );
    assert!(
        circuit.contains("\"chainId\"") && circuit.contains("\"promiseCommit\""),
        "layout must list chainId and promiseCommit"
    );

    let types = read_repo_file("deposit-prover/src/types.rs");
    assert!(
        types.contains("DEPOSIT_NUM_PUBLIC_INPUTS"),
        "deposit-prover NUM_PUBLIC_INPUTS must derive from layout table"
    );
    assert!(
        !types.contains("NUM_PUBLIC_INPUTS: usize = 11"),
        "stale hand-maintained 11-PI constant in deposit-prover/types.rs"
    );
}

#[test]
fn td_68_project_facts_deposit_row_twelve_pi() {
    let facts = read_repo_file("audit/PROJECT_FACTS.md");
    let row = facts
        .lines()
        .find(|l| l.starts_with("| Deposit |"))
        .expect("PROJECT_FACTS Deposit row");
    assert!(
        row.contains("**12**"),
        "PROJECT_FACTS Deposit PI count must be **12**: {row}"
    );
    assert!(
        !row.contains("| **11** |"),
        "PROJECT_FACTS must not list 11 deposit PI: {row}"
    );
}

#[test]
fn td_68_check_pi_count_docs_script_passes() {
    let script = repo_root().join("scripts/check_pi_count_docs.sh");
    let status = std::process::Command::new("bash")
        .arg(&script)
        .status()
        .expect("spawn check_pi_count_docs.sh");
    assert!(
        status.success(),
        "scripts/check_pi_count_docs.sh must pass (TD-68 docs gate)"
    );
}
