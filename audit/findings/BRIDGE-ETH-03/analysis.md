# BRIDGE-ETH-03 — 128-window eviction and seq_no jump

**Class:** **QC** (liveness, not fund loss) — known A3-01 / WD-Q1  
**Status:** **closed (documented + tests + occupancy view)** — no window resize, no on-chain seq_no cap  
**Area:** `AckiNackiBridge` `_layerWindows`, `withdrawByProof` `UnknownAnchor`, `blockSeqNo`  
**Source:** Stage II Q&A PDF ETH-3

## Summary

The per-layer ring is 128 **successful `verifyBlock` appends**, not 128 `blockSeqNo`. An evicted `finalRoot` makes `withdrawByProof` revert `UnknownAnchor`; treasury is unchanged.

A large `blockSeqNo` jump does **not** skip extra window slots. One call writes one hash. Fast-forward is intentional catch-up (relayer policy remains `last_seen+1` off-chain). An on-chain jump cap would brick recovery after downtime; Circuit 1A/1B already bind `seq_no` to a real attested block.

Re-prove: the contract accepts any still-in-window `finalRoot`. Circuit 4 must produce a new proof of the same event against that root. Partner dense chain is `MAX_CHAIN_LEN = 11` rungs — pick a remaining window entry within that hop bound.

## PoC

`audit/spec/ethereum/WithdrawAnchorEviction.t.sol`

| Test | What it shows |
|------|----------------|
| `test_withdrawByProof_evictedAnchor_reverts` | Original A3-01: 129th append evicts the first L1 hash. |
| `test_seqNoFastForward_doesNotEvictEarlierAnchor` | Jump `seq_no` to 1e6; first L1 still known. |
| `test_reproveAgainstLaterInWindowAnchor_succeeds` | After eviction, payout against a later in-window L1 (mock Circuit 4). |
| `layerWindowLen(layer)` | Occupancy `0..=128` for the withdraw-relayer alert. |
| `reprove_against_later_layer_hash_keeps_nullifier` | Same event, later `layer_hash_hex` → new `final_root`, same nullifier (`bridge-event-prover-lib`). |

## Disposition

- SLA + alert recipe in `docs/audit/eth-qc-hardening-runbook.md` § WD-Q1 (`layerWindowLen`).
- NatSpec on `HISTORY_PROOF_WINDOW` and the `blockSeqNo` check.
- No Solidity cap on seq_no delta. No window enlargement this sprint (would be a circuit/storage change).
- Full Halo2 re-prove of a Poseidon-consistent descendant remains the partner circuits MockProver / n14 SHPLONK job; the translation layer and the contract path are pinned here.
