# TD-55 — Storage diff: failed deposit does not move counter/treasury/custody

PoC: `audit/spec/ethereum/DepositStorageDiff.t.sol`.

## Failure mode → revert → ledger delta

| Failure mode | Revert / outcome | counter Δ | treasury Δ | custody Δ |
|--------------|------------------|-----------|------------|-----------|
| Insufficient allowance | ERC20 `transferFrom` fail | 0 | 0 | 0 |
| Insufficient balance | ERC20 `transferFrom` fail | 0 | 0 | 0 |
| Blacklisted sender (TD-24) | `BlacklistableERC20.Blacklisted` | 0 | 0 | 0 |
| Paused token (TD-24) | `BlacklistableERC20.Paused` | 0 | 0 | 0 |
| Returnless token (TD-49) | `SafeERC20` / transfer fail | 0 | 0 | 0 |
| Reentrant `deposit` (TD-25) | `AckiNackiBridge.Reentrancy` | 0 | 0 | 0 |
| Success control | — | +1 | +amount | +amount |

Helper: `snapshotBridgeLedger(bridge, token)` → `(counter, treasury, custody)`.

## Cross-refs

| ID | Link |
|----|------|
| TD-49 | `DepositMutationKill.t.sol` — counter/treasury freeze mutants |
| TD-24 | `DepositUsdcBlacklistPause.t.sol` — blacklist/pause |
| TD-25 | `DepositCrossFnReentrancy.t.sol` — `CrossFnReentrantERC20` |
| DEP-5 | `depositCounter` monotonicity on success only |

## Verdict: **META/OK**

All fail-closed deposit paths in PoC leave bridge ledger and custody unchanged; success path moves all three slots.

## Commands

    cd audit/spec/ethereum && forge test --match-path '*StorageDiff*' -q
    cd audit/spec/ethereum && forge test --match-path '*MutationKill*' -q
    cd audit/spec/ethereum && forge test --match-path '*UsdcBlacklistPause*' -q
