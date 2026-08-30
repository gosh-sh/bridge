# BRIDGE-ETH-08 — one-step `transferOwnership`

**Class:** **QC** (ops hygiene; owner key typo / compromised tx is instant and irreversible)  
**Status:** **open → patched in this change** (`pendingOwner` + `acceptOwnership`)  
**Area:** `AckiNackiBridge.transferOwnership`  
**Source:** Stage II Q&A PDF ETH-8  
**Invariant:** AC-4 — owner-only AAVE / skim / config; a mis-set owner is a full ops takeover (not a user-principal drain by itself, but harvest/skim/emergency follow the new owner)

## Summary

`transferOwnership` wrote `owner = newOwner` in the same transaction. A wrong address (or a front-run / leaked key used once) immediately hands over `supplyToAave`, `emergencyWithdrawAll`, `harvestYield`, `skimExcessUsdc`, and `setYieldRecipient`.

## PoC

Gate: `cd contracts/ethereum && forge test --match-test test_transferOwnership_flowsAllAuthorities -vv`

| Before | After |
|--------|--------|
| `transferOwnership(user2)` makes `owner == user2` immediately | nominates `pendingOwner`; `user2` cannot call `onlyOwner` until `acceptOwnership` |

## Fix

Ownable2Step-style: `transferOwnership` sets `pendingOwner` and emits `OwnershipTransferStarted`. `acceptOwnership` requires `msg.sender == pendingOwner`, then sets `owner` and clears pending. Zero address still rejected on nominate.

ETH-14: if `yieldRecipient` still equals the outgoing owner, it moves with the role. See `audit/findings/BRIDGE-ETH-14/`.

Not an OZ import (contract already has a custom `onlyOwner`). Storage: `pendingOwner` is a new slot on a non-proxy deploy.
