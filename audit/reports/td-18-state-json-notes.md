# TD-18 — `state.json` durability

PoC: `tests/td_18_state_json_durability.rs`, `state.rs` unit tests, fix `reset_for_new_deployment` on `--force-state`.

## Mechanisms

| Path | Behaviour |
|------|-----------|
| `save` | write `state.json.tmp` → fsync → rename |
| `load` | reads `state.json` only; partial `.tmp` ignored |
| `ensure_deployment` mismatch | error unless `force_state` |
| `force_state` + mismatch | stamp new deployment + **reset** progress cursors |

## Verdict: **OK**

1. Atomic roundtrip + corrupt JSON → safe error (no silent defaults).
2. Kill-sim: partial `.tmp` without rename does not poison load.
3. `last_processed=5` + different `dapp_id` without force → startup reject.
4. With force → cursor reset; relayer targets `start_deposit_id` (no wrong-namespace inherit).

**Not BC:** no skip/double-mint observed on crash/partial state in PoC.

**QC:** operator must use `--force-state` knowingly; AN nullifier is authoritative for double-mint guard.

## Commands

    cd crates/deposit-relayer-daemon && cargo test td_18 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test
