# TD-62 — Off-chain `depositId` gap vs on-chain dense counter

PoC:
- `audit/spec/ethereum/DepositCounterDense.t.sol`
- `crates/deposit-relayer-daemon/tests/td_62_deposit_id_gap.rs`

## L1 dense ids vs relayer sequential target

```
L1 chain:     depositId 0, 1, 2, ... (dense; depositCounter = n)
Relayer:      next_target = last_processed+1 (or start_deposit_id)
AN nullifier: per depositId (finalize gap ≠ missing L1 id)
```

Park/skip (TD-26, TD-37) creates **finalize gap** on AN — L1 ids still dense.

## Gap scenario matrix

| Scenario | Target | Source / chain | Tick outcome | Finalize |
|----------|--------|----------------|--------------|----------|
| (a) Only id 2 staged, start=0 | 0 | no 0/1 | `NotYetAvailable` ×2 | none (not id 2) |
| (b) last_processed=1, empty | 2 | no 2 | `NotYetAvailable` | none; cursor stays 1 |
| (c) counter hint=3, target=5 | 5 | no 5 | `NotYetAvailable` | QC: operator ahead of chain |
| (d) ids 0,1,2 staged | 0→2 | sequential | `Finalized` ×3 | [0,1,2] |

## Cross-refs

| ID | Link |
|----|------|
| TD-06 | Scan cursor — sequential visibility |
| TD-26 / TD-37 | Park/skip — finalize gap, not L1 id gap |
| DEP-5 | Monotonic `depositCounter` (`DepositCounter.t.sol`) |

## Verdict: **INV/QC**

On-chain ids dense; relayer HOL on missing lower id; no silent skip to higher id.

## Commands

    cd audit/spec/ethereum && forge test --match-path '*DepositCounter*' -q
    cd crates/deposit-relayer-daemon && cargo test td_62 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_06_scan_cursor -- --nocapture
