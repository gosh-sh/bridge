# bridge-EVM — Follow-up Questions (2026-07-27)

**To:** Pruvendo / Sergey Egorov
**From:** Alina

## NB-Q1 — `WITHDRAW_ANCHOR_LAYER = 1` still hardcoded 

**State.** `AckiNackiBridge.sol:74-75, 1040-1044`:

```solidity
uint8 internal constant WITHDRAW_ANCHOR_LAYER = 1;
// ...
// L1-only witness builder (`layer_idx = 0`). Future: read `anchorLayer`
// from Circuit 4 public input slot [10] when partner extends the layout.
if (!_isKnownLayerAnchor(WITHDRAW_ANCHOR_LAYER, pub.finalRoot)) {
    revert UnknownAnchor(pub.finalRoot);
}
```

Partner side is preparing witnesses anchored at `layer_idx ∈ 1..MAX_LAYER_HASHES` (multi-thread L(N)-tree work). The moment those ship, `withdrawByProof` will `revert UnknownAnchor` for every valid L≥2 proof because the anchor scan is pinned to L1.

**Questions.**

1. Is Option A (Circuit 4 grows PI slot [10] `anchorLayer`, bridge range-checks `1..MAX_LAYER_HASHES` and scans `_layerWindows[pub.anchorLayer]`) still the target? The comment at `:1040` implies yes but there is no tracking issue.
2. Any appetite for a transitional Option D (flat scan over all 10 windows via existing `_isKnownAnchor`) guarded by a feature flag, so partner is not forced to hold L≥2 witness shipping until the Shplonk re-keygen of Circuit 4 lands? 5-line change, revertible.
3. Confirm this gates on the Circuit 4 layout freeze, i.e. it is a single re-aggregator-export away rather than an independent PR.

---

## NB-Q2 — `storedLayerHashes` still overwritten unconditionally across all 10 slots 

**State.** `verifyBlock` still runs (`AckiNackiBridge.sol:727-730`):

```solidity
storedNumLayers = numLayers;
for (uint256 i = 0; i < MAX_LAYER_HASHES; i++) {
    storedLayerHashes[i] = layerHashes[i];
}
```

So a shallow successor (e.g. `numLayers=1` after a `numLayers=3` block) still zeroes slots `[1..9]` of `storedLayerHashes`. The per-layer anchor pick in `_expectedPrevAnchor` sources from `_layerWindows` and is unaffected — but the flat cache still leaks stale zeros through `getStoredLayerHashes()`.

**Questions.**

1. Is the flat overwrite intentional (kept only for the legacy "last block's array" view) or an oversight now that the per-layer windows are the truth?
2. If intentional: please document in the NatSpec that `getStoredLayerHashes()[i]` returns zero for `i >= storedNumLayers` of the *last* block (not the last-known per-layer value). Off-chain readers already exist and this rename would signal it clearly.
3. If oversight: switch the loop to `for (uint8 i = 0; i < numLayers; i++)` and leave higher slots at their previous per-layer value. This is ABI-visible for `getStoredLayerHashes()` — worth calling out in a minor-version note.

---

## NB-Q3 — `storedPrevMaxLevelLayerHash` retained as an "informational" SSTORE 

**State.** `AckiNackiBridge.sol:731-737`:

```solidity
// Record this block's max-level layer hash. This is now informational
// only (the per-layer windows below are the anchor source — see
// `_expectedPrevAnchor` / AB-Q4); kept for backward-compatible reads.
storedPrevMaxLevelLayerHash = layerHashes[numLayers - 1];
```

Read-side users are now:
- genesis bootstrap seed in `_expectedPrevAnchor` (`if (t == 0) return storedPrevMaxLevelLayerHash;`) — a **one-shot** use before the first `_appendLayer`;
- the public getter.

Every subsequent `verifyBlock` pays the SSTORE for a value that no on-chain logic will ever read again.

**Questions.**

1. Can this be split: keep a separate `_genesisPrevMaxLevelLayerHash` immutable seed (set in the constructor / initializer) so the SSTORE on the hot path goes away, and let the public getter return `_layerLatest(_highestActiveLayer())` instead?
2. If Pruvendo wants to keep the field for indexer ABI stability, mark it `deprecated` in NatSpec and stop writing to it — the last-write timestamp would still be correct as of the genesis seed.

---

## NB-Q4 — Two per-layer caches (flat `storedLayerHashes` + `_layerWindows`) coexist

`_layerWindows[L]` is now authoritative for anchoring; `storedLayerHashes` is a legacy view. This invites silent drift: any future change to `_appendLayer` semantics (e.g. skipping duplicates) will not be reflected in the flat cache, and vice versa.

**Question.** Is the plan to delete `storedLayerHashes` + `storedNumLayers` at v2.0 and expose a `getLatestPerLayer()` view backed by `_layerWindows[L].data[writeCursor-1]`? Same intent as NB-Q2 (3) and NB-Q3 (1); asking whether they should all land in one v2.0 sweep or piecemeal.

---

## NB-Q5 — Residual Groth16/gnark NatSpec + deploy-log labels (interfaces + `DeployRealBridge`)

All Groth16 adapter contracts and generated verifiers for 1A/2/4 are gone from `contracts/ethereum/src/` — nothing to delete anymore. But the *interface* NatSpec and one deploy script still describe the proof bytes as `gnark Groth16`, which is wrong now that every wired backend is Shplonk (and 1B specifically was retired on `e2a962b`).

**Sites (verified 2026-07-27):**

| File | Lines | Issue |
|---|---|---|
| `src/IPrimaryVerifier.sol` | 21 | "256-byte Groth16 proof (8 × uint256, gnark MarshalSolidity layout)" |
| `src/IFallbackVerifier.sol` | 18 | same |
| `src/ILayerHashesMovementVerifier.sol` | 29, 35 | "Groth16 proof" / "reverts in the underlying gnark" |
| `src/IBridgeWithdrawalVerifier.sol` | 23, 25, 28, 60, 62 | describes struct as mirroring gnark PI layout and references `IBridgeWithdrawalGroth16Verifier` — a file that no longer exists |
| `script/DeployRealBridge.s.sol` | 17 | `@dev … gnark Groth16 for 1B fallback` — 1B is now Shplonk |
| `script/DeployRealBridge.s.sol` | 193 | `console.log("FallbackVerifier (Groth16):", …)` — operator-visible label, wrong |

Test-side mentions (`ShplonkAggregatorForgery.t.sol`, `MockPrimaryVerifier.sol`, `MockBridgeWithdrawalVerifier.sol`, `AckiNackiBridgeWithdrawByProof.t.sol`, `FuzzVerifiers.t.sol`, `verifiers/README.md`) are legitimate historical context / forgery fixtures — flagging so they're deliberately excluded from the sweep.

**Questions.**

1. OK to sweep the six sites above to "SHPLONK proof bytes (Halo2 KZG aggregator calldata)" and drop the stale `IBridgeWithdrawalGroth16Verifier` cross-reference?
2. Reaffirm policy: no Groth16 adapter will be wired into a live `AckiNackiBridge` constructor on any network including Sepolia, correct?

---

## Cross-cutting

Is there a single tracking issue that batches NB-Q2/3/4 so they land together with one ABI-break note? Would prefer one migration event over three.
