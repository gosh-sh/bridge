# bridge-EVM — Review Questions

**To:** Pruvendo / Sergey Egorov  
**From:** Alina (AN-side circuits + prover)  
**Scope:** `bridge` review from the AN→ETH side.

# Round 2 (2026-06-25) — `contracts/ethereum/src/AckiNackiBridge.sol`

**Scope:** Code-review questions on `AckiNackiBridge.sol` after the 06-21…06-23 Shplonk-only landing on `pruvendo/shellnet-e2e-landing`. Companion analysis: `bridge_updates_analysis_2026-06-25.md`.

## AB-Q1 — `anchorsRecorded`: what is the practical use of a cross-layer monotonic counter?

**State.** `AckiNackiBridge.sol:246`:

```solidity
/// @notice Monotonic counter of layer anchors appended by `verifyBlock`
///         (never reset; useful for off-chain tooling).
uint256 public anchorsRecorded;
```

Incremented once per `_appendLayer` call inside `_appendLayerHashes` (line 827–828, `unchecked { anchorsRecorded = anchorsRecorded + 1; }`) — so it grows by `numLayers` (1..10) per `verifyBlock`. Consumed only by:

- `AnchorRecorded(anchor, anchorsRecorded)` event (line 830)
- `LayerAnchorAppended(layer, hashValue, blockHeight, anchorsRecorded)` event (line 831)
- Foundry tests (`AckiNackiBridgeWithdrawByProof.t.sol:282,306`)

Never read by any on-chain logic, comparison, or storage key.

**Concerns.**

1. **Semantically weak.** Aggregating increments across 10 layers into one number is neither "blocks verified" (varying `numLayers` per call) nor per-layer count. An indexer cannot derive either from it without also reading `numLayers` per `BlockVerified` event.
2. **Overflow is theoretically possible but practically impossible.** `uint256` max ≈ 1.16e77. At measured devnet rate ~3 b/s × up to 10 layers ≈ 30 increments/s, exhausting `uint256` would take ~10⁶⁸ years. The `unchecked` wrapper acknowledges this — gas-saving, not unsafe. **Not a real bug**; flagging the design choice only.
3. **Storage write per layer.** Each `_appendLayer` does an SSTORE for `anchorsRecorded` (warm slot, but still ~100 gas × `numLayers`) and emits two events that both carry the same counter. For `numLayers = 10` that's 10 SSTOREs + 20 events. Tight for a hot path.

**Questions.**

1. Was the counter designed for any specific off-chain consumer (indexer, dashboard, daemon health-check), or is it speculative tooling? If a concrete consumer exists, where is it?
2. Would `blocksVerified` (one increment per `verifyBlock`) plus per-layer cursors (already in `HistoryWindow.writeCursor` / `dataLen`) cover the same use cases more cleanly?
3. Is the duplication between `AnchorRecorded(anchor, total)` and `LayerAnchorAppended(layer, hash, height, total)` deliberate (indexer migration), and what's the deprecation horizon for `AnchorRecorded`?
4. If `anchorsRecorded` has no concrete consumer, can it be removed before `v2.0.0` to reclaim the SSTOREs and the slot?

## AB-Q2 — Stale Groth16/gnark references in `AckiNackiBridge.sol` (and adjacent files) — documentation drift after Shplonk landing

**State.** `e2a962b` (2026-06-23) retired the Fallback Groth16 hybrid and deleted the entire Fallback Groth16 stack: `FallbackGroth16VerifierGenerated.sol`, `FallbackVerifier.sol`, `IFallbackGroth16Verifier.sol`, `FallbackVerifier.t.sol`, `install_fallback_groth16_verifier.sh`, and `gnark-wrappers/circuit-1b/`. Production deploy scripts now only wire Shplonk Yul verifiers (`PrimaryAggregatorVerifier`, `FallbackAggregatorVerifier`, `LayerHashesAggregatorVerifier`) via `ShplonkDeployLib`. Three `.bin` files are checked in under `contracts/ethereum/verifiers/`.

**That cleanup was not applied symmetrically to circuits 1A, 2, or 4.** The repository now contains a confusing mix:

| Layer | Production runtime | Lingering Groth16 surface |
|-------|--------------------|---------------------------|
| Circuit 1A | `PrimaryAggregatorVerifier.sol` (Shplonk, deployed by `ShplonkDeployLib.deployPrimaryAdapter`) | `PrimaryVerifier.sol` (Groth16 adapter), `PrimaryGroth16VerifierGenerated.sol`, `IPrimaryGroth16Verifier.sol`, `gnark-wrappers/circuit-1a/` |
| Circuit 1B | `FallbackAggregatorVerifier.sol` (Shplonk) | **cleaned** (`e2a962b`) |
| Circuit 2 | `LayerHashesAggregatorVerifier.sol` (Shplonk) | `LayerHashesMovementVerifier.sol` (Groth16 adapter), `LayerHashesGroth16VerifierGenerated.sol`, `ILayerHashesGroth16Verifier.sol`, `gnark-wrappers/circuit-2/` |
| Circuit 4 | none yet (gated `WIRE_WITHDRAW_BY_PROOF`) | `BridgeWithdrawalVerifier.sol` (Groth16 adapter), `IBridgeWithdrawalGroth16Verifier.sol`, `gnark-wrappers/circuit-4/` (gutted but dir remains) |

**The `AckiNackiBridge.sol` contract is documentation-drifted.** It still calls its verifier slots "Groth16 adapter" at every reference site, even though at runtime they are Shplonk wrappers behind the same `IPrimaryVerifier` / `IFallbackVerifier` / `ILayerHashesMovementVerifier` / `IBridgeWithdrawalVerifier` interface. The 11 stale references:

```text
contracts/ethereum/src/AckiNackiBridge.sol:
   30  /// @dev ...a tuple of two cross-circuit-bound Halo2/Groth16 proofs:
   40  /// @dev ...plus a Circuit 4 (`bridge-event-prove-circuit`) Groth16 proof
  147  /// @notice Circuit 1A (Primary attestation) verifier (Groth16 adapter).
  152  /// @notice Circuit 1B (Fallback attestation) verifier (Groth16 adapter).
  156  /// @notice Circuit 2 (Layer hashes movement) verifier (Groth16 adapter).
  192  /// @notice Circuit 4 verifier (Groth16 adapter; 10-input single-final-root
  604  ///      after both gnark Groth16 verifiers report success and every cross-
  625  /// @param attestationProof gnark Groth16 proof bytes for Circuit 1A or 1B (256 bytes).
  626  /// @param layerHashesProof gnark Groth16 proof bytes for Circuit 2 (256 bytes).
  731  /// @param attestationProof SHPLONK/Groth16 attestation proof bytes.
  894  ///         Groth16 proof.
  928  /// @param proof   256-byte gnark Groth16 proof bytes (Circuit 4 wrap).
  972  // ---- Crypto: verify the Groth16 proof. The 10 public inputs flow
```

**Concerns.**

1. **Misleading authoritative reference.** `AckiNackiBridge.sol` is the contract auditors and integrators read first. Today it claims a Groth16 backend at every verifier slot. A reader has to cross-check `ShplonkDeployLib.sol` + `production_plan.md` to learn that the runtime wiring is Shplonk. That's exactly the gap that made Q1 of the 06-18 review necessary.
2. **Two interface IDs that mean the same thing.** `IPrimaryVerifier` (still consumed by `AckiNackiBridge`) and `IPrimaryGroth16Verifier` (consumed by the unused `PrimaryVerifier.sol` adapter) co-exist. Same for `ILayerHashesMovementVerifier` vs `ILayerHashesGroth16Verifier`, and `IBridgeWithdrawalVerifier` vs `IBridgeWithdrawalGroth16Verifier`. The first form is production; the second is dead-code-only — but it isn't obvious from the names.
3. **Asymmetric cleanup precedent.** `e2a962b` proved the Fallback cleanup pattern works: delete the adapter, the generated verifier, the Groth16 interface, the tests, and the gnark wrapper directory. Nothing prevents the same surgery on 1A, 2, 4 (modulo any Foundry tests that legitimately want to keep Groth16 fixtures as forgery-test inputs).
4. **`gnark-wrappers/circuit-1a/` + `circuit-2/`** still ship with identity-stub `Define()` (`api.AssertIsEqual(PI[i], PI[i])`). The 06-19 policy is *unambiguous*: stubs only in CI/Foundry, never in production deploy. Production deploy is now stub-free, but the stubs are still in-tree and indistinguishable from real code at a glance.

**Questions.**

1. Is `PrimaryVerifier.sol` / `LayerHashesMovementVerifier.sol` / `BridgeWithdrawalVerifier.sol` still serving any production or CI purpose, or is it dead code waiting for the same surgery as `FallbackVerifier.sol`?
2. Are `PrimaryGroth16VerifierGenerated.sol` and `LayerHashesGroth16VerifierGenerated.sol` (and the matching `I*Groth16Verifier` interfaces) referenced by any Foundry test that has to stay, or can they be deleted along with the adapter files?
3. Can the `gnark-wrappers/circuit-1a/` and `circuit-2/` (and the gutted `circuit-4/`) directories be removed in the same commit as the Solidity cleanup? If a forgery-test fixture is the only reason to keep one of them, please document it in `gnark-wrappers/SECURITY.md`.
4. Sweep `AckiNackiBridge.sol` NatSpec at the 11 sites listed above: replace "Groth16 adapter" / "gnark Groth16 proof bytes" / "Halo2/Groth16 proofs" with the actual production wording ("Shplonk aggregator adapter", "SHPLONK proof bytes", "Halo2 SHPLONK aggregator proofs"). The interface-level abstraction the bridge depends on is `I{Primary,Fallback,LayerHashesMovement,BridgeWithdrawal}Verifier` — the NatSpec should describe *that*, not the no-longer-default backend.
5. Same sweep over the remaining doc files that still mention Groth16 as the production crypto (`docs/audit_trail_v2.md`, `docs/bridge_verification.md`, `docs/integration_analysis.md` were already touched in `4a22334` — please confirm none missed).
6. Is there a single tracking ticket / commit batching all of (1)–(5), or should it land in pieces?

## AB-Q3 — `withdrawByProof` hardcodes `WITHDRAW_ANCHOR_LAYER = 1`; generalize to arbitrary anchor layer

**State.** `AckiNackiBridge.sol:74` and `:968`:

```solidity
uint8 internal constant WITHDRAW_ANCHOR_LAYER = 1;
// ...
// L1-only witness builder (`layer_idx = 0`). Future: read `anchorLayer`
// from Circuit 4 public input slot [10] when partner extends the layout.
if (!_isKnownLayerAnchor(WITHDRAW_ANCHOR_LAYER, pub.finalRoot)) {
    revert UnknownAnchor(pub.finalRoot);
}
```

Today the AN-side witness builder `bridge-event-witness` always anchors withdrawal proofs at L1 (`layer_idx = 0`), so the hardcoded `1` is correct *for now*. Partner side (Alina) plans to lift this restriction shortly — the witness will be able to anchor at any layer `1..MAX_LAYER_HASHES`. Once that ships, `withdrawByProof` will reject valid L≥2 proofs because it scans only the L1 `HistoryWindow`.

**Design options for generalizing the check:**

**Option A — `anchorLayer` as Circuit 4 public input (PI slot [10]).**
Already foreshadowed by the source comment at line 967 and by Pruvendo's 06-19 response to Q3 ("Future: PI slot 10 `anchorLayer` when witness grows to L≥2"). Bridge reads `pub.anchorLayer`, range-checks `1 ≤ anchorLayer ≤ MAX_LAYER_HASHES`, then `_isKnownLayerAnchor(pub.anchorLayer, pub.finalRoot)`. Requires:

- Circuit 4 PI layout extends from 10 → 11 slots.
- The circuit **internally constrains** that `finalRoot` is the root at layer `anchorLayer` of the witness (not at some other layer where the same hash happens to appear).
- `IBridgeWithdrawalVerifier.WithdrawalPublicInputs` struct grows a `uint256 anchorLayer` field.
- Circuit 4 SHPLONK aggregator + Yul verifier regenerated for 11 PIs (re-keygen, new `.bin`).

Pros: layer-to-proof binding is cryptographic — the verifier itself attests "this `finalRoot` is the L-th layer root of the block I prove against". Storage / replay safety unchanged. Aligns with the layout the bridge code already anticipates.

Cons: ABI break on `WithdrawalPublicInputs`; re-keygen + re-aggregator-export round; depends on partner shipping the PI extension.

**Option B — `anchorLayer` as a calldata argument to `withdrawByProof` (your suggestion).**

```solidity
function withdrawByProof(
    bytes calldata proof,
    IBridgeWithdrawalVerifier.WithdrawalPublicInputs calldata pub,
    uint8 anchorLayer
) external ... {
    if (anchorLayer == 0 || anchorLayer > MAX_LAYER_HASHES) {
        revert LayerOutOfRange(anchorLayer);
    }
    if (!_isKnownLayerAnchor(anchorLayer, pub.finalRoot)) {
        revert UnknownAnchor(pub.finalRoot);
    }
    // ...
}
```

Pros: Solidity-only change. No circuit change, no PI extension, no re-keygen, no `.bin` regeneration. Function argument is simple, range-check is trivial.

Cons (the important one): **`anchorLayer` is unbound — supplied by the caller, not by the proof.** Unless Circuit 4 internally constrains finalRoot to a *specific* layer of the witness, an attacker who holds *any* valid Circuit 4 proof can replay it claiming any layer index that happens to contain the same `finalRoot` hash. In practice that's unlikely (each layer's window holds different roots), but the contract no longer enforces layer-of-origin — it just enforces "this root appears *somewhere* the relayer claims". That is effectively the same trust model as the legacy flat `_knownAnchors` Q3 just replaced — except spread across 10 SLOAD-bounded windows instead of one mapping. The per-layer separation buys nothing if the layer-selector is attacker-controlled.

**Option C — combine.** Take `anchorLayer` from the PI (Option A) and **also** accept a calldata `anchorLayer` argument that must equal `pub.anchorLayer`. Pure convenience — saves nothing.

**Option D — fallback scan.** If `anchorLayer` is not in `pub`, fall back to `_isKnownAnchor` (the flat scan over all 10 windows) — preserves correctness at the cost of O(10·W) gas per withdraw. Acceptable as a *transitional* step until PI[10] lands; not acceptable as the steady state.

**Concerns.**

1. **Layer-binding is a circuit obligation, not a contract obligation.** Whatever path is chosen, Circuit 4 must constrain `finalRoot ↔ layer` if the bridge is to trust the layer index. Confirm whether partner's planned witness extension binds layer (it should — the Merkle proof inside Circuit 4 walks from leaf upward to a *specific* layer's root).
2. **ABI stability.** If `WithdrawalPublicInputs` grows a field today (Option A), any in-flight integrations have to update. If we ship Option B now and Option A later, there are two ABI churn events. If Option D is the interim, only one ABI churn (Option A) ever happens.
3. **`MAX_LAYER_HASHES = 10` is the right ceiling** for the range check (already used by `_appendLayer` line 811 and `_isKnownLayerAnchor` line 836).

**Questions.**

1. Which design does Pruvendo prefer for the lift — Option A (PI[10]), Option B (calldata arg), Option C (combined), or Option D (transitional scan)? The source comment at `:967` already points at Option A; please confirm whether that's still the plan after seeing the partner-side timing.
2. If Option A: target shape for `IBridgeWithdrawalVerifier.WithdrawalPublicInputs` (add `uint256 anchorLayer` as slot 10? something else?). Does this gate on Circuit 4 layout freeze (the same blocker as the `.bin` artifact — see `bridge_updates_analysis_2026-06-25.md`), or can it land independently of the Shplonk landing?
3. If Option B is rejected solely on "layer not bound in proof" — would Pruvendo accept it *temporarily* if partner can guarantee the witness builder never produces two valid proofs whose `finalRoot` collides across layers? (We can guarantee that — each layer's hash is a SHA-256 of a structurally distinct preimage.)
4. Whichever path: is there appetite to land a "flat scan fallback" (Option D) in the same PR that introduces `_appendLayer` per layer, so that L≥2 witnesses don't silently revert in the gap between partner shipping non-L1 witnesses and Pruvendo shipping the PI[10] verifier? It would be a 5-line addition guarded by a feature flag.
5. Naming: keep `WITHDRAW_ANCHOR_LAYER` as a legacy constant referencing layer 1 for backward-compat tests, or remove it entirely after Option A lands? (Renaming to `_LEGACY_DEFAULT_WITHDRAW_ANCHOR_LAYER` would signal intent.)

## AB-Q4 — `storedPrevMaxLevelLayerHash` and `storedLayerHashes` semantics: anchor should be picked at the new block's max layer, not stored flat

**State.** `AckiNackiBridge.sol:709-716` (in `verifyBlock`):

```solidity
storedLastSeenBlockSeqNo = blockSeqNo;
storedNumLayers = numLayers;
for (uint256 i = 0; i < MAX_LAYER_HASHES; i++) {
    storedLayerHashes[i] = layerHashes[i];      // (1) full snapshot — written, never read by verifyBlock
}
storedPrevMaxLevelLayerHash = layerHashes[numLayers - 1];  // (2) topmost of the just-verified block
_appendLayerHashes(numLayers, layerHashes, blockSeqNo);    // (3) per-layer HistoryWindow (Q3)
```

The next `verifyBlock` consumes `storedPrevMaxLevelLayerHash` as the anchor (`prevMaxLevelLayerHash` argument to `layerHashesVerifier.verifyLayerHashesMovement(...)`). `storedLayerHashes` is populated but not read by any state-mutating function — only by the `getLayerHashes()` view helper (line 859-867).

**Why this is incorrect for some block sequences.** The proposal (`History proofs proposal-2.docx`) says each layer L's root is constructed at boundaries of `BWS^L` (default `BWS = 128`). A key block straddles all boundaries whose `BWS^L` divides its height — so `numLayers` per block is **not monotonic across the chain**:

| Block | Height | numLayers | layerHashes |
|-------|--------|-----------|-------------|
| A | 128³ (=2,097,152) | 3 | [L1ᴬ, L2ᴬ, L3ᴬ] |
| B | 128³ + 128 | 1 | [L1ᴮ] |
| C | 128³ + 128² | 2 | [L1ᶜ, L2ᶜ] |

Per AN producer code (`acki-nacki/node/src/block/producer/producer_service/block_producer.rs:1471-1513`), every layer L's root is computed as:

```rust
layer_data.calculate_root_hash(
    latest_layer_root(L),         // SAME-layer predecessor — anchor for layer L
    latest_layer_root(L + 1),     // higher-layer predecessor — context only
)
```

So **B's L1 root derives from A's L1 root** (its same-layer predecessor), **not from A's L3 root**. Yet today the bridge feeds `prevMaxLevelLayerHash = layerHashes_A[2]` (A's L3) into B's `verifyLayerHashesMovement`. Circuit 2 either has to ignore the mismatch (silently broken anchoring) or revert (silently broken liveness when `numLayers` decreases across consecutive key blocks).

**Symmetric case.** If C arrives after B with `numLayers_C = 2`, the contract has already overwritten `storedPrevMaxLevelLayerHash` to `L1ᴮ`. C's L2 root depends on the previous L2 root — `L2ᴬ` — which `storedPrevMaxLevelLayerHash` no longer remembers; only `storedLayerHashes[1]` (set when A was processed) holds it, **and only if `storedLayerHashes` was not overwritten by B**. Inspecting the loop on line 712-714: **`storedLayerHashes` IS overwritten on every `verifyBlock`** — including slots above `numLayers - 1`. So if B has `numLayers = 1`, B's `layerHashes[1..9]` overwrite A's L2/L3 history. The full per-layer history is gone after one block.

**Proposed fix (your reading).** Replace flat-stored anchor with a per-layer pick:

```solidity
// Pseudo-Solidity sketch
function verifyBlock(...) external {
    // ...
    uint8 anchorIdx = numLayers - 1;          // new top layer
    uint256 expectedAnchor = storedLayerHashes[anchorIdx];
    // pass expectedAnchor (not storedPrevMaxLevelLayerHash) as prevMaxLevelLayerHash
    bool lhOk = layerHashesVerifier.verifyLayerHashesMovement(
        layerHashesProof, blockId, bkSetCommitment,
        uint256(numLayers), layerHashes,
        expectedAnchor
    );
    // ...
    // On commit: keep storedLayerHashes ONLY at slots [0..numLayers-1];
    // do NOT overwrite higher slots (they still hold the last-seen higher-layer roots).
    for (uint8 i = 0; i < numLayers; i++) {
        storedLayerHashes[i] = layerHashes[i];
    }
    // (drop storedPrevMaxLevelLayerHash altogether, or keep as a read-only view alias)
}
```

This matches `calculate_root_hash`'s contract on AN side: each layer L derives from the last known L-root.

**Concerns.**

1. **Liveness bug today.** If the chain ever produces two consecutive key blocks where `numLayers` *decreases* (A=3 → B=1), the bridge's current `prevMaxLevelLayerHash = A.layerHashes[2]` will not match B's Circuit 2 witness (which references A's L1). Either Circuit 2 rejects (`verifyBlock` reverts forever — bridge halts), or Circuit 2 is more permissive than it should be (anchoring is silently weakened).
2. **`storedLayerHashes` higher-slot loss.** The unconditional 10-slot overwrite (line 712-714) destroys A's L2/L3 the moment a shallower block lands. Off-chain readers of `getLayerHashes()` see B's layer fields *plus stale zeros above B's numLayers*. That's also why moving to "write only `[0..numLayers)`" is needed — to preserve L2/L3 across shallower successors.
3. **`storedPrevMaxLevelLayerHash` is redundant** once we read from `storedLayerHashes[numLayers - 1]`. Drop the slot to recover one SSTORE per `verifyBlock`.
4. **HistoryWindow already separates per-layer.** `_layerWindows[L]` (Q3) already preserves per-layer history correctly via `_appendLayer`. The flat `storedLayerHashes` is now a *second* per-layer cache with worse semantics. Either delete the flat cache entirely and source the anchor from `_layerWindows[anchorIdx + 1].data[last]`, or fix the flat cache. Two caches is the worst option — they diverge silently.
5. **Circuit 2 contract.** Whatever the bridge passes must match the witness builder's input. Confirm with partner side (Alina): does Circuit 2's `prev_max_level_layer_hash` PI semantically mean "previous L root at the same level as this block's max layer" (the producer-code reading above) or "any previously-anchored top layer" (the bridge's current reading)? If the former — bridge is buggy. If the latter — Circuit 2 is buggy (or the proposal is being interpreted loosely). One of the two has to move.

**Cross-references.**

- AN producer per-layer derivation: `acki-nacki/node/src/block/producer/producer_service/block_producer.rs:1471, 1510-1513`
- `calculate_root_hash` same-layer / higher-layer inputs: `acki-nacki/node/src/types/history_proof.rs:163-186`
- `HISTORY_PROOF_WINDOW_SIZE` (= 128 = the same `W` the bridge uses for HistoryWindow): `acki-nacki/node/libs/history-proof/src/lib.rs`
- Proposal: `History proofs proposal-2.docx` (root recursion at `BWS^L` boundaries; `history_proofs` written into the first block of each batch).

**Questions.**

1. Is the current `prevMaxLevelLayerHash = storedLayerHashes[storedNumLayers - 1]` reading deliberate (Circuit 2 accepts any previously-known top regardless of its layer index), or is the bridge silently mis-anchoring for shallower successors? Please cite the Circuit 2 PI definition that resolves this.
2. If it is mis-anchoring, can the bridge land the per-layer pick (`storedLayerHashes[numLayers - 1]` of the **new** block) plus a "write only `[0..numLayers)`" loop in a single commit? Both changes are local to `verifyBlock`.
3. Should `storedPrevMaxLevelLayerHash` be deprecated in favour of `_layerWindows[L].data[last]` (per-layer HistoryWindow is the source of truth)? Keeping both invites drift.
4. Foundry regression: a three-block sequence A (numLayers=3) → B (numLayers=1) → C (numLayers=2) that exercises both shrink-then-grow and the higher-slot preservation. Owner: Pruvendo? Partner can supply the bound witness vectors once Q1 of the Circuit 2 contract is answered.
5. Is `getLayerHashes()` consumed by any off-chain indexer today? If yes, switching to "write only `[0..numLayers)`" changes its return shape for slots above `numLayers` (would now return the **last-known per-layer** value rather than the **last block's array** — semantically closer to spec §8.3 but ABI-observable).
