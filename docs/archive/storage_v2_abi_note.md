> **⚠️ ARCHIVED 2026-08-18 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/ETH-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

# Storage v2.0 ABI note — batched decisions for NB-Q2 / Q3 / Q4

**Status:** design decision, unimplemented.
**Batching:** Sergey's cross-cutting reply asked for a single tracking issue for NB-Q2/3/4 (`sergey_answer_27_07_2026.md:21`). This doc is that issue in written form.
**Scope:** `contracts/ethereum/src/AckiNackiBridge.sol` storage layout + external view surface. Circuits and off-chain callers are unaffected until the on-chain change actually lands.
**Prereq:** everything here costs zero halo2 keygen cycles (`sergey_answer_27_07_2026.md:21`), so it is independent of the Circuit 4 SHPLONK re-keygen that gates Option A of NB-Q1.

## Question recap

Full text: `new_bridge_questions_to_sergey_from_alina_27_07_2026.md:30-79`. Short form of each and Sergey's ruling from `sergey_answer_27_07_2026.md:13-14`.

### NB-Q2 — `storedLayerHashes` overwrite

`verifyBlock` currently runs (post-NB-Q1 the line numbers shifted by 2; the block itself is unchanged, `AckiNackiBridge.sol:752-756`):

```solidity
storedNumLayers = numLayers;
for (uint256 i = 0; i < MAX_LAYER_HASHES; i++) {
    storedLayerHashes[i] = layerHashes[i];
}
```

A shallow successor (`numLayers=1` after `numLayers=3`) zeroes slots `[1..9]` of `storedLayerHashes`. The per-layer anchor pick in `_expectedPrevAnchor` sources from `_layerWindows` and is unaffected — the flat cache only leaks stale zeros through `getStoredLayerHashes()`.

**Sergey's ruling:** intentional; document it (option 2). Explicitly *not* option 3 (`for (i = 0; i < numLayers; i++)`), because that would yield a row of slots from different heights with nothing recording which height each came from — Sergey calls this "the NB-Q4 drift". Instead: add `getLatestPerLayer()` off `_layerWindows`.

### NB-Q3 — `storedPrevMaxLevelLayerHash` hot-path SSTORE

Read-side users are: (a) genesis bootstrap seed in `_expectedPrevAnchor` (`AckiNackiBridge.sol:937-939`), a one-shot use before the first `_appendLayer`; (b) the public getter. Every subsequent `verifyBlock` pays the SSTORE for a value no on-chain logic will ever read again.

**Sergey's ruling:** agreed — make it an immutable genesis seed, deprecate the getter and repoint it. Batch with NB-Q2.

### NB-Q4 — dual per-layer caches

`_layerWindows[L]` is now authoritative for anchoring; `storedLayerHashes` is a legacy view. This invites silent drift: any future change to `_appendLayer` semantics will not be reflected in the flat cache, and vice versa.

**Sergey's ruling (implicit, via the NB-Q2 dispatch):** the fix is `getLatestPerLayer()` off `_layerWindows`, replacing `storedLayerHashes` as the per-layer view. Land in the same v2.0 sweep as NB-Q2/Q3.

## Storage v2.0 change

Landed as a single upgrade. `storage_v1_shim = false` for greenfield deploys; no compat shim on live Sepolia — the writes are eliminated, not renamed, so `slot(storedPrevMaxLevelLayerHash)` reads as its immutable genesis value forever, which is a safe read for any pre-v2.0 indexer that stored the field.

### Writes eliminated on the hot path

`verifyBlock` (`AckiNackiBridge.sol:752-762`) drops both the flat-loop cache overwrite and the informational SSTORE. Only the per-layer `_layerWindows[L].append(...)` writes remain (already the authoritative source).

Concretely:

```solidity
// v1 hot path (current):
storedNumLayers = numLayers;
for (uint256 i = 0; i < MAX_LAYER_HASHES; i++) {
    storedLayerHashes[i] = layerHashes[i];        // ← 10 SSTOREs, ≥9 wasted
}
storedPrevMaxLevelLayerHash = layerHashes[numLayers - 1];  // ← 1 SSTORE, never read on-chain after the next _appendLayer

// v2 hot path (target):
// (no writes here at all — `_appendLayer(L, layerHashes[L-1], seqNo)`
//  in the per-layer loop is the only writer that survives)
```

### Storage slots

| Slot | v1 | v2 | Notes |
|---|---|---|---|
| `storedNumLayers` (`uint8`) | mutable, overwritten each block | **removed** | Read as `_highestActiveLayer()` when needed. |
| `storedLayerHashes[10]` (`uint256[10]`) | mutable, flat-overwritten each block | **removed** | Replaced by `getLatestPerLayer()` view over `_layerWindows`. |
| `storedPrevMaxLevelLayerHash` (`uint256`) | mutable, hot-path SSTORE + genesis seed | **immutable** (constructor-only) | Retains the value the constructor received as `genesisPrevMaxLevelLayerHash` (`AckiNackiBridge.sol:557`); never mutated post-deploy. |
| `_genesisPrevMaxLevelLayerHash` (`uint256 immutable`) | absent | **new alias**, set in constructor | Optional — if there is any risk of a Solidity `immutable` slot layout collision with the existing `storedPrevMaxLevelLayerHash`, introduce a fresh `immutable` and repoint the public getter. Prefer keeping the same public name to preserve ABI. |

Storage bytes freed per hot-path invocation: **11 SSTOREs → 0 SSTOREs** (10 `storedLayerHashes` slots + 1 `storedPrevMaxLevelLayerHash`, all zeroed out of the hot path). Rough gas: 10 × 2900 (dirty write) + 2900 = ~31.9 k gas returned to every `verifyBlock` on the happy path.

### External view surface

| View | v1 semantics | v2 semantics | ABI |
|---|---|---|---|
| `storedNumLayers()` | last block's numLayers | **removed**; use `latestPerLayer().length` or check window emptiness | breaking |
| `storedLayerHashes(uint256)` | last block's flat cache, index-addressed | **removed** | breaking |
| `getStoredLayerHashes()` | last block's flat cache, `uint256[10]` | **removed** | breaking; replaced by `getLatestPerLayer()` |
| **new** `getLatestPerLayer()` | — | `uint256[MAX_LAYER_HASHES] memory` where entry `[L-1]` is `_layerWindows[L].data[(writeCursor - 1) mod HISTORY_PROOF_WINDOW]` (zero if the window is empty) | additive |
| `storedPrevMaxLevelLayerHash()` | last block's max-level layer hash **plus** genesis seed | **genesis seed only** (immutable) — NatSpec marked `deprecated`, points readers at `getLatestPerLayer()` for per-block max-layer values | semantic change |
| `expectedPrevAnchor(uint8)` | unchanged | unchanged | — |

Concrete `getLatestPerLayer()`:

```solidity
/// @notice Latest anchor written into each layer window. Entry `[L-1]`
///         is the head of `_layerWindows[L]`, i.e. the most recent
///         Poseidon Merkle root committed for layer `L` across all
///         `verifyBlock` calls so far — *not* just the last block's
///         array. Empty windows return zero.
///
///         This replaces `getStoredLayerHashes()` (removed in v2.0). The
///         per-layer view over `_layerWindows` is the authoritative
///         source; a shallow successor block no longer overwrites deeper
///         layers with zero.
function getLatestPerLayer() external view returns (uint256[MAX_LAYER_HASHES] memory) {
    uint256[MAX_LAYER_HASHES] memory out;
    for (uint8 L = 1; L <= MAX_LAYER_HASHES; L++) {
        HistoryWindow storage w = _layerWindows[L];
        if (w.dataLen > 0) {
            uint16 head = (w.writeCursor + HISTORY_PROOF_WINDOW - 1)
                          % uint16(HISTORY_PROOF_WINDOW);
            out[L - 1] = w.data[head];
        }
    }
    return out;
}
```

Same shape as the existing `getStoredLayerHashes()` (`AckiNackiBridge.sol:973-978`) so indexers can drop-in swap the getter name and index base (0-indexed `[L-1]` matches the current array layout).

## Off-chain read impact

Off-chain callers of the removed getters need a migration path *before* the v2.0 deploy. Grep hits (2026-08-04):

- Rust bindings: `crates/eth-frontend/src/contract.rs` regenerates `abigen!` from the ABI — will re-emit against the new surface at rebuild, no manual edits.
- Foundry tests: `getStoredLayerHashes` used in `AckiNackiBridgeVerifyBlock.t.sol`, `AckiNackiBridgeRelayerLoop.t.sol` — assertion helpers migrate to `getLatestPerLayer` with the semantic-change caveat (deep-first-shallow-next no longer zeroes tail slots).
- Relayer / indexer consumers (none in this tree today; flag for downstream teams before deploy).

The semantic change is deliberate and correct: v2.0 exposes per-layer *state*, not per-block *snapshot*. Any consumer that actually wanted the "last block's numLayers-only array" behaviour was already at risk of the NB-Q2 zero-leak.

## Deploy / migration checklist

1. Land the code change in one PR (references this note).
2. Update NatSpec on the surviving `storedPrevMaxLevelLayerHash` public var: mark `@dev deprecated in v2.0: this is now the immutable genesis seed; use `getLatestPerLayer()` for per-block max-layer values`.
3. Regenerate Foundry `verifyBlock` invariant tests to assert:
   - `getLatestPerLayer()[L-1]` monotonically tracks the last non-zero anchor per layer across a sequence of mixed-depth blocks.
   - `storedPrevMaxLevelLayerHash()` returns the constructor value forever.
   - No hot-path storage write to slot(s) formerly occupied by `storedLayerHashes` / `storedNumLayers` (bytecode-level assertion via `vm.load` / snapshot diff over `verifyBlock`).
4. Deploy fresh bridge (v2.0 is not a hot upgrade — the storage layout change is not compatible with in-place upgrade even under a proxy without a storage-slot renumbering shim; a fresh deploy is the intended path since the AN→ETH bridge is a fresh-contract-per-deploy model per the existing shellnet / mainnet cadence).
5. Off-chain: point indexers / relayers at `getLatestPerLayer()`. `eth-frontend` `abigen!` regenerates automatically.

## Non-goals

- **NB-Q1 (`WITHDRAW_ANCHOR_LAYER`)** — already landed via commit `09c1686` (flat `_isKnownAnchor`). Independent of storage v2.0.
- **Circuit 4 PI extension (Option A)** — separate deliverable, gated on partner SHPLONK re-keygen. Storage v2.0 does not depend on it.
- **`_layerWindows` internal shape** — untouched. v2.0 only removes the redundant flat cache; the authoritative per-layer rolling window is already correct.

## References

- Question source: `new_bridge_questions_to_sergey_from_alina_27_07_2026.md:30-79`
- Sergey's rulings: `sergey_answer_27_07_2026.md:13-14` (NB-Q2/Q3/Q4) + `:21` (batching directive)
- Current storage layout: `contracts/ethereum/src/AckiNackiBridge.sol:180-197`
- Current hot-path writes: `contracts/ethereum/src/AckiNackiBridge.sol:752-762`
- Existing anchor authority: `_layerWindows` / `_expectedPrevAnchor` (`AckiNackiBridge.sol:258, 927-943`)
- Related landed work: NB-Q9 crate rename (`061f69e`), NB-Q1 flat anchor scan (`09c1686`), aggregator_cache content-address (`183c95c`).
