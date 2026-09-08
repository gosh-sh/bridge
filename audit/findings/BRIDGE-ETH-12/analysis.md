# BRIDGE-ETH-12 — zero `newCommitmentL3` + rotation vs layer cursor

**Class:** **QC** (BK-set rotation hygiene)  
**Status:** **partial → zero check patched**; ordering remains a relayer rule  
**Area:** `AckiNackiBridge.applyBkSetUpdate`  
**Source:** sauin re-review of PR #39 (ETH-12, Low, partial)  
**Invariant:** BK-6 — stored commitment is a real Poseidon BK-set root, not the Fr-zero that `_requireCanonicalFr` still accepts

## Summary

Canonical-Fr on `blockId` / `newCommitmentL3` landed with ETH-02. Zero is in-range, so a rotation could store `storedBkSetCommitment = 0` and force every later `verifyBlock` to attest a zero commitment.

The two cursors (`storedLastBkSetUpdateSeqNo`, `storedLastSeenBlockSeqNo`) still move independently — that is an off-chain ordering constraint, not an on-chain lock.

## Fix

`applyBkSetUpdate` reverts `ZeroBkSetCommitment` if `newCommitmentL3 == 0`. Test: `test_applyBkSetUpdate_rejectsZeroNewCommitment`.

Ordering: `docs/audit/eth-qc-hardening-runbook.md` (QC-A2-2 / ETH-12) — rotate only after `verifyBlock` has landed that seq_no, or fall-forward under the new commitment.
