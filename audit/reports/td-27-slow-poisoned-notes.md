# TD-27 — Slow deposit vs poisoned — one `record_failure` path

PoC: `crates/deposit-relayer-daemon/tests/td_27_slow_vs_poisoned.rs`.

## Shared path

`NotYetAvailable`, `ProofFailed`, and `AnRejected` all call `record_failure` →
`record_attempt` + optional `record_skip` when `attempts_since_progress >= skip_after_attempts`.

`attempts_since_progress` alone does **not** distinguish slow vs poisoned.

## Verdict: **QC** (Opus D-16)

| Scenario | Outcome |
|----------|---------|
| Slow, appears before threshold tick | Finalizes; **not** in `parked_deposit_ids` |
| Poison at threshold | `Skipped`; id in `parked_deposit_ids` |
| Slow never confirms past threshold | **Parked** same as poison — ops must read logs (`NotYetAvailable` vs `ProofFailed`) |

Not BC: parked list + `finalize-one` recovery path exists (TD-26).

## Commands

    cd crates/deposit-relayer-daemon && cargo test td_27 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test
