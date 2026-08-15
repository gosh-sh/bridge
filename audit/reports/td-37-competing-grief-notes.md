# TD-37 — Competing submitters grief / peer finalize-one

PoC: `td_37_competing_grief_finalize_one.rs`.  
Cross-ref: TD-26/28/65, QC-OFF-02, QC-AN-09, `f10_competing_submit.rs`.

## Flow: parked → finalize-one → cursor advance

```
Relayer A tick ──► prove fail on id 0 (HOL)
        │
        ▼
Peer finalize-one (submit good bundle for id 0)
        │
        ▼
Relayer A tick ──► AlreadyFinalized id 0 ──► cursor → 1
        │
        ▼
Relayer A tick ──► Finalized id 1
```

With `--skip-after-attempts`: poison id 0 → `Skipped` + `parked_deposit_ids`; peer finalize clears AN nullifier; relayer continues forward.

## Verdict: **QC** (not BC)

| Scenario | Mint | Funds |
|----------|------|-------|
| Competing finalize | ≤1 per depositId | safe (TD-28) |
| Peer finalize-one | unlocks HOL | ops path |
| Bad proof grief | 0 mint | prove gas waste only |

## Commands

    cd crates/deposit-relayer-daemon && cargo test td_37 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test f10_competing -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test
