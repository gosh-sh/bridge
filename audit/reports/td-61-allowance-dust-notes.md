# TD-61 — Allowance dust / exact approve edge cases (L1 INV)

PoC: `audit/spec/ethereum/DepositAllowanceDust.t.sol`.

`deposit()` pulls **exact** `amount` via `transferFrom(msg.sender, bridge, amount)` — no partial pull, no allowance bookkeeping in bridge.

## Approve vs amount → outcome

| Case | approve | deposit | Outcome | allowance after | counter / treasury |
|------|---------|---------|---------|-----------------|-------------------|
| (a) exact | `amount` | `amount` | Ok | **0** | +1 / +amount |
| (b) surplus | `amount + S` | `amount` | Ok | **S** | +1 / +amount |
| (c) exhausted | `amount` (once) | `amount` ×2 | 2nd **revert** | 0 | frozen after 1st |
| (d) max | `type(uint256).max` | `amount` ×2 | both Ok | **max** (no decay) | +2 / +2×amount |
| (e) short | `amount - 1` | `amount` | **revert** | `amount - 1` | 0 / 0 (TD-55) |
| (f) dust | `amount + 1` | `amount` then `1` | both Ok | 0 | +2 / +amount+1 |

MockERC20 semantics (f): remainder allowance **1** base unit (1e-6 USDC); micro-deposit `amount=1` succeeds (TD-33 min deposit). Real USDC: same ERC20 allowance decay.

## Cross-refs

| ID | Link |
|----|------|
| TD-55 | `DepositStorageDiff.t.sol` — failed `transferFrom` leaves ledger frozen (case e) |
| TR-1 | Exact `transferFrom` amount; bridge does not track allowance |
| TD-33 | Min deposit `amount=1` (dust spam path) |

## Verdict: **INV/OK**

Standard ERC20 allowance rules; bridge ledger only moves on successful exact pull.

## Commands

    cd audit/spec/ethereum && forge test --match-path '*AllowanceDust*' -q
    cd audit/spec/ethereum && forge test --match-path '*StorageDiff*' -q
    cd audit/spec/ethereum && forge test --match-path '*MutationKill*' -q
