# BRIDGE-ETH-02 — congruent layer hash poisons the window

**Class:** **BC** (High — freeze of `verifyBlock` / AN→ETH; no direct second payout)  
**Status:** **patched** (`FieldElementOutOfRange` on each active `layerHashes[i]`, `prevMaxLevelLayerHash`, and `blockId`; `applyBkSetUpdate` also gates `blockId` and `newCommitmentL3`)  
**Area:** `AckiNackiBridge.verifyBlock`, `LayerHashesAggregatorVerifier`, Halo2 Yul `mod(calldataload, f_q)`  
**Source:** Stage II Q&A PDF ETH-2  
**Invariant:** chain head in `_layerWindows` is the canonical Fr the honest prover will re-use as `prevMaxLevelLayerHash`

## Summary

Same reduction mismatch as BRIDGE-ETH-01, on Circuit 2 layer hashes.

Yul (`contracts/ethereum/verifiers/LayerHashesAggregatorVerifier.sol`) stores `mod(calldataload(off), f_q)` for every instance, including `layerHashes[0..9]` and `prevMaxLevelLayerHash`. The adapter compares raw words. `_appendLayer` writes the raw word into the 128-slot window.

Front-run: take an honest `verifyBlock` for hash `H`, submit `H + k·R` in the last active slot (instance + `layerHashes[last]`). Pairing accepts. Window head becomes `H+R`. The honest relayer's next block carries `prevMax = H` → `PrevAnchorMismatch`. If this is the first block of a height, honest `H` never lands; `withdrawByProof` proofs bound to `H` revert `UnknownAnchor`.

This is a liveness halt, not a second mint. Combined with ETH-1 it would also let a withdrawal `finalRoot = H+R` match a poisoned window.

## PoC

Gate: `cd contracts/ethereum && forge test --match-contract AckiNackiBridgeEthFieldCongruence -vv`

| Test | What it shows |
|------|----------------|
| `test_eth2_verifyBlock_layerHashPlusR_rejected` | Mock Circuit 2 (accepts anything): `H+R` must revert `FieldElementOutOfRange`; honest `H` and `H+R` stay out of the window; genesis head unchanged. |
| `test_eth2_verifyBlock_prevMaxPlusR_rejected` | Same gate on `prevMaxLevelLayerHash`. |
| `test_eth2_verifyBlock_blockIdPlusR_rejected` | Re-review ETH-02 remainder: unreduced `blockId` cannot enter `verifyBlock` (event-log poison). |
| `test_applyBkSetUpdate_rejectsUnreducedRoot` | Unreduced `blockId` on BK rotation → `FieldElementOutOfRange`. |
| `test_applyBkSetUpdate_rejectsUnreducedNewCommitment` | Unreduced `newCommitmentL3` cannot land in `storedBkSetCommitment`. |
| ETH-1 `test_eth1_productionYul_acceptsNullifierPlusR` | Empirical: this repo's Yul family does **not** range-check unreduced instances — it mods. Circuit 2 source has the same `mod(calldataload, f_q)` preamble. |

Circuit 2 committed `.bin` / `_calldata.bin` do **not** verify against each other even canonically (Stage II ETH-6). Pairing for `H+R` is therefore not reproduced on that artefact pair; the contract gate does not depend on it.

## Fix

`_requireCanonicalFr` on every active `layerHashes[i]`, `prevMaxLevelLayerHash`, and `blockId` in `verifyBlock`. Same gate on `blockId` and `newCommitmentL3` in `applyBkSetUpdate` so stored field elements are canonical Fr.

## Notes

- Distinct from A3-01 (128-slot eviction of an *honest* canonical head). ETH-2 plants a *non-canonical* head.
- Wave 3 should still hash-pin Circuit 2 `.bin` ↔ calldata so a production pairing PoC can be added.
