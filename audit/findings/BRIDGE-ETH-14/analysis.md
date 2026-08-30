# BRIDGE-ETH-14 — `acceptOwnership` left `yieldRecipient` on the old owner

**Class:** **BC** (ops key-rotation: harvest/skim pay the compromised address)  
**Status:** **open → patched in this change** (`yieldRecipient` follows owner when it still equals the outgoing owner)  
**Area:** `AckiNackiBridge.acceptOwnership`  
**Source:** Vasya re-review of PR #39 (ETH-14, Medium, new)  
**Invariant:** AC-4 / harvest pays `yieldRecipient`; constructor sets both `owner` and `yieldRecipient` to `msg.sender`

## Summary

ETH-8 added two-step ownership. Constructor still sets `yieldRecipient = msg.sender`. `acceptOwnership` moved only `owner`. After a compromise rotation, `harvestYield` / `skimExcessUsdc` kept paying the old address with no revert.

## PoC

Gate: `cd contracts/ethereum && forge test --match-test test_acceptOwnership_movesDefaultYieldRecipient_harvestPaysNewOwner -vv`

| Before | After |
|--------|--------|
| `acceptOwnership` leaves `yieldRecipient` on the previous owner | if `yieldRecipient == previous owner`, it becomes `msg.sender` and emits `YieldRecipientSet` |
| explicit `setYieldRecipient` is overwritten | explicit recipient is left alone (`test_acceptOwnership_keepsExplicitYieldRecipient`) |

## Fix

Compare `yieldRecipient` to the outgoing `owner` *before* overwriting `owner`. Do not assume the roles always coincide after an explicit set.
