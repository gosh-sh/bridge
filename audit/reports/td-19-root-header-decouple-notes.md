# TD-19 — receipt root / header hash decouple

PoC: `tests/td_19_root_header_decouple.rs` (BC-CIRCUIT-004 binding).

## Mechanisms

| Attack shape | Circuit behaviour |
|--------------|-------------------|
| Header `receiptsRoot` ≠ MPT-verified root | **Reject** (byte-wise bind field 5) |
| Corrupt `receipt_root` witness field | **Reject** (TD-09 class) |
| Mutated `block_header_rlp` | **Reject** (keccak / RLP constraints) |
| Stale RPC `blockHash` vs PI | **OK** off-circuit — relayer `check_binds_to` |

## Verdict: **OK**

Decoupled roots/header do not satisfy MockProver. No BC finding — fix #1/#2 from circuit review holds.

**QC:** forged self-consistent header still proves (TD-03); canonicality is AN anchor ops.

## Commands

    cd deposit-prover && cargo test td_19 -- --nocapture
    cd deposit-prover && cargo test
