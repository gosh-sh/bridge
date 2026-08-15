# TD-40 — CLI backoff=0, Pending timeout, fsync policy (DEP-BACKOFF / D-16)

PoC: `td_40_cli_backoff_pending.rs`.  
Cross-ref: TD-18 (`state.save` tmp+fsync+rename), TD-26/28 (`AnPending` retry), `f10_proptest` backoff props.

## Config → outcome

| Config / event | Outcome | BC? |
|----------------|---------|-----|
| `backoff_initial_secs = 0` | `BackoffConfig::validate()` Err; CLI daemon startup fail | No — guarded |
| `backoff_multiplier = 0` | `validate()` Err; CLI daemon startup fail | No — guarded |
| `SubmitOutcome::Pending` (timeout mock) | `TickOutcome::AnPending`; `record_attempt`; retry succeeds | No — recovery path |
| `AnPending` tick | `last_processed` unchanged; scan cursor unchanged | No |
| `RelayerState::save` | tmp → fsync → rename; corrupt `.tmp` ignored on load | No — TD-18 |

## Verdict: **partial QC** (ops/liveness documented)

Zero backoff blocked at validate. Pending after confirmation timeout is transient — relayer retries without advancing deposit cursor. Not BC (no permanent hang without operator skip/finalize-one).

## Commands

    cd crates/deposit-relayer-daemon && cargo test td_40 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test f10_proptest -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_18 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test
