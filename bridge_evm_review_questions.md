# bridge-EVM — Review Questions

**To:** Pruvendo / Sergey Egorov
**From:** Alina (AN-side circuits + prover)
**Scope:** `bridge-EVM` review from the AN→ETH side (circuits 1A/1B/2/4).

---

# Critical

## Q1 — R15: the only Groth16 verifiers on chain are identity-stub gnark wrappers

**State.** The only Groth16 verifiers actually wired into `contracts/ethereum/src/AckiNackiBridge.sol` are the four gnark-generated `*Groth16VerifierGenerated.sol` (Primary, Fallback, LayerHashes, BridgeWithdrawal — via the adapters `PrimaryVerifier.sol`, `FallbackVerifier.sol`, `LayerHashesMovementVerifier.sol`, `BridgeWithdrawalVerifier.sol`). The `Blake2bHalo2Verifier.sol` / `Halo2Verifier.sol` are test-only legacy. The `bridge-evm-aggregator` (snark-verifier-sdk → Yul) is a separate feasibility spike, not deployed.

**Problem.** Each of those four gnark wrappers — `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-{1a,1b,2,4}/circuit.go` — has a `Define()` body of just:

```go
for i { api.AssertIsEqual(PI[i], PI[i]) }
api.AssertIsDifferent(DomainSize, 0)
```

Self-documented in the source. Circuit 1B (`circuit-1b/circuit.go:21-32`):

> *Define() method here uses identity constraints. Full in-circuit Halo2/SHPLONK verification is planned as a future improvement (it would require a gnark implementation of the Halo2 SHPLONK verifier — non-trivial; see Phase 8 R&D… For now the Groth16 proof commits to the public input values… The off-chain relayer is responsible for having held a verified Halo2 proof before generating the Groth16 wrap.*

Circuit 4 (`circuit-4/circuit.go:29-34`):

> *IMPORTANT — R15 status. This wrapper enforces only identity-stub assertions over the public inputs. The Halo2 SHPLONK proof itself is **NOT verified** inside Groth16… the single biggest open mainnet blocker on the AN→ETH side.*

So on-chain `AckiNackiBridge.verifyBlock` / `withdrawByProof` accept a Groth16 proof that the wrapper PK holder can mint for any PI tuple — the Halo2 layer is not cryptographically attested.

**Branch audit (2026-06-18).** Verified that `Define()` bodies are byte-for-byte identical identity-stubs across **every** open branch on `gosh-sh/bridge-EVM` (`main`, `pruvendo/shellnet-e2e-landing`, `deposit-rlc-vkblob-v2`, `feat/usdt-deposits`, `test/arbitrum-replay-and-n14-runbook`, `docs/agents-git-remotes`, `feature/integrate-all-bridge-prover-code`). No branch has an in-flight real `Define()` for any of the four circuits — R15 is uniformly open repo-wide, not just on `main`.

**Roadmap.** `docs/r15_snark_verifier_roadmap.md` (accepted 2026-05-26) chooses `snark-verifier-sdk::AggregationCircuit` + `gen_evm_verifier_shplonk` → Yul `BridgeWithdrawalAggregatorVerifier.sol`, spiked in `crates/bridge-evm-aggregator/`. Status: M1–M3 ✅, M5 de-risked on a synthetic inner (2026-05-29); M4 (real Circuit 4 inner) blocked on Circuit 4 stability + Blake2b-vs-Poseidon transcript mismatch.

**Questions.**
1. Is the snark-verifier-sdk aggregator + Yul verifier still the chosen path, or has it shifted?
2. Concrete milestone / target dates for M4 + M5 against the real Circuit 4 proof, and for wiring `BridgeWithdrawalAggregatorVerifier.sol` into `AckiNackiBridge.withdrawByProof`?
3. Do circuits 1A/1B/2 also migrate to the aggregator path (Yul), or are they staying on gnark and getting a real `Define()` later? If gnark, what's the plan and timeline?
4. Transcript: do you need us to switch the Circuit 4 prover from Blake2b to Poseidon, or will you add a Blake2b loader to `snark-verifier`? Please confirm which side owns the change.
5. Until R15 closes, please confirm in writing that the intended trust model for Sepolia is relayer-rooted attestation, and that mainnet is explicitly gated on R15 (no `v2.0.0` tag before).

---

## Q2 — `crates/bridge-evm-aggregator` status: Circuit 4 plan is unimplemented, Circuits 1/2 not even planned

**State.** `crates/bridge-evm-aggregator/` is the chosen R15 vehicle (snark-verifier-sdk → Yul). It is a standalone-workspace **feasibility spike** — M2 closed 2026-05-27, M5 instance-exposure de-risked 2026-05-29 on a synthetic inner (`src/multiply.rs` proving `a*b == c`). The Yul output `target/spike/AggregatorVerifierSpike.{sol,bin}` is `.gitignore`d and **not wired into any production contract**. Nothing in `src/` references the real circuits.

**Gap — Circuit 4.** Circuit 4 is now finished on the partner side (`acki-nacki-to-eth-bridge-halo2-circuits`), so the M4 *partner-side* blocker is lifted. But the bridge-EVM side of M4 (swap `multiply.rs` for real Circuit 4 deserialization), and M5/M6/M7 (final K-sizing, move Yul into `contracts/ethereum/src/BridgeWithdrawalAggregatorVerifier.sol`, Foundry E2E) are **all still on paper only**. The roadmap doc has no dates.

**Black hole — Circuits 1A/1B/2.** Every milestone in `docs/r15_snark_verifier_roadmap.md` and `bridge-evm-aggregator/README.md` is written exclusively for Circuit 4 / `BridgeWithdrawalAggregatorVerifier.sol`. Circuits 1A (Primary attestation, per-block), 1B (Fallback, per-block), and 2 (Layer hashes, per attestation) **are not mentioned in any milestone**. Yet on chain they sit behind the same kind of identity-stub gnark wrapper (Q1) — fixing only Circuit 4 closes the withdrawal lane but leaves the entire block-attestation lane cryptographically void.

**Questions.**
1. Concrete dates for M4-EVM (swap `multiply.rs` → real Circuit 4 snark deserialization in `aggregator.rs`), M5 final K-sizing for Circuit 4's ~10 PIs, M6 (wire generated Yul to `BridgeWithdrawalVerifier.sol` adapter), M7 (Foundry E2E).
2. For Circuits 1A/1B/2, pick one and commit to it in the roadmap:
   - **(A)** Extend the aggregator spike to four Yul verifiers — needs K-sizing + per-verify gas estimate (1A/1B are called *every block*; BLS pairings in 1A likely don't fit K=21).
   - **(B)** Real gnark `Define()` bodies for 1A/1B/2 (your own `circuit-1b/circuit.go:21-32` calls this "non-trivial; Phase 8 R&D") — give cryptographer-owner + ETA.
   - **(C)** Leave 1A/1B/2 relayer-attested by design — give the written threat model + on-chain compensating control (multisig, challenge window).
3. Transcript: confirm all four circuits switch from Blake2b to Poseidon (not just Circuit 4), since `snark-verifier`'s EVM loader requires it.
4. Confirm `v2.0.0` (mainnet) is gated on the answer to #2, not just on Circuit 4 R15 closure.

---

## Q3 — Bridge-state shape diverges from `GLOBAL_HISTORY_DATA_SPEC.md §8`: latent withdrawal correctness bug + unbounded storage

**State.** `contracts/ethereum/src/AckiNackiBridge.sol` models the on-chain mirror of `GlobalHistoryData` as a **flat snapshot plus an unbounded set**:

```solidity
// AckiNackiBridge.sol:161-239 (paraphrased)
uint64  public storedLastSeenBlockSeqNo;
uint8   public storedNumLayers;                            // 1..=10
uint256[MAX_LAYER_HASHES] public storedLayerHashes;        // MAX_LAYER_HASHES = 10
uint256 public storedPrevMaxLevelLayerHash;
mapping(uint256 => bool) private _knownAnchors;
uint256 public anchorsRecorded;
```

`verifyBlock` (line 607–695) commits state with a **verbatim, overwriting copy** of the calldata array followed by a *single* anchor insert:

```solidity
// AckiNackiBridge.sol:680-692
storedLastSeenBlockSeqNo = blockSeqNo;
storedNumLayers          = numLayers;
for (uint256 i = 0; i < MAX_LAYER_HASHES; i++) {
    storedLayerHashes[i] = layerHashes[i];               // (*) overwrites
}
storedPrevMaxLevelLayerHash = layerHashes[numLayers - 1];
_recordAnchor(layerHashes[numLayers - 1]);               // (**) only the top

// AckiNackiBridge.sol:705-711
function _recordAnchor(uint256 anchor) internal {
    _knownAnchors[anchor] = true;
    unchecked { anchorsRecorded = anchorsRecorded + 1; }
    emit AnchorRecorded(anchor, anchorsRecorded);
}
```

`withdrawByProof` (line 780–860) then performs a single flat membership check against that unbounded set:

```solidity
// AckiNackiBridge.sol:821-823
if (!_knownAnchors[pub.finalRoot]) {
    revert UnknownAnchor(pub.finalRoot);
}
```

**Spec divergence.** `acki-nacki-to-eth-bridge-halo2-circuits/GLOBAL_HISTORY_DATA_SPEC.md §8.3` already prescribes the **correct on-chain layout** the contract is supposed to mirror:

```solidity
struct HistoryWindow {
    bytes32[W] data;       // W = HISTORY_PROOF_WINDOW_SIZE = 128 in production
    uint256    dataLen;
    uint256    writeCursor;
    uint256    lastHeight;
}
mapping(uint8 => HistoryWindow) layerWindows;   // layer L = 1..MAX_LAYERS

function appendLayer(uint8 layer, bytes32 hashValue, uint256 blockHeight) external { ... }
```

— a **per-layer circular buffer** identical in shape to the off-chain `BridgeState` already implemented in `acki-nacki-to-eth-bridge-halo2-prover/bridge-prover-lib/src/bridge_state.rs`:

```rust
pub const MAX_LAYERS: usize = 10;
pub struct HistoryWindow {
    pub data:         Vec<[u8; 32]>,   // sized W
    pub heights:      Vec<u64>,
    pub data_len:     usize,
    pub write_cursor: usize,
    pub last_height:  u64,
}
pub struct BridgeState {
    pub window_size:   usize,
    pub layer_windows: Vec<HistoryWindow>,   // 10 entries, per spec
    ...
}
```

The verifier daemon (`bridge-verifier-daemon/src/main.rs`) already calls `state.append_bundle(per_layer_hashes, block_height, ...)` on this off-chain mirror per spec §3 cadence (layer L appended iff `h % W^L == 0`). `BRIDGE_PROVER_THINNING_SPEC.md §8.1/§8.3` confirms thinning leaves the `appendLayer` signature and `HistoryWindow` layout unchanged — the spec is stable and ready to land in Solidity unmodified.

**`AckiNackiBridge.sol` does none of that.** It has no circular buffer, no per-layer windows, no `appendLayer` entry point. `storedLayerHashes[10]` is a *single-snapshot* of the most recent `verifyBlock` call — overwritten on every subsequent call. The historical context is squeezed into `_knownAnchors`, a flat untyped `mapping(uint256 => bool)` that only ever grows and only ever records the topmost hash.

**Problems.**

1. **No layered structure in storage.** The contract loses the layer-of-origin of every recorded hash. `_knownAnchors` is a flat bag; once a hash is in it there's no way to ask *"is this an L1 root, an L5 root, or an L10 root?"* The off-chain mirror keeps that distinction because the spec requires it for proving (the witness builder must know which layer the proof anchors to). The contract throws that away on insert.

2. **`storedLayerHashes` is a snapshot, not a window.** Each `verifyBlock` call **overwrites** all 10 slots (line 683-685). Two successive calls expose the layer-hashes of the second one only; the first set is unreachable from `storedLayerHashes`. The spec's rolling-window semantics (`data_len`, `write_cursor`) — which the off-chain mirror implements — are absent.

3. **`_recordAnchor` records the topmost layer only.** Line 691-692 takes `layerHashes[numLayers - 1]` and inserts *that single value*. For a key block at an L≥2 boundary (≈ 6 % of bundles in production per `BRIDGE_PROVER_THINNING_SPEC.md §3.2`), the L1 root that's sitting in `layerHashes[0]` is **never written into `_knownAnchors`**.

4. **Latent correctness bug — withdrawals for ~6 % of bundles will revert with `UnknownAnchor`.** `bridge-event-witness/src/bin/build.rs:~439` hard-codes `layer_idx = 0` — i.e., every withdrawal proof anchors to the **L1 root** (`pub.finalRoot` = an L1 hash). Combine with problem (3):
   - When the key block has `numLayers == 1`: topmost = `layerHashes[0]` = L1 root → recorded → withdraw succeeds ✓
   - When the key block has `numLayers ≥ 2`: topmost = `layerHashes[numLayers-1]` ≠ L1 root. The L1 root sits in `storedLayerHashes[0]` only until the next call overwrites it, and is **never inserted into `_knownAnchors`**. Withdrawals proving anchoring to that L1 root **revert with `UnknownAnchor`** even though the contract did verify the corresponding bundle's L1 hash via Circuit 2 — i.e., the contract has the cryptographic evidence but discarded it.

   Per §3.2 of the thinning spec at P=8/W=128, ≈ 6 % of bundles are order ≥ 2 — that is a ≈ **6 % silent withdrawal-revert rate baked into the storage shape**. Not a transient race, not a relayer mistake: the contract is structurally unable to validate those proofs. (This bug is also called out indirectly in `acki-nacki-to-eth-bridge-halo2-prover/TECHNICAL_README.md:551-563` as "L1→L5 escalation TODO" — the L1-only anchoring side of the same coin.)

5. **`_knownAnchors` is unbounded.** Nothing ever deletes entries. At today's measured ~3 source-block/s and a thinned bundle every `W·P = 1024` source blocks, the contract executes ≈ 252 `verifyBlock` calls/day ≈ 92 000/year, each minting one new mapping entry (one fresh `SSTORE` of an unset slot ≈ 22 100 gas — paid on top of the proof gas). Five years of mainnet operation ≈ 460 000 anchors; over a contract's expected lifetime the cost stays manageable per call but state grows monotonically forever. Compare to the spec's bounded `W = 128` per layer × 10 layers = **1 280 slots total**, ≈ 41 KB per thread, **fixed size**.

6. **Flat anchor membership conflates layers — vulnerable to ambiguity.** Because `_knownAnchors` is shape-untyped, two anchors at different layers that happen to share a hash value (cryptographically unlikely but conceptually unsound) collide. The spec's per-layer windows preclude this by typing.

7. **The off-chain twin is already correct — the contract is the only piece missing.** `bridge-prover-lib::BridgeState` (mirror), `bridge-verifier-daemon` (writer of that mirror), and the spec (§8.3-8.4 reference Solidity) **all already agree on the per-layer-window shape**. The contract is the lone outlier. Closing the gap is purely a Solidity change — no off-chain prover or circuit changes are required.

**Solidity sketch (replacement for §"Stored AN state" + `_recordAnchor` + `withdrawByProof` anchor check).**

Mirrors §8.3-8.4 of `GLOBAL_HISTORY_DATA_SPEC.md` directly. Storage layout, append cadence, and pruning semantics match the off-chain `BridgeState`.

```solidity
// ---- Storage (replaces storedLayerHashes / _knownAnchors / anchorsRecorded) ----

uint8   public constant MAX_LAYERS              = 10;
uint16  public constant HISTORY_PROOF_WINDOW    = 128;        // = W

struct HistoryWindow {
    uint256[HISTORY_PROOF_WINDOW] data;    // circular buffer
    uint64[HISTORY_PROOF_WINDOW]  heights; // height of each entry; supports range queries
    uint16                        dataLen;     // active length, 0..W
    uint16                        writeCursor; // next-write slot, 0..W-1
    uint64                        lastHeight;  // height of the most-recent append
}

// Layer L ∈ [1, MAX_LAYERS] → its rolling window.
mapping(uint8 => HistoryWindow) internal _layerWindows;

// Monotonic event counter (off-chain indexers).
uint256 public anchorsRecordedTotal;

// ---- Append (replaces flat _recordAnchor) ----

/// @dev Append `hashValue` into layer L's circular buffer. Spec §8.4 semantics.
/// Called once per non-zero entry in the incoming `layerHashes` tuple from
/// `verifyBlock`, with cadence already enforced by the prover side
/// (layer L appended iff `h % W^L == 0` in source-block terms — the contract
/// trusts the prover here because the cadence is part of the Circuit-2 PI).
function _appendLayer(uint8 layer, uint256 hashValue, uint64 blockHeight) internal {
    require(layer >= 1 && layer <= MAX_LAYERS, "layer OOB");
    HistoryWindow storage w = _layerWindows[layer];
    require(blockHeight > w.lastHeight, "non-monotonic height");

    w.data[w.writeCursor]    = hashValue;
    w.heights[w.writeCursor] = blockHeight;
    w.writeCursor = uint16((uint256(w.writeCursor) + 1) % HISTORY_PROOF_WINDOW);
    if (w.dataLen < HISTORY_PROOF_WINDOW) {
        w.dataLen = w.dataLen + 1;
    }
    w.lastHeight = blockHeight;

    unchecked { anchorsRecordedTotal++; }
    emit LayerAnchorAppended(layer, hashValue, blockHeight, anchorsRecordedTotal);
}

/// @dev O(W) membership test against layer L's window. W=128 → ~3 200 gas worst-case.
function _isKnownLayerAnchor(uint8 layer, uint256 hashValue) internal view returns (bool) {
    HistoryWindow storage w = _layerWindows[layer];
    uint256 n = w.dataLen;
    for (uint256 i = 0; i < n; i++) {
        if (w.data[i] == hashValue) return true;
    }
    return false;
}

// ---- verifyBlock — replace lines 683-692 ----

// Old: overwriting flat copy + single _recordAnchor(topmost).
// New: per-layer append for each non-zero entry. Trust the cadence
// because the layerHashesProof Circuit-2 PI binds (numLayers, layerHashes)
// to the block_height.
storedLastSeenBlockSeqNo    = blockSeqNo;
storedNumLayers             = numLayers;
storedPrevMaxLevelLayerHash = layerHashes[numLayers - 1];

uint64 blockHeight = blockSeqNo;  // or a separate `block_height` PI if Circuit 2 ever splits the two
for (uint8 L = 1; L <= numLayers; L++) {
    _appendLayer(L, layerHashes[L - 1], blockHeight);
}

// ---- withdrawByProof — replace lines 821-823 ----

// The withdrawal proof exposes (finalRoot, anchorLayer) — anchorLayer is the
// layer-of-origin of `finalRoot`. Add it to Circuit 4's PIs (slot [10]); for
// the current L1-only event-witness builder it is constantly = 1.
// O(W) lookup in the right window — and ONLY the right window.
if (!_isKnownLayerAnchor(pub.anchorLayer, pub.finalRoot)) {
    revert UnknownAnchor(pub.finalRoot);
}
```

**Notes on the sketch.**

- **Bounded storage.** 10 × (128 × 32 B data + 128 × 8 B heights + 3 scalars) ≈ 51 KB, fixed forever. Equivalent to the spec's bound (≈ 41 KB without heights). No growth term.
- **Bounded gas.** `_isKnownLayerAnchor` is `O(W) = O(128)` per call (`~3 200` gas) plus a single SLOAD for `dataLen` — well below the current per-call cost of `verifyBlock` / `withdrawByProof`. If linear scan turns out to be hot enough to optimize, fold a per-window `mapping(uint256 => bool) presence` *inside* the struct and keep it in sync with the circular eviction — bounded at `W` entries per layer.
- **No `_knownAnchors` migration question.** If `verifyBlock` has not yet been called against this storage on mainnet (current sepolia/local-only state), the old slots can be dropped wholesale. If any anchors have been recorded on mainnet, a one-shot migration script can replay the events into the new windows. **For the current state of the repo there is no on-chain data to lose.**
- **Circuit-4 PI surface change.** `withdrawByProof` needs to learn the layer-of-origin of `finalRoot`. Two paths: (a) add `anchorLayer` (`uint256`) as a new public input on Circuit 4 (PI slot 10), and propagate through `bridge-event-witness`; (b) keep the current L1-only contract assumption (`anchorLayer = 1` hardcoded in Solidity) and defer (a) until the event-witness builder grows L≥2 anchoring (TODO in `TECHNICAL_README.md:551-563`). Path (b) is the minimal closing of the latent-bug surface; path (a) is the spec-correct end state.
- **`appendLayer` cadence trust.** The spec defers cadence enforcement (`h % W^L == 0`) to the prover — Circuit 2's PIs already bind `(numLayers, layerHashes, blockHeight)` together, so the contract can append each non-zero entry without re-checking the modular cadence. This matches §8.4 of the spec.

**Recommendation for Pruvendo.**

1. The on-chain storage layout in `AckiNackiBridge.sol:161-239` is the **only piece** of the bridge that has not yet implemented `GLOBAL_HISTORY_DATA_SPEC.md §8.3-8.4`. The off-chain mirror (`bridge_state.rs`), the verifier daemon, the event witness builder, and the thinning spec are all already aligned with the per-layer rolling-window shape. **Bring the contract into alignment with the spec it cites.**

2. Treat this as a **correctness fix, not an optimization** — the current flat `_knownAnchors` mapping has a ≈ 6 % silent withdrawal-revert rate (problem 4 above) under the production thinning configuration (W=128, P=8). The unbounded growth of `_knownAnchors` is a *separate* concern and individually less urgent; the layered-rolling-window design fixes both in one structural change.

3. Coordinate the Circuit-4 PI addition (`anchorLayer`) with the AN side (`bridge-event-witness`) so the contract's per-layer lookup has the right layer to consult. If you'd prefer to land the storage shape first and the PI later, the interim Solidity can pass `1` as a constant `anchorLayer` — this *already* fixes problem 4 for the current event-witness builder, because it always anchors to L1.

4. **Pair this with a Foundry test** that runs the order-≥-2 case (a key block with `numLayers ≥ 2`) followed by a withdrawal anchored to `layerHashes[0]`. With the current `AckiNackiBridge.sol`, that test reverts with `UnknownAnchor`. After the change, it passes. This is a cheap, regression-tight way to lock in the fix and the spec's expected behavior in CI.

**Questions.**

1. Is there an explicit reason `AckiNackiBridge.sol` ships the flat `_knownAnchors` + overwriting `storedLayerHashes[10]` design instead of the per-layer rolling-window layout prescribed by `GLOBAL_HISTORY_DATA_SPEC.md §8.3-8.4`? If so, please document the design-departure rationale; otherwise please track this as a fix.
2. Confirmation that the ≈ 6 % silent withdrawal-revert rate is a known issue, or, if news, an indicator of whether you can reproduce it in a Foundry test (we can supply one based on the partner-side fixtures).
3. Concrete ownership and target date for landing the spec's storage layout + `appendLayer` + per-layer anchor check, and whether the Circuit-4 PI extension (`anchorLayer`) lands in the same change or staged.
4. Whether you'd prefer the AN side to keep `bridge-event-witness` L1-only (matching the interim hardcoded `anchorLayer = 1`) until the full L≥2 escalation work in `TECHNICAL_README.md:551-563` is funded.

---

# Moderate

## Q4 — `bridge-prover-orchestrator/src/` largely duplicates `bridge-prover-lib`; please refactor

The orchestrator's four `src/bin/` binaries are the only product here, yet only `export_primary_proof.rs` actually imports from `bridge_prover_lib` (`generate_primary_proof`, `KeyManager`). The other three (`export_fallback_proof.rs`, `export_layer_hashes_proof.rs`, `export_bound_block_proofs.rs`) reach for orchestrator-local re-implementations. Roughly **~65% of `src/*.rs` is duplicated logic** — three of the four binaries can move to `bridge_prover_lib` imports with small upstream additions.

**Delete after small lib additions (Circuit 1B + Circuit 2 paths):**

| File | Replace with | Lib addition (status) |
|---|---|---|
| `keys.rs` | `bridge_prover_lib::keys::KeyManager` (`fallback_vk/pk/config()` accessors) | ✅ already present in lib (`keys.rs:294-304`) |
| `layer_hashes_keys.rs` | same `KeyManager` (`layer_vk/pk/config()`, `layer_k/num_unusable_rows/lookup_bits()`) | ✅ already present in lib (`keys.rs:391-413`) |
| `prover.rs` (Blake2b) | `bridge_prover_lib::prover::generate_fallback_proof` | ✅ already present in lib |
| `prover.rs` (Poseidon transcript) | `bridge_prover_lib::prover::create_proof_with_transcript<E, T, C>` driven with orchestrator's `PoseidonWrite` | ✅ **landed** in `acki-nacki-to-eth-bridge-halo2-prover@main` — generic transcript helper exposing the underlying KZG/SHPLONK `create_proof` call |
| `verifier.rs` (Blake2b) | `bridge_prover_lib::verifier::verify_fallback_proof` | ✅ already present in lib |
| `verifier.rs` (Poseidon transcript) | `bridge_prover_lib::verifier::verify_proof_with_transcript<E, T>` driven with `PoseidonRead` | ✅ **landed** in `acki-nacki-to-eth-bridge-halo2-prover@main` — generic transcript helper for the verify path |
| `layer_hashes_prover.rs` | `bridge_prover_lib::layer_prover::generate_layer_proof_with_input` accepting `LayerHashesProofInput<'a>`; returns `LayerHashesProofOutput` | ✅ **landed** in `acki-nacki-to-eth-bridge-halo2-prover@main` — `LAYER_HASHES_NUM_PUBLIC_INPUTS = 14`, struct + wrapper added next to existing `generate_layer_proof` |
| `layer_hashes_test_data.rs` | `bridge_test_data_gen::layer_hashes::build_synthetic_layer_hashes_input` returning `SyntheticLayerHashesInput` | ✅ **landed** in `acki-nacki-to-eth-bridge-halo2-circuits@main` — **production-fixed `TREE_DEPTH = 8` (`HISTORY_PROOF_WINDOW_SIZE = 128`)**. The orchestrator-local copy hardcodes `TREE_DEPTH = 4` (lightweight `WINDOW_SIZE = 8`) — that fixture is not acceptable for mainnet. The upstreamed builder exposes only depth 8; no smaller value is available. Re-export from `bridge_prover_lib::layer_prover` is trivial if a single import root is preferred. |

All five additions are additive (no breaking changes to lib's existing public API) and live next to the existing primitives in `bridge-prover-lib/src/{prover,verifier,layer_prover}.rs` and `bridge-test-data-gen/src/layer_hashes.rs`. Built clean (`cargo build -p bridge-prover-lib`, `cargo build -p bridge-test-data-gen`). The orchestrator's `Cargo.toml` already pins both crates by git URL — just `cargo update` to pick them up.

**Keep — genuinely orchestrator-specific:**
- `poseidon_transcript.rs` — vendored Poseidon FS transcript, only used by the gnark export path.
- `halo2_tvm_bundle.rs` — `ZKHALO2VERIFYWITHVK` TVM opcode payload (AN-side TVM, not relevant to lib consumers).
- `proof_export.rs` — gnark wrapper IO glue.
- `bound_test_data.rs` — cross-circuit fixture for `export-bound-block-proofs`; may eventually graduate into `bridge-prover-lib` test helpers, but not blocking.

**Request.** The lib + test-data-gen additions are already shipped (see `acki-nacki-to-eth-bridge-halo2-prover@main` commit `e4d083d9` and the `bridge-test-data-gen` follow-up). Please refactor `src/bin/{export_fallback_proof,export_layer_hashes_proof,export_bound_block_proofs}.rs` to import from `bridge_prover_lib` / `bridge_test_data_gen` and delete the six files above. **Note on depth:** the upstreamed `build_synthetic_layer_hashes_input` is fixed to `TREE_DEPTH = 8` — the only mainnet-acceptable configuration; the existing depth-4 fixture must not survive the cutover. Net effect: orchestrator `src/` shrinks from ~2.9 kLoC to ~1.5 kLoC of code that is genuinely orchestrator-only.

---

## Q5 — `poseidon-proof/` role and removal candidacy

**State.** `poseidon-proof/` is a standalone, workspace-excluded crate (root `Cargo.toml:10`). It defines a trivial demo Halo2 circuit (`PoseidonPreimageCircuit`: prove `Poseidon(x) = h`, single Fr public input, K=12, ~1.5 KB proof, BN254/SHPLONK/Blake2b transcript) plus four binaries (`generate-proof`, `generate-verifier`, `generate-calldata`, `test-keccak-evm`) that emit fixture artifacts under `poseidon-proof/data/`. No Rust crate links it. Only consumer is the Foundry test suite: `contracts/ethereum/test/Blake2bHalo2Verifier.t.sol:55,201,212` reads `evm_calldata.bin`, `keccak_evm_calldata.bin`, `keccak_verifier_deployment.bin`, and `foundry.toml:14` whitelists `../../poseidon-proof/data/` for `vm.readFileBinary`.

**Verdict (per `docs/integration_analysis.md:42` — "reference / demo material" — and `docs/BLAKE2B_HALO2_VERIFIER.md`).** The production AN→ETH path now uses gnark Groth16 wrappers (`crates/bridge-prover-orchestrator/gnark-wrappers/circuit-{1a,1b,2,4}/`) around the partner's four-circuit stack — that supersedes the direct-Halo2-on-chain route. So `poseidon-proof` itself is historical/reference material that still backs the Blake2b Solidity verifier test suite.

**Halo2 incompatibility.** `poseidon-proof/Cargo.toml` pins crates.io `halo2-base = "=0.5.0"` and `snark-verifier-sdk = "=0.2.3"`. The production graph (deposit-prover, bridge-prover-orchestrator, partner's `bridge-prover-lib`) is anchored on the gosh fork `halo2-lib-zkevm-sha256-and-bls12-381` at `bump-halo2-lib-v0.4.1` (halo2-base 0.4.1 / zkevm-hashes 0.2.1). Mixing the two halo2 lines into one cargo unit is not possible — which is exactly why this crate has its own `[workspace]` boundary today. It also means the global `[patch]` work just landed (snark-verifier pin to `ccdb510`, axiom-eth bump to `51dec0d3`) does not — and cannot — reach this crate.

**Questions.**
1. Is `poseidon-proof/` still needed at all? The only live consumer is the Blake2b verifier Foundry test; the artifacts under `poseidon-proof/data/` are already checked in (`evm_calldata.bin`, `keccak_evm_calldata.bin`, `keccak_verifier_deployment.bin`, `params/kzg_bn254_12.srs`). If the binaries are not re-run, the crate is effectively unused build-time weight.
2. Removal option A — delete `poseidon-proof/` entirely; keep the pre-built `data/*.bin` artifacts moved under `contracts/ethereum/test/fixtures/`. The Blake2b verifier test continues to work; we lose the ability to regenerate the fixtures, but the AN→ETH gnark path doesn't need them.
3. Removal option B — keep the crate but freeze it (no toolchain bumps, no halo2 upgrades). Accept that it will never share the gosh halo2-axiom backend.
4. If neither — what is the intended long-term role of `poseidon-proof/`? Should it be migrated to the gosh fork (non-trivial: drops crates.io 0.5.x → fork 0.4.1, possible API drift), or kept on crates.io 0.5.x as a deliberate isolation?
