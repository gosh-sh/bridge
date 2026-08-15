# TD-29 — RPC `unwrap_or_default` log_index / block_hash

PoC: `crates/deposit-relayer-daemon/tests/td_29_rpc_default_log_index.rs`.

## Defaults (`source.rs`)

| Field | `unwrap_or_default` | Risk |
|-------|---------------------|------|
| `log_index` | `0` | Wrong receipt position or mapping `Eth` error → HOL |
| `block_hash` | `B256::ZERO` | PI/bind mismatch or anchor reject |

## Verdict: **QC** (Opus D-11)

No wrong-deposit mint in PoC — anchor gate / `check_binds_to` fail-closed. Ops must not trust defaults; monitor RPC completeness.

## Commands

    cd crates/deposit-relayer-daemon && cargo test td_29 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test
