# BRIDGE-ETH-15 — flat `_isKnownAnchor` dropped on-chain layer identity

**Class:** **QC** (trust-boundary move, not a confirmed exploit)  
**Status:** **closed as documented property** — Option A (Circuit 4 `anchorLayer` PI) remains the re-keygen target  
**Area:** `AckiNackiBridge._isKnownAnchor`, Circuit 4 `PUB_FINAL_ROOT`  
**Source:** Vasya re-review of PR #39 (ETH-15, Medium, new)  
**Invariant:** WD-6 / NB-Q1 — every accepted `finalRoot` was written by `verifyBlock`

## Summary

The contract comment already said the flat scan no longer asserts which layer a withdrawal is anchored in. The re-review asked that the Circuit 4 property which *replaces* that check be written down and included in circuit-audit scope.

No Solidity exploit was found: forging an anchor still requires a verified `verifyBlock` slot. The widened set is “any of ten windows” instead of one.

## Disposition

Written property: `docs/audit/circuit4-anchor-binding.md`.  
Option A (PI slot `anchorLayer` + `_isKnownLayerAnchor`) is not this change — it needs Circuit 4 re-keygen. Owner 2026-09-08: that re-keygen remains the target; this written property is the interim answer. It also closes ETH-09 when it lands.

## PoC

No new falling Solidity test (the widening is intentional NB-Q1). Translation-layer re-prove: `reprove_against_later_layer_hash_keeps_nullifier` in `bridge-event-prover-lib`.
