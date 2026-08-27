# BRIDGE-ETH-07 — leftover aUSDC after emergency is harvestable yield

**Class:** **BC** (owner can pay leftover principal to `yieldRecipient` via `harvestYield`)  
**Status:** **open → patched in this change** (`EmergencyLeftoverAToken`)  
**Area:** `AckiNackiBridge.emergencyWithdrawAll`, `harvestYield`, `accruedYield`  
**Source:** Stage II Q&A PDF ETH-7; related QC-A1-3 / A1-F5  
**Invariant:** A4-INV-3 / TR-3 — owner paths must not extract user principal; `accruedYield` is aUSDC above `suppliedPrincipal`

## Summary

`emergencyWithdrawAll` called `aavePool.withdraw(..., type(uint256).max)` then **always** set `suppliedPrincipal = 0`. `accruedYield()` is `aUSDC.balanceOf(bridge) - suppliedPrincipal`. If the pool under-redeems (pause, rounding, malicious/buggy pool) leftover aTokens remain. With principal booked at zero, that leftover **is** harvestable yield. `harvestYield` then withdraws it to `yieldRecipient`.

QC-A1-3 / `skimExcessUsdc` already covers yield that *did* land as liquid USDC. ETH-7 is the complementary hole: leftover **aToken** shares.

Honest AAVE V3 `withdraw(max)` usually burns all aTokens. The hole is still a real accounting bug whenever redeem is incomplete: the contract must not pretend the aToken book is empty.

## PoC

Gate: `cd contracts/ethereum && forge test --match-test test_eth7_emergencyLeftoverAToken -vv`

Mock pool (`setLeftoverOnMaxWithdraw`) redeems `balance - leftover` on `withdraw(max)`.

| Before the gate | After |
|-----------------|--------|
| emergency succeeds; `suppliedPrincipal = 0`; leftover aUSDC = harvestable | `EmergencyLeftoverAToken(leftover)`; principal book and `aaveEnabled` unchanged (tx reverts) |

## Fix

After `withdraw(max)`, if `aUsdcBalance() != 0` revert. Do not zero `suppliedPrincipal`. Disable AAVE only on a clean drain.

Liquid post-emergency yield remains `skimExcessUsdc` (QC-A1-3). Do not treat leftover aTokens as yield.
