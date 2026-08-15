# TD-63 — `ethereum_fetcher` parse edge (data width, topic0)

PoC: `deposit-prover/tests/td_63_ethereum_fetcher_parse_edge.rs`.

## ABI data layout (4 × 32B words)

| Word | Field | Notes |
|------|-------|-------|
| 0 | `amount` | uint256, 32 bytes |
| 1 | `anWorkchain` | int8 in last byte of word |
| 2 | `anAccount` | bytes32 |
| 3 | `timestamp` | uint256; parse uses low 8 bytes (120–127) |

Indexed: topic0 = signature, topic1 = `depositId`, topic2 = `sender`.

## Fetcher vs circuit boundary

| `data.len()` | `parse_deposit_event` | Circuit (TD-11) |
|--------------|----------------------|-----------------|
| 127 | Err (too short) | — |
| 128 | Ok | Ok (capacity pin) |
| 129+ | Ok (reads first 128B only) | Reject (QC) |

## Parse edge matrix

| Case | Result |
|------|--------|
| (a) Canonical 128B + topics | Ok |
| (b) 127B data | Err |
| (c) 129B data | Parse Ok; circuit reject (TD-11 cross-ref) |
| (d) Wrong topic0 | Err signature |
| (e) topics < 3 | Err |
| (f) Wrong log address | Err |

## Verdict: **META/QC**

Fetcher fail-closed on bad signature / short data / wrong contract; extra data bytes are QC at circuit layer.

## Commands

    cd deposit-prover && cargo test td_63 -- --nocapture
    cd deposit-prover && cargo test td_11_capacity_bounds -- --nocapture
    cd deposit-prover && cargo test td_59 -- --nocapture
