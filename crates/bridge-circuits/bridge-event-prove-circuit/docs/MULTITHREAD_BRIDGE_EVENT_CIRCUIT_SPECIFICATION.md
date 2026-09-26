# Multi-Thread Cryptographic Scheme — Bridge Event-Prove Circuit

Target embodiment: `bridge/crates/bridge-circuits/bridge-event-prove-circuit` (AN → ETH bridge Circuit 4).

This document describes the cryptographic mechanism for proving, in zero knowledge, that a `WithdrawalInitiated` event in **any thread t** of Acki Nacki can be anchored, via cross-thread chaining when `t ≠ 0`, against a layer-N batch root recorded in the Ethereum-side `AckiNackiBridge` per-layer window, and how the bridge Circuit 4 embodies that mechanism.

This document tracks only the **bridge-specific deltas**, most of which flow from a single design choice: **the bridge has no anonymity requirement**, so it drops salting / position tags / uniformity padding (like it was done in DEX) and glues snarks by clear block-ids.

## 0. Terminology

Where a symbol has the same meaning as in the DEX spec, only the delta is called out.

| Term | Meaning |
|------|---------|
| **BWS** | Batch Window Size = **128**. (`HISTORY_PROOF_WINDOW_SIZE`, canonical.) |
| **Batch M** (thread 0) | Blocks at heights `[M·BWS, (M+1)·BWS − 1]` of thread 0. |
| **`#L<N>(M)`** | Layer-N batch root for batch M of thread 0. |
| **X** | The **event block** — the AN block in some thread t (t may be 0 or ≠ 0) that emitted the `WithdrawalInitiated` event. Hidden by convention, but not required to be hidden (see §9). |
| **Y** | The **anchor block** — a thread-0 block reached from X by a chain of L7 reference hops. `t = 0 ⇒ Y = X`. |
| **X-side / Y-side** | Same meaning as DEX. When t = 0 they collapse onto the same block; when t ≠ 0 they are separate. |
| **`event_hash`** | 32-byte SHA-256 hash of the `WithdrawalInitiated` ext-out message wrapper cell (root of the 4-cell BOC). |
| **Block leaf** (thread 0, layer-1) | `block_leaf = Poseidon96(block_id ‖ envelope_hash ‖ tracked_ext_out_messages_root)`. Identical to DEX. |
| **`finalRoot`** | The layer-N batch root the prover anchors against — exposed as `PUB_FINAL_ROOT` on `BridgeEventFinalProof`. Must lie inside the Ethereum-side `layerWindows[anchorLayer]` (`contracts/ethereum/src/AckiNackiBridge.sol`, `_isKnownLayerAnchor`). |
| **`anchorLayer`** | 1-indexed layer number the prover claims for `finalRoot`. Range `1..=MAX_ANCHOR_LAYER = 10` (must equal Solidity `MAX_LAYER_HASHES`). Existing public input, unchanged. |
| **L** | True chain length in hops from X to Y. `L = 0 ⇔ t = 0`. |
| **`L_MAX`** | Circuit-side upper bound on L. **Production target = 300** (same node-team ceiling as DEX). Prototyping point matches DEX: `L_MAX = 20`. |
| **`H`** | Hops packed per `BridgeMultiHopProof` snark. Locked at **5** (identical to DEX). |
| **`N_BUNDLE`** | Maximum number of `BridgeMultiHopProof` snarks per claim. Dynamic under this design (§6.5); prototype cap = 4 (`L_MAX = 20`), production cap = 60 (`L_MAX = 300`). |
| **Bundle** | One `BridgeEventFinalProof` + `n ∈ [0, N_BUNDLE]` `BridgeMultiHopProof` snarks. `n = 0` is the same-thread case (`t = 0`, L = 0). |
| **Circuit 4** | Current on-chain name for the bridge event-prove circuit in `AckiNackiBridge.sol`. This spec keeps the name and extends the public-input surface. |

**Poseidon input convention (unchanged from DEX and current bridge circuit).** Byte streams pack into Fr in **31-byte chunks** (LE, high byte implicitly zero). An N-byte input consumes `⌈N/31⌉` Fr elements. Applies to every `Poseidon(...)` in this spec — Poseidon96 block-leaf, ext-out leaf, layer-N combines. The salt/salted-endpoint constructions of the DEX spec are **absent** here.

---

## 1. Anchor model

### 1.1 Where events happen vs. where they anchor

`WithdrawalInitiated` (declared in the AN bridge exchange contract, ABI event id `0x3c838959`) can be emitted from any thread of Acki Nacki. The **anchor** must be a thread-0 block, exactly as in DEX: only thread 0 populates `history_proofs`, and the Ethereum verifier reads its per-layer window that mirrors `GlobalHistoricalData[thread 0]` (see [`crates/bridge-circuits/docs/GLOBAL_HISTORY_DATA_SPEC.md`](../../docs/GLOBAL_HISTORY_DATA_SPEC.md)).

When `t = 0`, X = Y and no cross-thread bridging is needed. When `t ≠ 0`, a chain of L cross-thread L7 references links X (thread t) to some Y (thread 0).

### 1.2 The contract-side check — Ethereum flavour

Where DEX consumes `gosh.check_layer_hash` inside `RootPN.sol`, the bridge consumes the on-chain per-layer window inside `AckiNackiBridge.sol`. The existing code path (`_isKnownLayerAnchor`) already accepts `(finalRoot, anchorLayer)` and returns `true` iff `finalRoot ∈ layerWindows[anchorLayer]`. The change here is orchestration — the withdraw entrypoint now consumes a **bundle** of proofs and checks continuity across them before delegating to the per-snark SHPLONK verifiers.

### 1.3 Anchor strategy

Same as DEX: prefer the smallest N (cheapest), fall back to higher N if the layer-1 root containing the event has aged out of the Ethereum-side rolling window. Unlike DEX, **anchoring is not used as an anonymity primitive**; picking a higher N never buys privacy here, so the daemon should always pick the smallest layer whose window still holds the containing batch.

### 1.4 Uniformity is NOT required

The DEX spec sets `N_BUNDLE` to a constant across every claim and pads inactive hops for uniformity — an observer must not tell `t = 0` from `t ≠ 0`. The bridge has **no anonymity requirement** (§9), so:

- `N_BUNDLE` per claim is **dynamic**: exactly `ceil(L / H)` snarks. `t = 0` sends **only** the FinalProof — no hop snarks at all.
- Inactive-hop padding still exists inside a partially-full trailing snark (when `L mod H ≠ 0`), but the bundle length itself is not padded.
- `bundle_index` and per-snark position tags are not needed.

This makes the bundle strictly cheaper than DEX at every real L.

---

## 2. block_id construction — depth-4, 16-leaf SHA-256 tree

**Delta from DEX: none.** Same 16-leaf, depth-4 SHA-256 tree; same right-subtree constants (§2.1 of DEX spec); same L7 (variable-depth Poseidon dense-Merkle of `[parent_block_id, refs...]`); same L8 (Poseidon dense-Merkle root of tracked ext-out messages).

The bridge repo pins the canonical block-id construction in [`crates/bridge-circuits/docs/BLOCK_ID_ALG_NEW.md`](../../docs/BLOCK_ID_ALG_NEW.md); read it for the layer-by-layer breakdown. The single item worth re-stating here is that **L9..L15 are `[0u8; 32]` constants** and therefore three of the four right-subtree cousins used when opening L8 are hard-coded fixed cells — only `h0..7` is a live witness. Opening L8 in-circuit therefore costs 4 SHA-256 compressions (one per tree level), same accounting as DEX §2.1.

Opening L7 for a hop costs 4 SHA-256 compressions the same way; all four siblings (`L6, h45, h0..3, h8..15`) are live witnesses. `h8..15` is opaque in the hop context (no L8 re-derivation needed inside a hop — hops don't bind L8).

Discrepancy note: `BLOCK_ID_ALG_NEW.md:42` still describes L8 as "SHA-256 root". That row was accurate at commit `36cd98721` (2026-07-07); the AN node has since switched L8 to a Poseidon dense-Merkle (matching this circuit's `walk_dense_merkle_bind_pos` at `bridge_event_prove_circuit.rs:810`, and matching DEX §2.4). Treat DEX §2.4 as the source of truth pending a doc refresh on the bridge side.

---

## 3. Per-thread layer-N batch tree (thread 0 only)

**Delta from DEX: none in circuit-level primitives.** The Y-side dense-Merkle-plus-dense-chain flow is already implemented in the single-thread bridge circuit at `bridge_event_prove_circuit.rs:848-876` (`preprocess_dense_proof` → `dense_merkle_root_circuit` → `verify_chain_of_dense_proofs`), operating on the same `HistoryBlockData::calculate_root_hash` layout that DEX §3 describes.

Consumers already speak the ETH-side per-layer window semantics (see [`GLOBAL_HISTORY_DATA_SPEC.md`](../../docs/GLOBAL_HISTORY_DATA_SPEC.md) and [`BRIDGE_PROVER_THINNING_SPEC.md`](../../docs/BRIDGE_PROVER_THINNING_SPEC.md)).

Under this spec, the Y-side reconstruction identically to the current single-thread circuit — the only thing that changes is **whose** `block_id`/`envelope_hash`/`tracked_ext_out_messages_root` feed `block_leaf`. In the multi-thread case those are Y's (thread-0 anchor block), and Y's ext-out-messages root and envelope hash are unconstrained by the X-side.

---

## 4. Cross-thread inclusion proof — the L7 walk

**Delta from DEX: cosmetic (hop primitive itself is identical).** The hop primitive of DEX §4 — variable-depth L7 opening with a witnessed `refs_tree_depth`, `ref_index` range-checked into `1..2^refs_tree_depth`, tagged Poseidon leaf using `REFERENCED_REF_BLOCK_TAG`, 4 SHA compressions for the outer depth-4 block-id opening — is reused **verbatim**. The bridge does **not** need a new hop cryptographic construction.

What changes:

- **No `is_active` gating for salted-endpoint continuity.** In DEX, `is_active[h]` disables the ref-opening and the salted-endpoint equality when a hop is padded. In the bridge, padded hops appear only inside a partially-full trailing snark (§6.5); their `is_active` selector still exists and still gates the SHA-256 + L7 Poseidon opening off, but the "propagation rule" changes: inactive hops enforce `hop_next_block_id == hop_current_block_id` byte-equality directly (no salt to worry about), so a padded hop is just an identity on the clear block-id.
- **Public exposure of `salted_*` endpoints becomes public exposure of the clear `hop_*_block_id`.** Cross-snark continuity is a direct 32-byte equality on the Fr-encoded block-id.

Otherwise: `MAX_PROOF_BLOCK_REFS_DEPTH = 8`, `MAX_PROOF_BLOCK_REFS = 256`, `REFERENCED_REF_BLOCK_TAG` bytes, variable-depth-fold algorithm, `dense_merkle_root_padded_bound` position-binding walker — all inherited unchanged.

Reference implementation of the hop primitive: `dexdo-halo2-kit/dex-halo2-circuit/src/multi_hop_proof.rs`. The three per-hop gadget functions (`prove_hop_ref_tree_opening`, `prove_hop_block_id_reconstruction`, `prove_hop_salted_endpoints`) map onto two-and-a-half functions in the bridge equivalent: the first two are reused verbatim; the salted-endpoint function is replaced by a trivial "publish clear block-id" step. Cost per hop is therefore slightly **lower** than DEX (no Poseidon for salted endpoints).

---

## 5. Full protocol binding scheme

Reading the chain from the withdrawal back to the anchor:

```
WithdrawalInitiated(X)  →  event body BOC (4 cells)
   ↓ (SHA-256: wrapper, body, recipient, sender + 3 child-hash equality links)
event_hash = SHA-256(wrapper cell repr data) = repr_hash(C0)
   ↓ (Poseidon96)
ext_msg_leaf  =  Poseidon96( account_dapp_id ‖ account_id ‖ event_hash )
   ↓ (Poseidon dense-Merkle path, depth ≤ 8, events_pos position-bound)
X.tracked_ext_out_messages_root                        (= X.L8)
   ↓ (block-id tree, depth 4, opens L8; 3 constant siblings + h0..7 witness)
X.block_id                                             (PUBLIC on FinalProof)
   ↓ (L7 walk: L hops, L ∈ [0, L_MAX])
Y.block_id                                             (Y in thread 0; when t=0, Y = X and L = 0)
                                                       (PUBLIC on FinalProof)
   ↓ (Poseidon96)
block_leaf(Y)  =  Poseidon96( Y.block_id ‖ Y.envelope_hash ‖ Y.tracked_ext_out_messages_root )
   ↓ (Poseidon dense-Merkle, depth 8, thread-0 layer-1 batch tree)
#L1(M_Y)
   ↓ (dense chain, ≤ MAX_CHAIN_LEN = 11)
#L<N>(...)  =  finalRoot                               (PUBLIC on FinalProof)
```

Key properties (deltas from DEX §5 called out):

- **X-side is fully algebraic and self-contained.** The bridge prover supplies the same X-side witnesses as DEX (event BOC preimages, ext-out Merkle path, L8 block-id-tree opening) plus the four-cell child-hash chain the bridge already uses (wrapper → body → recipient / sender).
- **The L7 walk is a pure L7 traversal.** Same as DEX. Each hop opens the depth-4 SHA-256 block-id tree of some B, extracts B.L7, and opens one variable-depth Poseidon leaf slot to reveal an edge to some older block A. **`h8..15` is an opaque witness** because hops don't bind L8. Slot 0 (`parent_block_id`) is excluded.
- **Y is a thread-0 block.** Same as DEX. Y's `envelope_hash` and `tracked_ext_out_messages_root` are unconstrained witnesses — Y is the anchor, not the event source.
- **Uniformity for `t = 0`.** The bridge does not enforce uniformity (§1.4). When `t = 0`, the FinalProof simply publishes `X.block_id == Y.block_id` and the bundle contains **zero** hop snarks; the on-chain verifier reads that the FinalProof's X-block-id equals its Y-block-id, skips the hop-chain continuity checks, and proceeds to per-snark verification.
- **Nullifier stays on the X-side.** The existing nullifier `Poseidon(block_id_fr, tokenId, amount, recipientHi, recipientLo, senderAccFr, eventsPos)` now binds `X.block_id_fr` (the event block) rather than a single conflated `block_id`. Semantically unchanged — the withdrawal is uniquely identified by the event's location in the chain, which is X.

---

## 6. Bridge circuit embodiment — multi-proof composition on Ethereum

### 6.1 Why not one big circuit

Same reasoning as DEX §6.1: the L7 walk's SHA-256 cost is dominant, and a 20-hop walk alone is ≈ 28 M advice cells (past the smartphone K ≤ 17 ceiling of DEX §8.1 — though the bridge prover runs on a server, so the argument is subtly different, see §6.1a below). Two composition strategies:

- **(A) In-circuit aggregation** (`AggregationCircuit`): considered — see §8.
- **(B) Multi-proof composition with on-chain orchestration** (this design): produce one `BridgeEventFinalProof` and up to `N_BUNDLE` `BridgeMultiHopProof` snarks. The Ethereum contract checks continuity on the clear block-ids exposed as public inputs, then verifies each snark.

#### 6.1a Prover-side ceiling

The bridge prover runs in `crates/bridge-prover-libraries/bridge-event-prover-lib` on server hardware, not a phone. The DEX K ≤ 17 ceiling therefore does not bind for cost reasons — but it still binds for **on-chain verifier size**. Ethereum SHPLONK verifier bytecode is a monotone function of the circuit shape (see [`sha256_performance_comparison.md`](../../../../../../../.claude/projects/-Users-alinat-HALO2-TVM-EXPERIMENTS/memory/shplonk_aggregator_onchain_size.md) memory: the direct 1B inner verifier is 28 KB at K=17 vs. 21 KB EIP-170 limit, only fitting after SHPLONK aggregation). Keeping per-snark K at 17 with H=5 hops matches the DEX-side envelope, so the existing bridge SHPLONK aggregator toolchain (`bridge-evm-aggregator`, `contracts/ethereum/verifiers/`) applies unchanged.

### 6.2 Circuit definitions

| Circuit | Role | K | Snarks per claim |
|---|---|---|---|
| `BridgeEventFinalProof` | Withdrawal event binding + X-side L8/block-id reconstruction + Y-side thread-0 anchor. Exposes clear X-block-id and Y-block-id in addition to the existing 11 event/nullifier/anchor publics. | **17** (up from single-thread K = 17; adds +4 SHA compressions for the L8 opening) | 1 |
| `BridgeMultiHopProof` | A chain segment of up to `H = 5` hops with `is_active` per hop. Exposes clear start/end block-ids. Per-hop constraints reuse DEX §4.2 verbatim. | **17** | `ceil(L / H)`, 0 when `L = 0` |

Both circuits live in the same `bridge-event-prove-circuit` crate as sibling `Circuit` implementations, sharing the same `BaseCircuitParams` conventions and `Sha256Chip`/`gosh-dense-balanced-tree` toolchain the current code uses.

**Shared helper — `dense_merkle_bound.rs`.** Same rationale as DEX §6.2: the upstream `gosh-dense-balanced-tree` walker lets the prover pick direction bits freely, which is unsound when the leaf position is also exposed. The bridge crate already vendors this helper (`bridge_event_prove_circuit.rs:116`, `walk_dense_merkle_bind_pos`) for the events-tree walk; the L7 hop walk (new) will reuse it, and the L8 block-id-tree opening (new) also needs it if any tree slot other than L8 might be opened (in practice only L8 is opened, so the position is a constant `8` and a plain walker suffices).

### 6.3 Clear (unsalted) endpoints — on-chain continuity

The DEX spec devotes §6.3 to salted endpoints because event blocks must remain unlinkable. The bridge has **no anonymity requirement** — the withdrawal is settled by paying an Ethereum address, the recipient is already public in slot `PUB_RECIPIENT_*`, and the source of funds is also on-chain in AN. Exposing X.block_id and Y.block_id costs nothing.

Therefore, each snark exposes clear 32-byte block-ids as its glue instances:

#### Per-`BridgeMultiHopProof` public inputs (2 Fr)

```
inst[0] = hop_start_block_id   =  bytes_to_fr( B_0.block_id )        // Fr-encoded LE
inst[1] = hop_end_block_id     =  bytes_to_fr( B_H.block_id )        // Fr-encoded LE
```

**No `salt_commitment`, no `bundle_index`, no position tag.**

Cross-snark continuity is a plain field equality: `hopProofs[i].publicInputs[1] == hopProofs[i+1].publicInputs[0]`.

#### Per-`BridgeEventFinalProof` public inputs (13 Fr)

Delta from the current 11-slot layout: **add two new instances at the end** (`PUB_X_BLOCK_ID`, `PUB_Y_BLOCK_ID`) so the ordering of the existing 11 slots is preserved and existing on-chain code + fixture consumers keep their offsets. Alternatively, insert them adjacent to `PUB_FINAL_ROOT` for readability — the choice is a downstream ergonomics call; both work.

Recommended layout (append-only):

```
[0]   token_id                 (unchanged: BE u32 from body[54..58))
[1]   amount                   (unchanged: BE u128 from body[38..54))
[2]   recipientHi              (unchanged: BE u80  from recipient[2..12))
[3]   recipientLo              (unchanged: BE u80  from recipient[12..22))
[4]   dstChainId               (unchanged: BE u256 from body[6..38))
[5]   senderAccFr              (unchanged: algebraic decode from sender cell)
[6]   dappFr                   (unchanged: destination dApp id, Fr-encoded)
[7]   accFr                    (unchanged: destination account id, Fr-encoded)
[8]   nullifier                (unchanged spec, now binds X.block_id — see §6.7)
[9]   finalRoot                (unchanged: output of Y-side dense chain)
[10]  anchorLayer              (unchanged: 1-indexed layer, range 1..=10)
[11]  x_block_id_fr            (NEW: bytes_to_fr(X.block_id), Fr-encoded LE)
[12]  y_block_id_fr            (NEW: bytes_to_fr(Y.block_id), Fr-encoded LE)
TOTAL_PUBLIC_INPUTS = 13
```

Rationale for exposing both `x_block_id_fr` and `y_block_id_fr`:

- **`x_block_id_fr`** is the head of the chain — `hopProofs[0].hop_start_block_id` must equal it. Also the value the on-circuit nullifier binds to, so exposing it lets a fraud-proof verifier or an off-chain auditor re-derive the nullifier from the observable event and instantly detect a mismatch.
- **`y_block_id_fr`** is the tail — `hopProofs[last].hop_end_block_id` must equal it. It's the block whose `block_leaf` feeds the Y-side dense-chain walk that produces `finalRoot`.
- **`t = 0` case** — `x_block_id_fr == y_block_id_fr` and the bundle contains zero hop snarks; the on-chain check degenerates to `require(x_block_id_fr == y_block_id_fr)`, no continuity walk.

**No DEX-style contract-identity pins are added.** The bridge already exposes `dappFr` / `accFr` as the destination (Acki-Nacki-side bridge contract's dApp id + account id), which serves the same role as the DEX contract-identity pins — the Ethereum verifier compares them against the pre-committed AN bridge address.

### 6.4 Ethereum orchestration

The AckiNacki bridge `withdrawByProof` entrypoint currently accepts a single Circuit-4 SHPLONK-aggregated proof (`contracts/ethereum/src/AckiNackiBridge.sol`). Under this spec it accepts a **bundle** — one `BridgeEventFinalProof` + `n ∈ [0, N_BUNDLE]` `BridgeMultiHopProof` snarks — and enforces:

```solidity
function withdrawByProofBundle(
    FinalProofData calldata finalProof,   // Circuit-4 replacement
    MultiHopProofData[] calldata hopProofs,
    // ...existing calldata (recipient signature, etc.)...
) external {
    // Cheap public-input consistency FIRST (fail-fast, no crypto yet)

    // 1a. Nullifier not spent
    bytes32 nullifier = finalProof.publicInputs[PUB_NULLIFIER];
    require(!spent[nullifier], ERR_ALREADY_WITHDRAWN);

    // 1b. Anchor known in on-chain window
    require(
        _isKnownLayerAnchor(
            finalProof.publicInputs[PUB_FINAL_ROOT],
            uint8(finalProof.publicInputs[PUB_ANCHOR_LAYER])
        ),
        ERR_UNKNOWN_ANCHOR
    );

    // 1c. Chain continuity — CLEAR block-id equality (no salt)
    bytes32 xBlockId = finalProof.publicInputs[PUB_X_BLOCK_ID];
    bytes32 yBlockId = finalProof.publicInputs[PUB_Y_BLOCK_ID];
    if (hopProofs.length == 0) {
        require(xBlockId == yBlockId, ERR_MISSING_HOP_FOR_CROSS_THREAD);
    } else {
        require(
            hopProofs[0].publicInputs[PUB_HOP_START] == xBlockId,
            ERR_X_HEAD_MISMATCH
        );
        for (uint i = 0; i + 1 < hopProofs.length; ++i) {
            require(
                hopProofs[i].publicInputs[PUB_HOP_END]
                    == hopProofs[i+1].publicInputs[PUB_HOP_START],
                ERR_CHAIN_BREAK
            );
        }
        require(
            hopProofs[hopProofs.length - 1].publicInputs[PUB_HOP_END] == yBlockId,
            ERR_Y_TAIL_MISMATCH
        );
    }

    // 1d. Destination identity: pinned dApp / account
    require(
        finalProof.publicInputs[PUB_DAPP_FR] == EXPECTED_BRIDGE_DAPP_FR &&
        finalProof.publicInputs[PUB_ACC_FR]  == EXPECTED_BRIDGE_ACC_FR,
        ERR_WRONG_BRIDGE_CONTRACT
    );

    // Expensive Halo2 KZG SHPLONK verifications — only after the cheap checks pass
    require(verify_final(finalProof), ERR_INVALID_FINAL_PROOF);
    for (uint i = 0; i < hopProofs.length; ++i) {
        require(verify_multi_hop(hopProofs[i]), ERR_INVALID_HOP_PROOF);
    }

    // Settle
    spent[nullifier] = true;
    _payoutWithdrawal(finalProof.publicInputs);
}
```

**Verifier keys.** Two new Yul verifier contracts are generated by the existing `bridge-evm-aggregator` pipeline (`export-inner-aggregator` / `aggregate-proof`): one for `BridgeEventFinalProof` (replacing the current Circuit-4 verifier, since PIs grew from 11 to 13), one for `BridgeMultiHopProof` (new artifact). Both slot into `contracts/ethereum/verifiers/`. A verification-key rotation is a breaking change per repo policy — call it out in `CHANGELOG.md` under `## [Unreleased]` when the multi-thread PR lands.

**Alternative — single outer aggregation.** The bundle could be reduced to one on-chain verification by an additional outer SHPLONK aggregator that takes the FinalProof + up to `N_BUNDLE` hop snarks as inputs. This trades one more prover-side proof (server, minutes) for `N_BUNDLE × 750K ≈` up to 3–4 M gas savings per withdrawal. Recommended follow-up once the base design is in production; not on the critical path for the initial multi-thread landing.

### 6.5 Dynamic bundle size — no uniformity padding

`N_BUNDLE_MAX = ceil(L_MAX / H)` is the **upper bound**, not a fixed shape. Per-claim count:

| True chain length L | Hop snarks | Total snarks in bundle |
|---|---|---|
| 0  (t = 0)          | 0 | 1 (Final only)          |
| 1..5                | 1 | 2                        |
| 6..10               | 2 | 3                        |
| 11..15              | 3 | 4                        |
| 16..20              | 4 | 5                        |
| ...                 | ceil(L/H) | 1 + ceil(L/H)     |
| 296..300 (prod cap) | 60 | 61                      |

Trailing snark padding: when `L mod H ≠ 0`, the last hop snark has `L mod H` active hops + `H - (L mod H)` padded hops. Padded hops use `is_active[h] = 0` and enforce `hop_next_block_id == hop_current_block_id` (byte-equality) so the chain propagates through them as identity.

Contrast with DEX §6.5: DEX pads to constant `N_BUNDLE` for anonymity uniformity. Bridge drops that. An observer can trivially read L from `hopProofs.length` — **that leak is acceptable in the bridge threat model** (§9).

### 6.6 `BridgeMultiHopProof` circuit detail

At K = 17 with H = 5 hops. Structural mirror of `MultiHopProofCircuit` in `dexdo-halo2-kit/dex-halo2-circuit/src/multi_hop_proof.rs` **minus** the salt / salted-endpoint machinery.

```
witnesses:
  for h in 0..H:
    is_active[h]                                    (bool, assert_bit)
    hop_current_block_id[h]                         (32 bytes)
    hop_next_block_id[h]                            (32 bytes)
    B_h.L0..L7_root, B_h.L8                         (9 × 32 bytes; needed to reconstruct B_h.block_id via 4 SHA compressions)
    refs_tree_depth[h] ∈ [0, MAX_PROOF_BLOCK_REFS_DEPTH]   (u8; range-checked)
    ref_index[h] ∈ [1, 2^refs_tree_depth[h])        (u32; range-checked)
    L7_inner_path[h]                                (8 × 32 bytes; unused steps ignored)

constraints:
  1. For each hop h in 0..H:
       when is_active[h]:
         - Reconstruct B_h.block_id from B_h.L0..L7, L8 via depth-4 SHA-256 tree (4 SHA compressions).
         - Constrain hop_current_block_id[h] == B_h.block_id (byte equality).
         - Tagged Poseidon leaf: ref_leaf = Poseidon(bytes_to_fr(REFERENCED_REF_BLOCK_TAG || A.block_id))
           where A.block_id = hop_next_block_id[h].
         - Variable-depth L7 fold: verify B_h.L7_root == open(ref_leaf, ref_index[h], L7_inner_path[h], refs_tree_depth[h]).
         - (Direction bits inside the walker are bound to bit-decomposition of ref_index[h] via `dense_merkle_root_padded_bound`.)
       when !is_active[h]:
         - Byte equality: hop_next_block_id[h] == hop_current_block_id[h]  (identity propagation)
         - All other per-hop crypto constraints selector-multiplied off.

  2. Intra-snark continuity (unconditional):
       for h in 0..H-1: hop_current_block_id[h+1] == hop_next_block_id[h]

  3. Publish public instances (clear Fr, LE-packed):
       inst[0] = bytes_to_fr(hop_current_block_id[0])       ← PUB_HOP_START
       inst[1] = bytes_to_fr(hop_next_block_id[H-1])        ← PUB_HOP_END
       (Fr-encoding via `bytes_to_fr` = the existing `gosh_dense_balanced_tree::bytes_to_fr` convention.)
```

**Cell budget.** H = 5 hops × (4 SHA + variable-depth Poseidon fold + range checks) ≈ 20 SHA compressions × 354 K + Poseidon overhead ≈ **7.1 M advice cells** — matches the DEX MultiHopProof envelope of DEX §6.6. K = 17 with ~110 advice columns gives ~14 M cells → ~49% margin.

**Not-a-witness.** No `salt`, no `voucher_secret_seed`, no `bundle_index`, no `salt_commitment` publication. Roughly 4–6 Poseidon calls per snark are removed vs. DEX's `MultiHopProofCircuit` (the salt derivation and per-endpoint salting), and per-hop `salted_start` / `salted_end` computations are dropped in favour of plain `bytes_to_fr` on the clear block-ids.

### 6.7 `BridgeEventFinalProof` circuit detail

At K = 17. Evolution of the existing `BridgeEventProveCircuit` (`bridge_event_prove_circuit.rs`) with three additions:

1. **X-side L8 opening (new).** After the existing `ext_out_root` is computed by `walk_dense_merkle_bind_pos` at `:810`, treat that value as `X.L8` and open it up the depth-4 block-id tree to derive `X.block_id`. Costs 4 SHA-256 compressions (§2). Three of the four required siblings are protocol-fixed constants (`L9 = 0×32`, `h10..11 = SHA(0×32 ‖ 0×32)`, `h12..15 = SHA(h10..11 ‖ h10..11)`) hard-coded into the circuit; the fourth (`h0..7`) is a live witness.

2. **Splitting the `block_id` witness into X and Y (new).** The current circuit has a single `block_id` field (line 346) that (a) feeds `block_leaf` on the Y-side and (b) feeds the nullifier on the X-side. In the multi-thread build, these become two distinct witnesses:
   - `x_block_id`: bound to the X-side by the L8 opening above. Feeds the nullifier (unchanged semantics).
   - `y_block_id`: bound to the Y-side by the existing `block_leaf` construction and the dense-chain walk. Y's `envelope_hash` and `tracked_ext_out_messages_root` remain unconstrained witnesses on the Y-side.
   - When `t = 0`, the prover passes identical bytes for both; the L8 opening still binds `x_block_id`, and Y-side gates independently bind the same value on `y_block_id`. No special-case circuit logic is needed for `t = 0`.

3. **Two new public instances (`PUB_X_BLOCK_ID`, `PUB_Y_BLOCK_ID`).** Append at the end of the instance vector (see §6.3 layout) so slots 0..10 stay bit-identical to the current single-thread circuit.

Everything else — the 4-cell BOC SHA-256 chain, the ABI event id constraint, the `d1` refs_count decompositions, the field extractions (`tokenId`, `amount`, `dstChainId`, `recipientHi/Lo`, `senderAccFr`), the destination `dappFr`/`accFr` derivation, the events-tree walker with `walk_dense_merkle_bind_pos`, `block_leaf` construction, `dense_merkle_root_circuit` for the block Merkle proof, `verify_chain_of_dense_proofs` for the dense chain, the `nullifier` Poseidon call, and the `anchorLayer` range-check + publication — is inherited **verbatim** from the current single-thread circuit code.

Explicit constraint list (numbering continues from single-thread flow at `bridge_event_prove_circuit.rs:71-93`):

```
existing:
  1..11. Single-thread pipeline (SHA of 4 cells, 3 child-hash equality links,
         ABI id, d1 refs_count, ext_msg_leaf, events-tree walk to ext_out_root,
         block_leaf, block Merkle proof, dense chain to final_root, nullifier,
         anchorLayer range-check). Unchanged.

new:
 12. X.block_id reconstruction (depth-4 SHA tree opening L8; 4 SHA compressions):
        h89     = SHA(X.L8 ‖ 0×32)                         // L9 = 0×32 (constant)
        h8_11   = SHA(h89   ‖ H10_11_CONST)                 // sibling constant
        h8_15   = SHA(h8_11 ‖ H12_15_CONST)                 // sibling constant
        X.block_id_recomputed == SHA(X_block_id_h07_sibling ‖ h8_15)  // h0..7 witness
        (Here X.L8 IS the ext_out_root cell produced by walk_dense_merkle_bind_pos
        at line :810 — no new witness is introduced; the existing ext_out_root
        is the tree leaf being opened.)

 13. Bind X.block_id publication:
        constrain_equal(X.block_id_recomputed, bytes_to_fr_of(x_block_id_witness))
        x_block_id_fr_public == bytes_to_fr(x_block_id_witness)

 14. Nullifier binds X.block_id (rewire, no algorithmic change):
        block_id_fr (input to Poseidon nullifier at line :972) := x_block_id_fr

 15. Bind Y.block_id publication:
        y_block_id_fr_public == bytes_to_fr(y_block_id_witness)
        (y_block_id_witness feeds block_leaf at line :834; that binding is the existing constraint.)
```

**Cell budget.** Adds ~1.4 M advice cells (4 SHA @ 354 K each) to the single-thread K = 17 circuit. Current circuit already occupies most of K = 17; the addition may require bumping to K = 18, **or** bumping the advice column count from 110 to ~150 within K = 17. K sweep required to lock the choice before landing (`test_k_sweep_benchmark` pattern from `dark_dex_circuit_new`).

If the K bump is chosen, K = 18 grows the on-chain SHPLONK verifier proportionally — verify against EIP-170 during the aggregator export.

### 6.8 Bundle size and prover time

Prover runs on server (not phone), so this is a throughput question, not a UX question.

| True chain length L | Hop snarks | Total snarks | Est. server prover time (parallel) |
|---|---|---|---|
| 0                    | 0 | 1  | ≈ 1–2 min                           |
| 1..5                 | 1 | 2  | ≈ 2–3 min                            |
| 6..10                | 2 | 3  | ≈ 3–4 min (parallelised across hops) |
| 11..20               | 3..4 | 4..5 | ≈ 4–6 min                       |
| L_MAX = 300          | 60 | 61 | ≈ 15–30 min (heavy parallelism)     |

Numbers are order-of-magnitude, calibrated from DEX-side stress runs (`test_bundle_stress_l{50,100,300}.rs`) which use the same hop primitive. Hop snarks are embarrassingly parallel; the FinalProof is on the critical path.

### 6.9 Verifying-key set

Two VKs: `VK_BridgeEventFinal`, `VK_BridgeMultiHop`. No aggregation, no universal VK, no recursion. Both are consumed by the existing `bridge-evm-aggregator` SHPLONK pipeline to produce Yul verifier contracts under `contracts/ethereum/verifiers/`.

**Key rotation is a breaking change** per bridge repo policy (`AGENTS.md` §Changelog policy). The multi-thread landing PR must document:

- Retirement of the current single-thread Circuit-4 VK.
- Introduction of `VK_BridgeEventFinal` and `VK_BridgeMultiHop`.
- Any prover-side artifact schema changes (`proof_event_*.json`, witness JSON).

---

## 7. Synthetic test data generator

A binary in `bridge-event-prove-circuit/examples/` (new — pattern-match against `dexdo-halo2-kit/dex-halo2-circuit/examples/`) that produces multi-thread fixtures for every supported configuration:

| Case | t     | L (real hops) | Hop snarks | Notes |
|------|-------|---------------|------------|-------|
| S0   | 0     | 0             | 0          | Same-thread; X = Y; hop-less bundle |
| S1   | ≠ 0   | 1             | 1 (1 active hop, 4 padded) | Shortest cross-thread |
| S5   | ≠ 0   | 5             | 1 (5 active hops)         | Single fully-active hop snark |
| S6   | ≠ 0   | 6             | 2 (5 + 1 active)          | Boundary into 2-snark regime |
| S15  | ≠ 0   | 15            | 3 (5 + 5 + 5)             | Mid-range |
| S20  | ≠ 0   | 20            | 4 (5 + 5 + 5 + 5)         | Worst case at prototyping `L_MAX = 20` |
| Sprod | ≠ 0  | 300           | 60                        | Production stress (mark `#[ignore]`) |

Each fixture emits:

1. Witnesses + native proof for `BridgeEventFinalProof`.
2. Witnesses + native proofs for the applicable `BridgeMultiHopProof` snarks.
3. Native bundle verification (Rust mock of `withdrawByProofBundle`, mirror `dex-halo2-circuit/src/bundle_verifier.rs`).

All fixtures use one fixed `VK_BridgeEventFinal` + one fixed `VK_BridgeMultiHop`.

Reuse the existing `test_helpers::*` synthetic-witness builders where the shape overlaps with the single-thread flow. New helpers required: L7 walk fixture builder, L8 depth-4 tree opening fixture builder — both can be lifted from the DEX-side test-data-gen with the salt paths removed.

---

## 8. Why not `AggregationCircuit`

Same rejection rationale as DEX §8, **plus** two bridge-specific concerns:

- **Ethereum verifier size.** In-circuit KZG aggregation would push K to 21+, blowing past the EIP-170 24 KB limit even after SHPLONK wrap. The current on-chain path already relies on SHPLONK aggregation to fit; layering an in-circuit aggregator on top would either require nested SHPLONK or force a switch to a proxy-verifier pattern.
- **Bundle verification cost budget.** Per-snark SHPLONK verify ≈ 700–800 K gas. Worst case at L = 20 → 5 snarks → ~3.75 M gas. At L = 300 → 61 snarks → ~46 M gas — **too expensive per single withdrawal**, so at L ≥ some threshold an outer-SHPLONK bundle aggregator becomes mandatory (§6.4 alternative). Trade the on-chain cost for one extra prover-side aggregation round; still cheaper than in-circuit aggregation on the phone-analogous DEX rationale.

For the initial multi-thread landing, target the multi-proof composition with an option to enable outer-SHPLONK aggregation once fixtures + verifier fit-tests are green.

---

## 9. What is exposed vs. hidden (no anonymity goal)

**The bridge does not aim for anonymity.** The DEX §9 anonymity analysis is replaced by a small "what leaks" summary — this design section exists so future readers don't add anonymity assumptions the design does not support.

### 9.1 Exposed on-chain

- **`X.block_id`, `Y.block_id`** — clear Fr-encoded 32-byte values as public inputs. An observer learns exactly which AN block emitted the withdrawal event and which thread-0 anchor was used.
- **`hop_start_block_id`, `hop_end_block_id`** on every hop snark — the entire cross-thread walk is public.
- **`hopProofs.length`** — reveals L (chain length from X to thread 0). Not a threat: L is a physical chain topology fact, unrelated to any user identity.
- **All withdrawal fields**: `tokenId`, `amount`, `recipient` (address), `dstChainId`, `senderAccFr` — public per the event ABI; the bridge is a public-good primitive on both sides.
- **`nullifier`** — public per spent-set tracking.
- **`finalRoot`, `anchorLayer`** — public per the layer-window mechanism.

### 9.2 Hidden

- **`X.envelope_hash`, `X.tracked_ext_out_messages_root`** — witnessed, not exposed (would leak nothing useful, but also serve no purpose to expose).
- **`Y.envelope_hash`, `Y.tracked_ext_out_messages_root`** — unconstrained witnesses; Y is only used as an anchor, not as an event source.
- **Ext-out-message tree slot `eventsPos`** — private witness bound to the walker's direction bits (`walk_dense_merkle_bind_pos`), enters the nullifier for replay disambiguation only.
- **Individual `refs[]` list entries** at each hop — only the specific `ref_block_id` opened per hop is disclosed (as `hop_next_block_id` = clear public input), other refs stay opaque.

### 9.3 Replay protection (nullifier)

`nullifier = Poseidon(x_block_id_fr, tokenId, amount, recipientHi, recipientLo, senderAccFr, eventsPos)` — unchanged from the single-thread circuit modulo the `block_id → x_block_id` rewire. Because `x_block_id` is now also a public input, the nullifier is verifier-computable from the public instance vector, which lets an off-chain observer audit the spent-set independently.

The nullifier binds the withdrawal to:

- the source event block (`x_block_id_fr`) — no more block-level anonymity, per §9.1;
- every settled withdrawal field (`tokenId`, `amount`, `recipient`, `sender`) — full-tuple replay protection;
- the event's in-block position (`eventsPos`) — disambiguates two identical `WithdrawalInitiated` events in one AN block.

---

## 10. Locked parameters and open questions

### 10.1 Locked

| Parameter | Value | Source / rationale |
|---|---|---|
| BWS | 128 | `HISTORY_PROOF_WINDOW_SIZE` (canonical) |
| Layer-1 batch tree depth | 8 (130 real leaves + 126 padding = 256) | Existing `verify_chain_of_dense_proofs` flow |
| Block-id tree depth | **4** (16 leaves) | Shared with DEX; see [`BLOCK_ID_ALG_NEW.md`](../../docs/BLOCK_ID_ALG_NEW.md) |
| L9..L15 padding | `[0u8; 32]` | Chain constant |
| L8 semantics | `tracked_ext_out_messages_root` (Poseidon dense-Merkle root; **not** SHA — see §2 discrepancy note) | Current bridge circuit + DEX §2.4 |
| Ext-out-messages tree | Poseidon dense-Merkle, depth ≤ 8 (`MAX_EVENTS_TREE_DEPTH`), padded with `[0u8; 32]` | `bridge_event_prove_circuit.rs:124` |
| L7 outer opening depth per hop | **4** SHA-256 sibling combines | §4 |
| `MAX_PROOF_BLOCK_REFS` | **256** leaves padded, depth 8 | Protocol cap |
| `H` (hops per `BridgeMultiHopProof`) | **5** | Match DEX for shared toolchain sizing |
| `L_MAX` (max real chain length) | **20** (prototyping) → **300** (production) | Node-team ceiling |
| `N_BUNDLE_MAX` (max hop snarks per claim) | **4** (prototyping) → **60** (production) | Dynamic per-claim, upper bound only |
| `MAX_CHAIN_LEN` (thread-0 dense chain) | **11** | `gosh-dense-balanced-tree` |
| `MAX_ANCHOR_LAYER` | **10** | Must equal `MAX_LAYER_HASHES` in `AckiNackiBridge.sol` |
| `MAX_EVENTS_TREE_DEPTH` | **8** | `bridge_event_prove_circuit.rs:124` |
| `BridgeMultiHopProof` K | **17** | Match DEX MultiHopProof |
| `BridgeEventFinalProof` K | **17 (with column bump)** or **18** — decide via K sweep | +4 SHA compressions vs. current single-thread K=17 |
| SHA-256 chip | `gosh-sha256-chip` | Existing dependency |
| On-chain verifier | per-snark Halo2 KZG via existing SHPLONK aggregator | No aggregation initially |
| Public inputs (`BridgeEventFinalProof`, 13) | see §6.3 | Preserves 11-slot prefix; appends X/Y block-ids |
| Public inputs (`BridgeMultiHopProof`, 2) | see §6.3 | New artifact |

### 10.2 Open questions

1. **K sweep on `BridgeEventFinalProof`.** Decide K=17 with wider advice columns vs. K=18. Blocker: run `test_k_sweep_benchmark` analogous to `dark_dex_circuit_new::test_k_sweep_benchmark` before locking the SHPLONK aggregator wiring.
2. **Outer-SHPLONK bundle aggregation.** Whether to ship the multi-thread landing with per-snark on-chain verification (simple; ~3.75 M gas at L=20) or with outer aggregation (one on-chain verify; extra prover round). See §6.4 alternative. Recommendation: ship per-snark first, add outer aggregation as a follow-up once fixtures + verifier fit-tests are green.
3. **Naming convention for the new final circuit.** Keep `BridgeEventProveCircuit` (existing name evolved) vs. rename to `BridgeEventFinalProofCircuit` (matches DEX taxonomy). Downstream consumers (`bridge-event-prover-lib`, `bridge-event-witness`) will need mechanical updates either way; the choice is ergonomic.
4. **Ethereum-side `withdrawByProofBundle` entrypoint.** Whether to add a new function name (backwards-compatible during rollout) or repurpose `withdrawByProof` (cleaner, but rotates the ABI). Coordinate with `contracts/ethereum/` and `docs/EVM-contracts-spec.md`.
5. **Handling `t = 0` on Ethereum without a special-case branch.** The layout above requires `xBlockId == yBlockId` when `hopProofs.length == 0`. Confirm this Solidity branch is well-formed under gas / calldata reasoning — an alternative is to always require at least one hop snark, using a "identity hop" for `t = 0`, at the cost of one extra 2-instance snark per claim.

---

## 11. Circuit implementation status

**Not started.** This document is the design step for the multi-thread landing on the `feature/multithreading` branch. The single-thread `BridgeEventProveCircuit` is fully implemented in `bridge_event_prove_circuit.rs`; extending it to multi-thread requires:

### 11.1 Circuit code — **TODO**

- **New file** `bridge-event-prove-circuit/src/multi_hop_proof.rs` — port `MultiHopProofCircuit` from `dexdo-halo2-kit/dex-halo2-circuit/src/multi_hop_proof.rs` with the salt path removed and the salted-endpoint publication replaced by clear block-id publication. Reuse `dense_merkle_root_padded_bound` and `Sha256Chip` per DEX.
- **New file** `bridge-event-prove-circuit/src/multi_hop_witness.rs` — mirror `dexdo-halo2-kit/dex-halo2-circuit/src/multi_hop_witness.rs` sans salt.
- **Extend** `bridge-event-prove-circuit/src/bridge_event_prove_circuit.rs`:
  - Split `block_id` → `x_block_id` + `y_block_id`.
  - Add L8 depth-4 SHA opening after `ext_out_root` computation.
  - Add `PUB_X_BLOCK_ID`, `PUB_Y_BLOCK_ID` to the instance vector; bump `TOTAL_PUBLIC_INPUTS` from 11 to 13.
  - Rewire nullifier to consume `x_block_id_fr`.
  - K sweep to lock K + advice column count.
- **New file** `bridge-event-prove-circuit/src/bundle_verifier.rs` — pure-Rust mock of the Ethereum `withdrawByProofBundle` acceptance gate, driving synthetic + real-prover bundle E2E tests. Mirrors `dex-halo2-circuit/src/bundle_verifier.rs`.
- **Extend** `bridge-event-prove-circuit/src/test_helpers.rs` — L7 walk fixture builder, L8 opening fixture builder, bundle-level assembly.

### 11.2 Off-tree work — **TODO**

- **Ethereum contract updates.** `AckiNackiBridge.sol` gains `withdrawByProofBundle` (or extension of `withdrawByProof`); two new verifier Yul contracts under `contracts/ethereum/verifiers/`; register new VKs.
- **SHPLONK aggregator export.** Run `bridge-evm-aggregator export-inner-aggregator` for both new circuits; check verifier bytecode against EIP-170.
- **Prover-side integration** (`bridge-event-prover-lib`, `bridge-event-witness`, `bridge-relayer-daemon`): witness builder for multi-thread claim bundles, per-claim dispatch of the correct number of hop snarks, on-demand PK loading (see [`memory/bridge_prover_daemon.md`](../../../../../../../.claude/projects/-Users-alinat-HALO2-TVM-EXPERIMENTS/memory/bridge_prover_daemon.md) for the existing dual-circuit prover pattern), retry / idempotency for partial bundle failures.
- **Changelog + docs**: `CHANGELOG.md` breaking-change entry (new VKs, ABI additions); `docs/EVM-contracts-spec.md` update; `deposit-prover/README.md`-style overview for the multi-thread circuit under `bridge-event-prove-circuit/README.md`.
- **CI**: `.woodpecker/bridge-circuits.yaml` gains a heavy-step `#[ignore]` bundle stress test to mirror the DEX `test_bundle_stress_l{50,100,300}.rs` cases.

### 11.3 Known deferrable items

- **Upstream 4-bit hardcode** in `gosh-dense-balanced-tree::dense_merkle_root_circuit_padded` (see DEX §11.3). Same issue applies to the new hop circuit here — no cheating window at `MAX_PROOF_BLOCK_REFS_DEPTH = 8`, but track the parameterisation fix in `gosh-halo2-crypto-lib` when next touched.
- **Outer-SHPLONK bundle aggregator** (§6.4 alternative). Deferred until per-snark verification is landed and stable.

---

## 12. Migration notes from the single-thread bridge circuit

For reviewers who know the current `BridgeEventProveCircuit` code, this section enumerates the diff-level changes:

| Current (`bridge_event_prove_circuit.rs`) | Multi-thread |
|---|---|
| Single `block_id: [u8; 32]` witness (line 346) | Split into `x_block_id` (bound via L8 opening) and `y_block_id` (bound via `block_leaf`) |
| `ext_out_root` (line 810) → discarded after feeding `block_leaf` (line 822) | Same value now ALSO fed as L8 leaf into a new 4-SHA depth-4 opening producing `x_block_id_fr` |
| `block_id_fr` witness feeds both `block_leaf` (line 822) and nullifier (line 972) | `x_block_id_fr` → nullifier; `y_block_id_fr` → `block_leaf` |
| `TOTAL_PUBLIC_INPUTS = 11`  (line 152) | `TOTAL_PUBLIC_INPUTS = 13`; append `PUB_X_BLOCK_ID = 11`, `PUB_Y_BLOCK_ID = 12` |
| Circuit K = 17, ~110 advice columns | K = 17 with wider advice OR K = 18; K sweep decides |
| Circuit file structure: single circuit | Add sibling `multi_hop_proof.rs`, `multi_hop_witness.rs`, `bundle_verifier.rs` |
| On-chain: one Circuit-4 SHPLONK verifier | Two Yul verifiers (final + hop); orchestration in `withdrawByProofBundle` |
| Downstream witness builder produces one witness per event | Produces a bundle (1 final + `ceil(L/H)` hop witnesses) |

Every existing public-input slot 0..10 keeps its current byte-for-byte semantics — downstream consumers that hardcode `PUB_TOKEN_ID..PUB_ANCHOR_LAYER` do not need to move, they only need to grow their vector length from 11 to 13 and read the two new tail slots.

---

## 13. Cross-repository references

- **DEX companion spec:** `dexdo-halo2-kit/MULTITHREAD_DEX_CIRCUIT_SPECIFICATION.md` (shared primitives, full anonymity model, aggregation-vs-multi-proof analysis).
- **DEX circuit code being ported:** `dexdo-halo2-kit/dex-halo2-circuit/src/{multi_hop_proof.rs, multi_hop_witness.rs, dark_dex_circuit.rs, bundle_verifier.rs, dense_merkle_bound.rs, salt.rs}` — the last one intentionally *not* ported.
- **Bridge single-thread circuit being extended:** `crates/bridge-circuits/bridge-event-prove-circuit/src/bridge_event_prove_circuit.rs`.
- **Bridge block-id doc (slice of the DEX spec):** [`crates/bridge-circuits/docs/BLOCK_ID_ALG_NEW.md`](../../docs/BLOCK_ID_ALG_NEW.md).
- **Bridge global-history-data doc (Y-side anchor infrastructure):** [`crates/bridge-circuits/docs/GLOBAL_HISTORY_DATA_SPEC.md`](../../docs/GLOBAL_HISTORY_DATA_SPEC.md).
- **Bridge thinning spec (window / anchor cadence context):** [`crates/bridge-circuits/docs/BRIDGE_PROVER_THINNING_SPEC.md`](../../docs/BRIDGE_PROVER_THINNING_SPEC.md).
- **AN node source of truth for block Merkle leaves:** `node/src/types/ackinacki_block/{mod.rs, merkle.rs}` on `acki-nacki@feature/node-3953-add-test-slow-block-builder-with-300ms-per-block-build-on state_v2`.
- **AN node source of truth for L7 / layer-N trees:** `node/libs/history-proof/src/lib.rs` (same repo/branch).
- **Off-chain reference chain-walk implementation:** `helpers/proof_helper/src/gql_proof.rs` (same repo/branch).
- **Ethereum on-chain verifier:** `contracts/ethereum/src/AckiNackiBridge.sol` (`_isKnownLayerAnchor`, `withdrawByProof`).
- **SHPLONK aggregator tooling:** `crates/bridge-evm-aggregator`.
