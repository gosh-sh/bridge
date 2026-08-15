# TD-26 — HOL blocking + `record_skip` / parked IDs

PoC: `crates/deposit-relayer-daemon/tests/td_26_hol_skip_policy.rs`.

## Policy (QC-OFF-01)

| Mechanism | Behavior |
|-----------|----------|
| HOL cursor | `next_target = last_processed + 1` — stuck id blocks later ids |
| `--skip-after-attempts` | `record_skip` parks id + advances cursor |
| `parked_deposit_ids` | Persisted in `state.json` for operator `finalize-one` |
| Transient errors | `ProofFailed` / `AnPending` increment attempts; recovery before threshold avoids park |

## Verdict: **QC**

Operator must manually finalize parked ids. Park list is **not** auto-cleared on later AN finalize — documented ops policy, not BC (deposit recoverable via `finalize-one`).

## Commands

    cd crates/deposit-relayer-daemon && cargo test td_26 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test f10_head_of_line -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test
