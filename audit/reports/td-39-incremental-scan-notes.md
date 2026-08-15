# TD-39 — Incremental scan vs O(head) rescan (QC-OFF-10)

PoC: `td_39_incremental_scan_cost.rs`.  
Cross-ref: TD-06 (no cursor advance on fetch), TD-07 (proof fail), F10 remediation QC-OFF-10.

## `safe_head` vs chunk calls per tick (from_block=90, cursor=90 → scan_from=91)

| safe_head | chunk calls (10-block windows) | Notes |
|-----------|-------------------------------|-------|
| 100 | 1 | 91–100 |
| 120 | 3 | 91–120 |
| 140 | 5 | 91–140 |
| 140 (cursor advanced to 139) | 1 | tail only 140 |

Targeted `fetch(deposit_id)` does **not** advance `scan_cursor` (TD-06) → each relayer tick rescans `[scan_from, safe_head]` → **O(head) RPC cost** per deposit attempt.

Advancing `scanned_through_block` / shared cursor to chain tail (daemon-level incremental scan — not implemented on fetch today) reduces cost to O(new blocks).

## Verdict: **INV/QC** (not BC)

No deposit loss on rescan; cost/liveness grief for ops at scale. Documented QC-OFF-10 remediation target.

## Commands

    cd crates/deposit-relayer-daemon && cargo test td_39 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_06 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_07 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test
