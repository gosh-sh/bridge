# TD-67 — Competing provers: same L1 event, different `dapp_id` PI

PoC: `crates/deposit-relayer-daemon/tests/td_67_competing_provers_dapp_id.rs`.

Shared fixture: `deposit_id = 0` (one L1 `Deposit` event).

## Flow

```
L1 Deposit (id=0)
        │
        ├─ Prover namespace A (dappId=A in PI) ──► AN gate expects A ──► mint ✓
        │
        └─ Prover namespace B (dappId=B in PI) ──► AN gate expects A ──► ERR_WRONG_DAPP (223)
```

Relayer path (c): after A mints, B may `AlreadyFinalized` via `is_finalized` pre-check (wrong PI never submitted). Direct submit with wrong PI → `ERR_WRONG_DAPP` (case b).

## Outcome table

| Case | Prover dapp | AN expects | Outcome | finalized_count |
|------|-------------|------------|---------|-----------------|
| (a) | A | A | Finalized | 1 |
| (b) | B (after A minted) | A | Rejected 223 | 1 |
| (c) relayer B tick | B | A | `AnRejected` **or** `AlreadyFinalized` (nullifier pre-check) | 1 |
| (d) relayer B matching | A (after A minted) | A | `AlreadyFinalized` | 1 |

## Cross-refs

| ID | Link |
|----|------|
| BC-AN-01 | `test_bc_an_01_dapp_id_double_mint.py` — replay anchor includes dappId |
| TD-05 | `td_05_dapp_id_injection.rs` — PI injection / ERR_WRONG_DAPP |
| TD-37 | `td_37_competing_grief_finalize_one.rs` — `AlreadyFinalized` nullifier |

## Verdict: **META/QC**

Post BC-AN-01 fix: one event cannot double-mint across dapp namespaces; wrong PI rejected at AN gate.

## Commands

    cd crates/deposit-relayer-daemon && cargo test td_67 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_05 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_37_competing_grief_finalize_one -- --nocapture
