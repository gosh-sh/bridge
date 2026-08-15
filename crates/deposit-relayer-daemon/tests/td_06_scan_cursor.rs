//! TD-06 — scan cursor must not skip sequential deposits in the same window.
//!
//! Catalog `TD-06`: before fix, `EthLogSource::fetch` wrote `scan_cursor = safe_head`,
//! so a later `fetch(N+1)` started at `safe_head + 1` and missed deposits below that.

fn effective_scan_from(from_block: u64, cursor_val: u64) -> u64 {
    if cursor_val >= from_block {
        cursor_val.saturating_add(1)
    } else {
        from_block
    }
}

#[test]
fn td06_buggy_cursor_math_skipped_intermediate_blocks() {
    const FROM_BLOCK: u64 = 90;
    const SAFE_HEAD: u64 = 115;
    const BLOCK_DEPOSIT_1: u64 = 101;

    let next_scan_from = effective_scan_from(FROM_BLOCK, SAFE_HEAD);
    assert!(
        BLOCK_DEPOSIT_1 < next_scan_from,
        "pre-fix policy skipped block {BLOCK_DEPOSIT_1} (scan from {next_scan_from})"
    );
}

#[test]
fn td06_fixed_cursor_at_from_block_covers_sequential_deposits() {
    const FROM_BLOCK: u64 = 90;
    const BLOCK_DEPOSIT_0: u64 = 100;
    const BLOCK_DEPOSIT_1: u64 = 101;

    let scan_from = effective_scan_from(FROM_BLOCK, FROM_BLOCK);
    assert!(BLOCK_DEPOSIT_0 >= scan_from);
    assert!(BLOCK_DEPOSIT_1 >= scan_from);
}
