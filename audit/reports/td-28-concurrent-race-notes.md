# TD-28 — Concurrent submitters / lost receipt / double-mint race

PoC: `crates/deposit-relayer-daemon/tests/td_28_concurrent_submit_race.rs`.

## Scenarios

| Case | Mint count | Second outcome |
|------|------------|----------------|
| Barrier concurrent ticks | 1 | `AlreadyFinalized` |
| Lost receipt (Pending after mint) | 1 | retry `AlreadyFinalized` |
| DEP-N-5 barrier race | 1 | one winner |

## Verdict: **OK** (not BC)

AN nullifier mirror rejects second finalize on same L1 deposit. Lost receipt is **liveness/grief** (extra prove gas) — QC-OFF-05/06 ops, not double-mint.

No `audit/findings/BRIDGE-XXX/` — safety holds under mock race.

## Commands

    cd crates/deposit-relayer-daemon && cargo test td_28 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test f10_competing -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test
