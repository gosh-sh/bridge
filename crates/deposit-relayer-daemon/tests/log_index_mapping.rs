//! Regression test for the production discovery glue: mapping a block-global
//! `logIndex` (from `eth_getLogs`) to the receipt-local index `deposit-prover`
//! consumes.
//!
//! Fixture: real Sepolia `depositId=0` tx
//! `0x9ac341666f70d55780f289187c11a0537a52f7381e5c4b1ebe6f671314552adf`.

use std::{fs, path::Path};

use alloy::rpc::types::Log;
use deposit_relayer_daemon::receipt_log_index_from_block_log;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Fixture {
    block_log_index: u64,
    expected_receipt_log_index: u64,
    receipt_logs: Vec<Log>,
}

#[test]
fn sepolia_deposit_id0_block_log_index_maps_to_receipt_local_index() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sepolia_deposit_id0.json");
    let raw = fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("read {}: {e}", path.display());
    });
    let fixture: Fixture =
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));

    let got = receipt_log_index_from_block_log(&fixture.receipt_logs, fixture.block_log_index)
        .unwrap_or_else(|e| panic!("mapping failed: {e}"));

    assert_eq!(
        got, fixture.expected_receipt_log_index,
        "block logIndex {} should be receipt position {}",
        fixture.block_log_index, fixture.expected_receipt_log_index,
    );
}
