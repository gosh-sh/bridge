# TD-25 — Cross-function reentrancy (token callback → mutating entrypoints)

PoC: `CrossFnReentrantERC20.sol`, `DepositCrossFnReentrancy.t.sol`.  
Prior gap: `ReentrantERC20` + `DepositEdgeCases` — deposit-only reenter.

Cross-ref: **DEP-CEI-RE**, **DEP-T09**, TD-47 (deposit-only OK).

## Matrix (deposit `transferFrom` path)

Callback `msg.sender` = bridge; reenter target invoked with `msg.sender` = token contract.

| Entrypoint | Revert | State |
|------------|--------|-------|
| `deposit` | `Reentrancy()` | unchanged |
| `verifyBlock` | `Reentrancy()` | unchanged |
| `applyBkSetUpdate` | `Reentrancy()` | unchanged |
| `withdrawByProof` | `Reentrancy()` | unchanged |
| `supplyToAave` | `NotOwner()` | unchanged |
| `withdrawFromAave` | `NotOwner()` | unchanged |
| `emergencyWithdrawAll` | `NotOwner()` | unchanged |
| `harvestYield` | `NotOwner()` | unchanged |
| `skimExcessUsdc` | `NotOwner()` | unchanged |

Owner entrypoints: modifier order `onlyOwner` before `nonReentrant`; callback caller is token → auth fails closed before guard. Equivalent safety for cross-fn CEI.

## AAVE `supplyToAave` path (supplement)

During `pool.supply`, underlying `transferFrom(bridge, pool)` triggers callback (`msg.sender` = pool).

| Reenter target | Revert | State |
|----------------|--------|-------|
| `deposit` | `Reentrancy()` | unchanged |
| `verifyBlock` | `Reentrancy()` | unchanged |
| `supplyToAave` | `NotOwner()` | unchanged |

## Verdict: **OK** (not BC)

1. All nine mutating entrypoints blocked on deposit-path callback — public four via `nonReentrant`, owner five via `NotOwner`.
2. Treasury / `depositCounter` / nullifier / `suppliedPrincipal` unchanged on every attempt.
3. Gap «deposit-only» closed — matrix PoC supersedes `ReentrantERC20` scope.

## Commands

    cd audit/spec/ethereum && forge test --match-path '*CrossFnReentrancy*' -q
    cd audit/spec/ethereum && forge test --match-path '*DepositEdgeCases*' -q
