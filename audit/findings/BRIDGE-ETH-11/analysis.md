# BRIDGE-ETH-11 — fee-on-transfer / rebase desyncs `treasuryBalance`

**Class:** **QC** (token hygiene; production USDC is exact-credit)  
**Status:** **patched** (`TransferAmountMismatch` on inexact custody delta)  
**Area:** `AckiNackiBridge.deposit`, `withdrawByProof`  
**Source:** sauin re-review of PR #39 (ETH-11, Low, partial)  
**Invariant:** TR-4 — after a successful `deposit(amount)`, `Δusdc.balanceOf(bridge) == amount`

## Summary

`treasuryBalance += amount` assumed `transferFrom` moved exactly `amount`. A fee-on-transfer or rebasing `_usdc` (free constructor argument) would overstate the ledger. `approve` bool was already checked (ETH-9).

`altTokenId` stays script-level on `chainid == 1` (owner 2026-09-08). This finding is only the custody delta.

## Fix

`_pullExactUsdc` / `_pushExactUsdc`: snapshot `balanceOf` around the ERC-20 call; revert if the delta is not `amount`. Does **not** credit net-of-fee (no FoT support).

## PoC

| Test | What it shows |
|------|----------------|
| `test_feeOnTransferToken_reverts` | 10% FoT → `TransferAmountMismatch`; treasury 0 |
| `DepositFoTInvariant.t.sol` | stateful FoT attempts cannot credit the ledger |
