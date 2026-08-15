# TD-44 — event `timestamp` / `anWorkchain` not in PI (mutation docs)

PoC: `DepositEventPiGap.t.sol`, `td_44_event_pi_gap.rs`, `td_44_event_fields_ignored.rs`.

Cross-ref: TD-31 (`anWorkchain`), TD-21 (`promiseCommit`), TD-32 (header RLP slot 11 → `blockHash`).

## Binding matrix

| Layer | Field | PI slot | Bound? | Notes |
|-------|-------|---------|--------|-------|
| L1 `Deposit` event | `amount` (data w0) | 2 | Yes | treasury += amount |
| L1 | `sender` (topic2) | 1 | Yes | |
| L1 | `depositId` (topic1) | 0 | Yes | |
| L1 | `anWorkchain` (data w1, byte 63) | none | **No** | TD-31 QC |
| L1 | `anAccount` (data w2) | 7–8 | Yes | |
| L1 | `timestamp` (data w3, bytes 96–127) | none | **No** | TD-44 INV/QC |
| L1 bridge state | event `timestamp` | — | **No** | counter + treasury only |
| ZK header witness | `timestamp` (RLP slot 11) | 9–10 indirect | Yes | via `keccak256(header_rlp)` = `blockHash` PI |
| Relayer `check_binds_to` | `event.timestamp` | — | **No** | TD-44 |
| Relayer `check_binds_to` | `event.an_workchain` | — | **No** | TD-31 |
| AN mint | workchain | — | WC **0** | QC-AN-J4 |

## Mutation pass/fail (MockProver / relayer)

| Mutation | MockProver | PI vs baseline | `check_binds_to` | Verdict |
|----------|------------|----------------|------------------|---------|
| Event log data timestamp word (96–127) | pass | slots 0–8 identical | pass | INV/QC (blockHash/promiseCommit drift with receipt) |
| `anWorkchain` byte 63 | pass | identical | pass | QC (TD-31) |
| `event_data.timestamp` only (no receipt patch) | pass | identical | pass | parser drift OK |
| Header RLP timestamp +1 (canonical hash) | N/A | — | — | `verify_block_header_rlp` reject |
| Corrupt witness `block_header_rlp` byte | **reject** | — | would fail if stale `block_hash` | control bind works |

## Verdict: **INV / QC** (not BC)

1. Twelve PI slots have no `timestamp` or `anWorkchain` label; fuzz/mutation leaves PI byte-identical while event fields vary.
2. Relayer binds seven event groups; `timestamp` and `an_workchain` are documented free variables (same class as `promiseCommit` at relayer).
3. Header `timestamp` is bound only through the block hash commitment (TD-32 slot 11 inside keccak), not as a standalone PI Fr.

Residual QC: L1-emitted `timestamp` is informational for indexers; cross-layer ordering or SLA claims must not rely on it being proof-bound.

## DEP-TIMESTAMP-FREE

Event `Deposit.timestamp` is not a ZK public input and is not checked by `check_binds_to`. Operators document that finalization binds amount/account/block, not L1 log timestamp word.

## Commands

    cd audit/spec/ethereum && forge test --match-path '*EventPiGap*' -q
    cd deposit-prover && cargo test td_44 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_44 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_31 -- --nocapture
