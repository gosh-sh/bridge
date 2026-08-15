# TD-30 — Prover RPC ≠ source RPC preflight

PoC: `tests/td_30_dual_rpc_preflight.rs`, `src/rpc_preflight.rs`.

## Gate (TD-30 fix)

`run_daemon` / `prove-one` in `deposit-relayer.rs`:

1. `eth_chainId` on `--rpc-url` (source)
2. If `--prover-rpc-url` differs → second `eth_chainId` must match
3. Fail **before** first prove/tick

Closes TD-08 gap «no prover-RPC preflight».

## Verdict: **OK**

| Check | Outcome |
|-------|---------|
| Sepolia vs Base at startup | `Err` preflight |
| Matching RPCs | pass + tick smoke |
| Bypass preflight (split-brain prover) | TD-08 `check_binds_to` still blocks mint |

## Commands

    cd crates/deposit-relayer-daemon && cargo test td_30 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_08 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test
