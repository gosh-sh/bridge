# Multi-Thread Cryptographic Scheme — Bridge Event-Prove Circuit

Target embodiment: `bridge/crates/bridge-circuits/bridge-event-prove-circuit` (AN → ETH bridge Circuit 4).

This document describes the cryptographic mechanism for proving, in zero knowledge, that a `WithdrawalInitiated` event in **any thread t** of Acki Nacki can be anchored, via cross-thread chaining when `t ≠ 0`, against a layer-N batch root retained by the Ethereum-side `AckiNackiBridge` per-layer window. It also specifies how the bridge Circuit 4 embodies that mechanism.

The document is self-contained. A bridge developer should be able to implement the multi-thread Circuit 4 from this text alone, without reading any companion specification.

## 0. Terminology

### 0.1 Notation convention — the `→` arrow

Throughout this document, an arrow

```
A  →  B
```

means **"A is a leaf in B's L7 Poseidon dense-Merkle tree, i.e.
`A.block_id` appears in `B.proof_block_refs` at some slot `k ≥ 0`"**.
The tail of the arrow is the *older* block (the leaf of the per-block
L7 tree); the head is the *newer* block (the root direction, which
Merkle-commits to the leaf through its L7 opening).

This matches the standard Merkle-tree convention: `leaf → root` folds
upward. The convention is uniform across both scales in this document:

- **Within a block** — folding L7 (or L8) up to `block_id` goes
  `leaf → root`, e.g. `L8 → h89 → h8..11 → h8..15 → block_id`.
- **Between blocks** — a hop step `A → B` points from the leaf side
  (older block, referenced inside B.L7) to the root side (newer block,
  containing A as a ref).

Reading a walk chain left-to-right therefore reads **oldest → newest**.
Because acki-nacki `proof_block_refs` always point at strictly older
blocks (slot 0 = same-thread parent, slots ≥ 1 = cross-thread older
blocks — see §2.3,
`node/src/multithreading/thread_synchrinization_service.rs:67-77` and
`helpers/proof_helper/src/gql_proof.rs:76,109`), the oldest-to-newest
direction is well-defined.

**Walk chain.** The walk starts at the event block **X** (thread t,
oldest / leaf side) and terminates at the anchored thread-0 block
**Y** (newest / root side) — Y is where the chain of leaf-to-root
openings finally hits a block whose own `block_leaf` lands inside
`layerWindows[]` on Ethereum. Written left-to-right:

```
X  →  B_{L-1}  →  …  →  B_1  →  Y
```

i.e. "X is a ref inside `B_{L-1}.L7`, which is a ref inside
`B_{L-2}.L7`, …, which is a ref inside `Y.L7`".

### 0.2 Terminology table

| Term | Meaning |
|------|---------|
| **BWS** | Batch Window Size = **128**. Canonical value from `HISTORY_PROOF_WINDOW_SIZE` in the AN node's `history-proof` library. |
| **Batch M** (thread 0) | The contiguous range of thread-0 blocks at heights `[M·BWS, (M+1)·BWS − 1]`. |
| **`#L<N>(M)`** | Layer-N batch root for batch M of thread 0. Layer-1 is built over the blocks of batch M; layer-(N+1) is built over `BWS` consecutive layer-N roots. |
| **X** | The **event block** — the AN block, in some thread t (t may be 0 or ≠ 0), that emitted the `WithdrawalInitiated` event. Its `block_id` is a public input (§6.3); no anonymity is claimed. X is the walk's leaf-side endpoint (older). |
| **Y** | The **anchor block** — a block in **thread 0**, *newer* than X, whose transitive `refs` closure reaches X within ≤ L cross-thread hops. Y's `block_leaf` folds into a `finalRoot ∈ layerWindows[]` on Ethereum, so Y is the on-chain trust root the walk begins at. When t = 0, Y = X and L = 0. |
| **X-side / Y-side** | The portions of the proof concerned with X (event binding, in thread t) and Y (thread-0 anchor). When t ≠ 0 they are separate; when t = 0 they collapse onto the same block. |
| **`event_hash`** | 32-byte SHA-256 root hash of the `WithdrawalInitiated` ext-out message wrapper cell (`repr_hash(C0)` of the 4-cell BOC — wrapper, body, recipient, sender). |
| **Block leaf** (thread 0, layer-1) | `block_leaf = Poseidon96(block_id ‖ envelope_hash ‖ tracked_ext_out_messages_root)`. Feeds the per-batch layer-1 Poseidon dense-Merkle tree in thread 0. |
| **`layerWindows[N]`** | On-chain (Ethereum) rolling window of thread-0 layer-N batch roots maintained by `AckiNackiBridge.sol`. Mirrors the node-side `GlobalHistoricalData[thread 0][N]`. |
| **`finalRoot`** | The layer-N batch root the prover anchors against. Exposed as `PUB_FINAL_ROOT`. Must lie inside `layerWindows[anchorLayer]` (`_isKnownLayerAnchor` in `AckiNackiBridge.sol`). |
| **`anchorLayer`** | 1-indexed layer number the prover claims for `finalRoot`. Range `1..=MAX_ANCHOR_LAYER = 10` (must equal Solidity `MAX_LAYER_HASHES`). |
| **L** | True chain length in hops between X and Y (`X → … → Y`, oldest → newest, leaf → root). `L = 0 ⇔ t = 0`. |
| **`L_MAX`** | Circuit-side upper bound on L — a **worst-case ceiling**, not a target. The node team has announced **300** as the hard worst-case for cross-thread walks under the current threading design; with slot-0 (same-thread parent) edges a walk can also run up to **~128** same-thread hops. Both numbers are already punishing for the prover (see §4.3 cell costs). The **desired average** we design for is **10–20** cross-thread hops, ideally **1–5**; longer walks are tolerated but should be the exception, not the plan. Prototyping cap = **20**. |
| **`H`** | Hops packed per `BridgeMultiHopProof` snark. Set to **1** for the bridge (see §5.H for the sizing derivation). |
| **`N_BUNDLE_MAX`** | Upper bound on the number of `BridgeMultiHopProof` snarks per claim. `N_BUNDLE_MAX = ⌈L_MAX / H⌉`. Dynamic per claim (§6.5); prototype cap = 20, worst-case production cap = 300. Typical claims land in the 1–20 range. |
| **Bundle** | One `BridgeEventFinalProof` + `n ∈ [0, N_BUNDLE_MAX]` `BridgeMultiHopProof` snarks. `n = 0` when `t = 0`. |
| **Circuit 4** | Current on-chain name for the bridge event-prove circuit registered in `AckiNackiBridge.sol`. This spec keeps the name and extends the public-input surface. |

**Poseidon input convention.** Poseidon here operates on BN254 Fr (`|p| ≈ 254 bits`). A byte stream is packed into Fr in **31-byte chunks** (little-endian, high byte implicitly zero), so every chunk is unambiguously `< p` and no modular reduction is needed. An N-byte input consumes `⌈N/31⌉` Fr elements. This applies uniformly to every `Poseidon(...)` in this spec — the tagged L7 leaves, the Poseidon96 block-leaf, the ext-out-messages leaves, dense-Merkle combines, and the layer-N batch combines.

---

## 1. Anchor model

### 1.1 Where events happen vs. where they anchor

The `WithdrawalInitiated` event — declared in the AN bridge exchange contract, ABI event id `0x3c838959` — may be emitted from **any** thread of Acki Nacki. The event's block X is therefore of arbitrary thread `t`.

The **anchor**, in contrast, must land in **thread 0**. Under the current threading protocol only thread 0 produces layer trees: `history_proofs` are populated exclusively on thread-0 key blocks, and only thread 0's per-thread window is mirrored to the Ethereum side. No other thread has a layer-N batch tree the Ethereum verifier can query. It follows that the anchor block Y must itself be a thread-0 block.

When `t = 0`, event and anchor coincide (`X = Y`) and no cross-thread bridging is needed. This is the "single-thread" case.

When `t ≠ 0`, the proof chains X (thread t, event) to Y (thread 0, anchored on Ethereum) via cross-thread L7 reference edges (§4). The walk is drawn `X → … → Y` (leaf → root): each step opens the newer block's L7 and exposes the older block as one of its refs. Y is the trust root — its `block_leaf` folds into a `finalRoot ∈ layerWindows[]` on Ethereum — and X inherits that trust transitively through the chain of L7 openings.

### 1.2 The contract-side check — Ethereum

`AckiNackiBridge.sol`'s withdrawal entrypoint consumes the bundle together with `(finalRoot, anchorLayer)` — the anchor the prover claims to hit — and asks whether that anchor exists in the on-chain window:

```solidity
require(
    _isKnownLayerAnchor(finalRoot, anchorLayer),
    ERR_UNKNOWN_ANCHOR
);
```

Two facts pin the anchor down:

1. **It's a public input of the proof.** `finalRoot` and `anchorLayer` are exposed as instances of `BridgeEventFinalProof` (see §6.3 for the 13-slot layout), so the circuit binds every private Y-side witness to *this specific* root at *this specific* layer.
2. **It must live in thread 0's window.** `_isKnownLayerAnchor` reads `layerWindows[anchorLayer]`, populated only from thread-0 layer-N batch roots ([`GLOBAL_HISTORY_DATA_SPEC.md`](../../docs/GLOBAL_HISTORY_DATA_SPEC.md)). The check therefore succeeds only when `finalRoot` is a genuine thread-0 layer-N root.

The rolling window has finite depth; anchors that age out of it become unusable and the prover must select a higher-layer anchor (§1.3).

### 1.3 Anchor strategy

The circuit does not anchor against a single fixed layer. It anchors against `#L<N>(M)` for some `(N, M)` chosen by the prover, subject only to the on-chain window still retaining that root at layer N. The daemon prefers the smallest N (cheapest — the dense chain is shorter, fewer layer combines to verify) and falls back to higher N if the layer-1 root containing the event has aged out of `layerWindows[1]`.

The bridge has no anonymity requirement (§1.4, §9), so anchor-layer choice is a pure cost decision — a higher N never buys privacy here.

### 1.4 No uniformity requirement

The bridge withdrawal is fully public: recipient address, token id, amount, source dApp / account, and every settled withdrawal field are on-chain. An observer already knows *what* was withdrawn and *to whom*. Consequently:

- **Bundle size is dynamic per claim.** Exactly `n = ⌈L / H⌉` hop snarks are submitted; `t = 0` sends **only** the FinalProof — zero hop snarks at all. Uniformity padding (padding every claim up to `N_BUNDLE_MAX` snarks so that an observer cannot distinguish short walks from long walks) is not enforced.
- **Endpoints are clear.** Each snark exposes the clear (unsalted) 32-byte block-ids at its head and tail. Cross-snark continuity is a plain field equality on Fr-encoded block-ids.
- **No per-snark position tag, no `bundle_index`, no `salt_commitment`, no salt witness.**

Every technique in this document that could have been used to hide `X.block_id`, the chain length L, or membership in a short-walk-vs-long-walk anonymity set has been deliberately omitted. §9 lists exactly what leaks and confirms that each leak is compatible with the bridge threat model.

---

## 2. `block_id` construction — depth-4, 16-leaf SHA-256 tree

Every AN block has `block_id = root of a 16-leaf SHA-256 Merkle tree of depth 4`. The 16 leaves are:

| Leaf | Definition | Hash family |
|------|------------|-------------|
| L0 | `Poseidon(layer_count ‖ (layer_id ‖ layer_root)×MAX_HISTORY_PROOF_LAYERS)` over the block's `history_proofs` map (populated only on thread-0 key blocks; zero elsewhere) | Poseidon |
| L1 | `SHA-256(bincode(CommonSection))` | SHA-256 |
| L2 | `Poseidon` dense-Merkle commitment to `old_bk_set` — zero if no BK change | Poseidon |
| L3 | `Poseidon` dense-Merkle commitment to `new_bk_set` — zero if no BK change | Poseidon |
| L4 | Poseidon dense-Merkle root over per-DApp TVM sub-block hashes (or tagged empty-block sentinel when the block carries no TVM transactions) | Poseidon |
| L5 | `SHA-256(bincode(DurableThreadAccountsStateDiff))` | SHA-256 |
| L6 | `SHA-256(tx_cnt.to_be_bytes())` (8-byte big-endian `u64`) | SHA-256 |
| L7 | Poseidon dense-Merkle root of `[parent_block_id, refs...]` (see §2.3) | Poseidon |
| **L8** | **`tracked_ext_out_messages_root`** — Poseidon dense-Merkle root of this block's tracked ext-out messages (see §2.4) | Poseidon |
| L9..L15 | `[0u8; 32]` (fixed zero padding) | — |

**Source of truth (chain side):** `node/src/types/ackinacki_block/mod.rs:499–567` (`block_merkle_leaves()`, `BLOCK_MERKLE_LEAF_COUNT = 16`); combine rule at `node/src/types/ackinacki_block/merkle.rs:11–37`. Landed 2026-07-08 in acki-nacki commit `4cd969bf9` ("Expanded Acki Nacki block Merkle leaves from 8 to 16"). The canonical bridge-side pin is [`crates/bridge-circuits/docs/BLOCK_ID_ALG_NEW.md`](../../docs/BLOCK_ID_ALG_NEW.md); it re-derives every constant from the chain source.

Combine rule at every level: `SHA-256(left_32B ‖ right_32B)`. Fifteen SHA-256 invocations total to fold 16 leaves into `block_id`.

```
                                     block_id  (SHA-256, depth 4)
                                    /                          \
                              h0..7                              h8..15
                             /      \                          /        \
                          h0..3     h4..7                   h8..11    h12..15
                          /  \      /  \                    /   \      /   \
                        h01 h23   h45 h67                 h89 h10-11 h12-13 h14-15
                        / \ / \   / \ / \                / \  / \    / \    / \
                       L0 L1L2 L3 L4 L5L6 L7            L8 L9 L10 L11 L12 L13 L14 L15
```

### 2.1 Circuit-side implications of the L8..L15 layout

- **L8 is the bridge circuit's only opening target on the X-side.** The circuit opens leaf L8 of X's block-id tree, exposing `X.tracked_ext_out_messages_root` as a 32-byte value.
- **The three siblings on the right-hand subtree are protocol-fixed constants.** L9..L15 are all `[0u8; 32]`, hence:
  - `h10-11 = SHA-256(0×32 ‖ 0×32)` — constant
  - `h12-13 = SHA-256(0×32 ‖ 0×32) = h10-11` — constant
  - `h14-15 = SHA-256(0×32 ‖ 0×32) = h10-11` — constant
  - `h12..15 = SHA-256(h12-13 ‖ h14-15) = SHA-256(h10-11 ‖ h10-11)` — constant
  - `L9 = 0×32` — constant
  These four constants are hard-coded into the circuit as fixed cells; the prover does not witness them. Only the L8 leaf itself and its left-half cousin `h0..7` are live witnesses when opening L8.
- **Opening any leaf costs 4 SHA-256 *calls* = 8 SHA-256 compressions** — one call per tree level. Each call hashes a 64-byte `(child ‖ sibling)` input; SHA-256 padding pushes any input ≥ 56 B into a **second compression block**, so one 64-byte call = 2 compressions, and a depth-4 path takes 4 calls × 2 = **8 compressions**. Opening L8 walks `L8 → h89 → h8..11 → h8..15 → block_id`; three of the four siblings (`L9`, `h10-11`, `h12..15`) are the constants above, one (`h0..7`) is a witness. Opening L7 for a hop (§4) walks `L7 → h67 → h4..7 → h0..7 → block_id`; all four siblings (`L6`, `h45`, `h0..3`, `h8..15`) are live witnesses, since a hop does not bind L8 and therefore leaves `h8..15` opaque.

### 2.2 CommonSection — fields feeding L0, L1, L7, L8

`node/src/types/ackinacki_block/common_section.rs`, declaration order:

```
parent_block_id                : BlockIdentifier              // 32 bytes — fed to L7 (slot 0)
block_height                   : BlockHeight
directives                     : Directives
block_attestations             : Vec<Envelope<AttestationData>>
round, producer_id, thread_id, threads_table
refs                           : Vec<BlockIdentifier>          // fed to L7 (slots 1..n)
block_keeper_set_changes       : Vec<...>
verify_complexity, acks, nacks, producer_selector
history_proofs                 : BTreeMap<LayerNumber, ProofLayerRootHash>   // fed to L0; populated only for thread-0 key blocks
tracked_ext_out_messages_root  : [u8; 32]                     // = L8 (also feeds L1 via bincode)
tracked_ext_out_messages       : BTreeMap<...>                 // preimage of the tree rooted at L8
block_keeper_set_change_proof_data : Option<...>
```

### 2.3 L7 — cross-thread reference tree (variable depth on the chain side)

L7 is the Poseidon dense-Merkle root of `[parent_block_id, refs[0..n]]`, where each leaf is a tagged Poseidon hash:

```
REFERENCED_PARENT_BLOCK_TAG = b"acki-nacki:referenced-block:parent:v1"   (37 bytes)
REFERENCED_REF_BLOCK_TAG    = b"acki-nacki:referenced-block:ref:v1"     (34 bytes)

leaf[i] = Poseidon( tag_i ‖ proof_block_refs[i] )    (32 bytes out)
```

`proof_block_refs[0] = parent_block_id` (same thread by producer construction — see below); `proof_block_refs[1..1+n] = refs` (cross-thread). The tree width is `leaves.len().next_power_of_two()` — **not** a fixed 256-leaf shape — and is padded with literal `[0u8; 32]` leaves up to that width before folding with `Poseidon(left_32B ‖ right_32B)`. Source: `node/libs/history-proof/src/lib.rs:195–215` (`compute_referenced_block_leaf_hash`, `compute_referenced_blocks_root`) and `dense_merkle_root` in the same file; empty ref-list yields `[0u8; 32]`.

The chain imposes **no hard ref-count ceiling** — `refs` is a plain `Vec<BlockIdentifier>`, so real blocks have variable-depth L7 (observed depths 0..8). The bridge circuit therefore witnesses the actual per-hop tree depth (`refs_tree_depth`) and walks a gated 8-step fold sized to `MAX_PROOF_BLOCK_REFS = 256` (depth 8); see §4.2 for the depth-witness handling.

**Chain-side: slot 0 vs slots ≥ 1.** The producer for thread `t` selects its parent via `select_thread_last_finalized_block(&thread_id)` and sets the child's block height as `parent_height.next(&thread_id)` (`node/src/block/producer/producer_service/block_producer.rs:534,576,992`), so under normal operation `parent_block_id` is same-thread and `refs[0..n]` are cross-thread. The *spawn edge* is the one exception — the first block of a newly-spawned thread T′ has as its parent the split block on the parent thread; a producer that wants to anchor across such an edge can also add it to `refs`. **The circuit treats both leaf kinds uniformly** (see §4 and the slot-agnostic hop gadget below): both leaves live inside the same L7 Poseidon tree, the leaf tag is picked by `is_zero(ref_index)` between `REFERENCED_PARENT_BLOCK_TAG` and `REFERENCED_REF_BLOCK_TAG` (`multi_hop_witness.rs` constants `REFERENCED_PARENT_BLOCK_TAG`/`REFERENCED_REF_BLOCK_TAG`), and `ref_index` is range-checked to `[0, 2^refs_tree_depth)`. Opening a slot-0 edge amounts to a same-thread parent walk-back, which the shortest-path witness builder uses freely (see [`MULTITHREAD_PRIVATE_WITNESS.md`](./MULTITHREAD_PRIVATE_WITNESS.md) §1).

L7 is populated for **every** block and provides the outgoing edges the L7 walk (§4) follows.

### 2.4 L8 — tracked ext-out messages Merkle tree (the bridge-visible slot)

`L8 = tracked_ext_out_messages_root` is the **Poseidon** dense-Merkle root of the block's tracked outgoing external messages. The `WithdrawalInitiated` event is emitted as one such message, and its Poseidon-tagged leaf is included in this tree.

**Shape:** identical to L7 — chain-side variable depth via `leaves.len().next_power_of_two()`, `[0u8; 32]` padding, no chain-enforced ceiling. Same `dense_merkle_root` routine as L7 and the layer-N batch trees (source: `node/libs/history-proof/src/lib.rs:162–193` — `compute_ext_out_messages_root`, `compute_ext_message_leaf_hash`). Empty case yields `[0u8; 32]`.

**Leaf format:** `Poseidon(account_dapp_id ‖ account_id ‖ ext_message_hash)` — a **96-byte** preimage, no tag prefix. This is the exact shape the current single-thread `BridgeEventProveCircuit` already computes as `ext_msg_leaf`.

**Circuit handling:** the bridge circuit uses a variable-depth gated fold: pass real siblings to `preprocess_dense_proof_padded`, walk a fixed `MAX_EVENTS_TREE_DEPTH = 8` levels in-circuit (`bridge_event_prove_circuit.rs:124`), and gate each level with a range-checked `num_events_levels` witness. Direction bits are bound to the bit decomposition of the event-tree position via the shared `walk_dense_merkle_bind_pos` helper (§6.2). The 8-level cap bounds L8 to 256 messages per block on the prover side; the chain itself does not enforce this.

### 2.5 L0..L6 and L9..L15 in this design

- **L0..L6** — each defined by the chain per the table above. On the X-side, all seven collectively appear only as the aggregate `h0..7` witness (the sole live sibling required to open L8). The circuit does **not** parse any of L0..L6 individually.
- **L9..L15** — zero-initialised in the chain's `block_merkle_leaves()` and never overwritten (`node/src/types/ackinacki_block/mod.rs:556`). Their contribution to the block-id tree collapses to the two SHA-256 constants of §2.1 baked into the circuit.

---

## 3. Per-thread layer-N batch tree (thread 0 only)

The per-layer batch tree is a **Poseidon dense-Merkle** of width `BWS = 128`. It is built independently per batch per layer for thread 0 only; thread 0's layer-N tree roots are the values mirrored into the Ethereum-side `layerWindows[N]`. **No other thread produces layer trees under this protocol.** See [`GLOBAL_HISTORY_DATA_SPEC.md`](../../docs/GLOBAL_HISTORY_DATA_SPEC.md) for both the on-chain window semantics and the production anchoring cadence — a layer-N root is appended on each thread-0 key block of order N, i.e. at heights where `height % W^N == 0`, so layer 2 anchors roughly every `W² = 128² = 16384` source blocks and higher layers correspondingly less often. [`BRIDGE_PROVER_THINNING_SPEC.md`](../../docs/BRIDGE_PROVER_THINNING_SPEC.md) documents an earlier fixed-stride (`W·P`) thinning model that is still partially reflected in `crates/bridge-relayer-daemon`; it is retained for background on the relayer code paths and should not be read as the current production cadence.

### 3.1 Layer-1 leaf: `block_leaf`

For every block B produced in thread 0, the producer of B's next key block emits a leaf:

```
block_leaf(B) = Poseidon( B.block_id ‖ B.envelope_hash ‖ B.tracked_ext_out_messages_root )
```

Total input: 96 bytes (three 32-byte fields). Single Poseidon invocation.

### 3.2 Layer-1 tree structure

Source: `HistoryBlockData::calculate_root_hash` at `node/src/types/history_proof.rs`.

For batch M of thread 0, the layer-1 tree has exactly `BWS + 2 = 130` real leaves, in this order:

```
leaves[0]         = last #L<2>(...) seen by thread 0 (zero if absent)   // higher-layer back-link
leaves[1]         = #L1(M − 1)                          (zero if M == 0)   // same-layer back-link
leaves[2 .. 130]  = block_leaf(B_{M·BWS + 0 .. M·BWS + BWS − 1})           // 128 block leaves
```

Padded to 256 with `[0u8; 32]`, folded as a Poseidon dense-Merkle of depth 8 with combine rule `Poseidon(left_32B ‖ right_32B)`. Root: `#L1(M)`.

The two prepended back-links create the "chain" property: `#L1(M)` cryptographically commits to `#L1(M−1)` and the most recent higher-layer root, allowing layered fallback without separate chain witnesses.

### 3.3 Layer-N recursion for N ≥ 2

Same shape as §3.2, over `BWS` consecutive layer-(N−1) roots of thread 0, with the same two back-link leaves. Root: `#L<N>(M)`.

### 3.4 The Y-side anchor path — what the bridge circuit opens

Given a thread-0 anchor block Y, the Y-side reconstructs the chain

```
block_leaf(Y) → #L1(M_Y) → #L2(...) → ... → #L<N>(...) = finalRoot
```

in four concrete steps (this is already implemented in the single-thread `BridgeEventProveCircuit` at `bridge_event_prove_circuit.rs:848-876` via `preprocess_dense_proof` → `dense_merkle_root_circuit` → `verify_chain_of_dense_proofs`, and is inherited **verbatim**):

1. **Y-leaf** — `block_leaf(Y) = Poseidon96(Y.block_id ‖ Y.envelope_hash ‖ Y.tracked_ext_out_messages_root)`.
2. **Batch-tree opening (8 levels)** — one Poseidon dense-Merkle path lifts `block_leaf(Y)` to the layer-1 batch root `#L1(M_Y)`. The batch tree is `BWS = 128` block leaves wide (§3.1) plus 2 back-link leaves, padded to 256 → **8-level** path.
3. **Layer ladder (≤ 11 links)** — a chain of dense-Merkle steps stacks `#L<k>` into `#L<k+1>` for `k = 1..N−1`, one step per layer (§3.3 recursion). Circuit-side length is fixed at `MAX_CHAIN_LEN = 11`; the prover activates only the first `anchorLayer − 1` links.
4. **Public output** — `anchorLayer = target_layer + 1` where `target_layer` is the last active link's index. The terminal root is `finalRoot`, exposed as `PUB_FINAL_ROOT` on `BridgeEventFinalProof`.

Y's `envelope_hash` and `tracked_ext_out_messages_root` are **unconstrained witnesses** — Y only supplies a thread-0 anchor; the event content is bound on the X-side (§4 hop walk and §6.7 event binding).

---

## 4. Cross-thread inclusion proof — the L7 walk

### 4.1 The hop primitive

A **hop** is the atomic cross-thread step. One hop proves:

> *Block A's block_id appears in block B's L7 as one of `proof_block_refs[k]` (slot `k ∈ [0, refs.len())`).*

Per the §0.1 arrow convention this is written **`A → B`** (A on the left, older, is a leaf in B.L7; B on the right, newer, opens its L7 to expose A). The hop's `hop_start` is B (the block whose L7 we open — root side); `hop_end` is A (the ref exposed inside B's L7 — leaf side). The arrow is drawn leaf → root (oldest → newest), consistent with every other Merkle fold in this doc.

At the walk-chain level, the walk starts at the event block **X** (thread t, oldest, leaf-most) and terminates at **Y** (thread 0, newest, anchored): `X → … → Y`, oldest → newest, left to right.

**Both slot kinds are valid hop edges** (see [`multithreading/README.md`](../../../../multithreading/README.md) §2.0): slot 0 (`parent_block_id`, same-thread parent chain) and slots ≥ 1 (cross-thread refs) live inside the same L7 Poseidon tree and open identically. The hop gadget is slot-agnostic.

### 4.2 What one hop constrains

The hop is a **building block, not a standalone snark** — the atomic hop has no public inputs of its own. Its endpoints (`current_block_id`, `next_block_id`) surface at the enclosing `BridgeMultiHopProof` boundary as clear Fr-encoded 32-byte block-ids (§6.3); at `H = 1` those endpoints *are* the snark's two publics.

**Shape-witnessing preamble.** The L7 tree on the chain side is variable-depth (§2.3). Since Halo2 constraints are a fixed circuit, we size the inner path array to the worst case (`MAX_PROOF_BLOCK_REFS_DEPTH = 8`) and carry a per-hop witness `refs_tree_depth ∈ [0, 8]` that tells the circuit how many combine steps of the pre-allocated 8-step fold are *live* for this hop. The remaining steps are gated off. This mirrors the pattern the current single-thread `BridgeEventProveCircuit` uses for the variable-depth ext-out-messages tree opening (`num_events_levels`).

Inputs for a hop `A → B` (A leaf/older, B root/newer — all private witnesses at the hop level):

```
current_block_id   = B.block_id                          (32 bytes)
next_block_id      = A.block_id                          (32 bytes)
B.L7_root                                                (32 bytes; the L7 root being opened against B.block_id)
outer_siblings     = [L6, h45, h0..3, h8..15]            (4 × 32 bytes; depth-4 SHA path from L7 up to block_id — all opaque, incl. h8..15)
refs_tree_depth    (u8; range-checked to [0, MAX_PROOF_BLOCK_REFS_DEPTH = 8])
ref_index          (u32; range-checked to [0, 2^refs_tree_depth))
L7_inner_path      ([[u8; 32]; 8]; fixed-length array — entries beyond refs_tree_depth are padding, ignored)
```

An `is_active` flag also lives at the hop level, but only makes sense inside `BridgeMultiHopProof` where a fixed-width array of `H` hops must be padded with no-op hops when the true walk length is shorter than `H` (only relevant when `H > 1` or on the last snark of a bundle when `L mod H ≠ 0`; see §6.6).

Constraints (single hop; the enclosing `BridgeMultiHopProof` gates them by its own `is_active[h]`):

1. **SHA-256 depth-4 Merkle path.** Open `B.L7_root` against `B.block_id` via the 4-step path `L7 → h67 → h4..7 → h0..7 → block_id` using witness siblings `[L6, h45, h0..3, h8..15]`. **4 SHA-256 calls** = **8 compressions** (each 64-byte call pads into a second block). `h8..15` is an opaque witness — a hop does not bind L8, so no derivation from L8 or from the L9..L15 zero-constants is needed here (that only happens in `BridgeEventFinalProof`, §6.7).
2. **Tagged leaf hash for A.** All L7 leaves share the same tag:
   ```
   tag_bytes = REFERENCED_REF_BLOCK_TAG
   tag_hash  = Poseidon(tag_bytes ‖ A.block_id)
   ```
3. **Depth-witness sanity.**
   - Range-check `refs_tree_depth ∈ [0, 8]` (a lookup or 4-bit decomposition).
   - Range-check `ref_index ∈ [0, 2^refs_tree_depth)` — i.e. the high `(8 - refs_tree_depth)` bits of `ref_index` are zero. This prevents the prover from opening a padding slot at any depth. Realisation: decompose `ref_index` into 8 bits `b0..b7`; unary-decompose `refs_tree_depth` into `d0..d7` where `dk = 1{k < refs_tree_depth}` (monotone-decreasing); assert `bk · (1 − dk) == 0` for `k = 0..7`.
4. **Variable-depth Poseidon dense-Merkle opening.** Verify `B.L7_root == open(tag_hash, ref_index, L7_inner_path, refs_tree_depth)`. Implemented as an unconditional 8-step fold with per-step live-flag:
   ```
   acc_0     = tag_hash
   for k = 0..8:
       live_k   = 1{k < refs_tree_depth}                        // = d_k above
       bit_k    = k-th LE bit of ref_index                       // sibling-order selector
       combined = bit_k ? Poseidon(L7_inner_path[k] ‖ acc_k)
                        : Poseidon(acc_k ‖ L7_inner_path[k])
       acc_{k+1} = live_k ? combined : acc_k                     // pad steps pass through
   assert acc_8 == B.L7_root
   ```
   Direction bits inside the walker are bound to the bit-decomposition of `ref_index` via `dense_merkle_root_padded_bound` (§6.2). This matches the canonical `dense_merkle_verify` algorithm of `node/libs/history-proof/src/lib.rs` when `refs_tree_depth == ceil(log2(leaves.len()))` where `leaves.len() = 1 + refs.len()`.

Inactive-hop behaviour (when a trailing snark carries `is_active[h] == 0`): the four constraints above are disabled by selector multiplication and byte-equality `next_block_id == current_block_id` is enforced instead. The chain propagates through the padded hop as identity.

**Reversibility.** If acki-nacki ever adopts a fixed-shape L7 (unlikely per current signals), the variable-depth circuit continues to accept it: a chain that always emits 256 leaves simply pins `refs_tree_depth = 8` for every hop. No protocol re-negotiation required to tighten later.

### 4.3 What one hop costs

- **4 SHA-256 compressions per hop** — one per level of the depth-4 outer path from L7 up to `block_id`. Each SHA call runs on 64 bytes and pads into a second compression block, so per-call = 2 compressions; a depth-4 path takes 4 calls = **8 compressions per hop** in the actual gosh-sha256-chip layout. See [`SHA256_INVOCATIONS.md`](../../docs/SHA256_INVOCATIONS.md) §4 for the per-call SHA cell accounting.
- **8 Poseidon combines** for the L7 inner fold (unconditional under the variable-depth scheme; pad steps are gated but still assigned). At ~2 K cells per Poseidon: ~16 K cells.
- 1 Poseidon for tagged-leaf construction → negligible.
- Depth-witness gadget (unary decomposition of `refs_tree_depth`, 8-bit decomposition of `ref_index`, per-step live-flag mux over `[u8; 32]` cells): a few thousand cells.
- Range checks + `is_active` selectors → ≈ 100 K cells.

At `gosh-sha256-chip`'s measured ≈ 354 K advice cells per SHA compression: **≈ 2.83 M advice cells per hop** dominated by the 8 SHA compressions. At `H = 1` the snark's SHA workload equals one hop.

### 4.4 The full L7 walk

The walk starts at **X** (the event block, thread t, older) and
terminates at **Y** (the anchor block, thread 0, newer, on-chain-anchored
via `layerWindows[anchorLayer]`). A chain of L hops
`[hop_0, hop_1, …, hop_{L-1}]` collectively proves:

```
X  =  B_L  →  B_{L-1}  →  ...  →  B_1  →  B_0  =  Y   (oldest → newest, leaf → root)
       ^                                              ^
       thread t (event block)                         thread 0 (anchor, on-chain-known)
```

with the per-hop endpoint convention `hop_i.hop_start = B_i` (the newer
block whose L7 is opened — the **root** side of the arrow) and
`hop_i.hop_end = B_{i+1}` (the older ref extracted from
`B_i.proof_block_refs` — the **leaf** side) and the gluing constraint
`hop_i.hop_end == hop_{i+1}.hop_start` for all i.

Reading the diagram: `hop_0.hop_start = B_0 = Y`, `hop_{L-1}.hop_end =
B_L = X`, so at the bundle level
`hopProofs[0].publicInputs[PUB_HOP_START] = y_block_id_fr` and
`hopProofs[last].publicInputs[PUB_HOP_END] = x_block_id_fr` (§6.4).
Bundle ordering is thus **right-to-left** in the diagram: `snarks[0]`
handles the rightmost arrow `B_1 → Y`, `snarks[L-1]` handles the
leftmost arrow `X → B_{L-1}`.

At `H = 1`, each hop lives in its own snark; the gluing constraint becomes cross-snark and is enforced by the Solidity orchestrator on the clear block-ids exposed at the snarks' publics (§6.4).

**Worst-case ceiling: `L_MAX = 300`** — the hard upper bound the node team announced for cross-thread walks under the current threading design, not a design target. Slot-0 (same-thread parent) edges add a separate ceiling of ~128 same-thread hops. Both are expensive: at ≈ 2.83 M advice cells per hop (§4.3), 300 hops ≈ 850 M cells of SHA work alone. The **desired average** is **10–20** hops, ideally **1–5**; the circuit tolerates longer walks but each extra hop costs real prover wall time. Prototyping cap `L_MAX = 20`. `N_BUNDLE_MAX = ⌈L_MAX / H⌉` scales linearly — at `H = 1` this yields 20 hop snarks (prototype) or 300 hop snarks (worst-case production). Per-snark K is unchanged by `L_MAX`.

Reference off-chain implementation of the equivalent chain walk: acki-nacki's `helpers/proof_helper/src/gql_proof.rs`. The circuit-side hop logic mirrors `verify_proof_block_ref_proof` (Poseidon inner) + `verify_block_merkle_leaf_proof` (SHA outer, at depth 4).

---

## 5. Full protocol binding scheme

**Diagram convention below.** The vertical `↓` arrows in the flow diagram
that follows denote **cryptographic reduction** (each step is a hash
opening: `child_bytes → hash(child_bytes) = parent_root`), read top-to-bottom
as "event data reduces to `finalRoot`". This is orthogonal to the
walk-chain arrow convention of §0.1 / §4.4 — the L7-walk row inside the
diagram compresses L hops in the oldest → newest / leaf → root order
(`X → … → Y`) into a single "walk" step for readability. Because the
surrounding diagram flows top-to-bottom from `X.block_id` down to
`Y.block_id`, the embedded walk row is drawn pointing **downward** (X at
the top, Y at the bottom); each `↓` is one hop's L7 opening (`refs[k]`
leaf → opening block's `block_id` root), and the composition of L such
openings reduces X to Y.

Reading the chain from the withdrawal event down to the on-chain-known anchor root:

```
WithdrawalInitiated(X)  →  event body BOC (4 cells: wrapper, body, recipient, sender)
   ↓ (SHA-256: wrapper, body, recipient, sender + 3 child-hash equality links)
event_hash = SHA-256(wrapper cell repr data) = repr_hash(C0)
   ↓ (Poseidon96)
ext_msg_leaf  =  Poseidon96( account_dapp_id ‖ account_id ‖ event_hash )
   ↓ (Poseidon dense-Merkle path, depth ≤ 8, events_pos position-bound)
X.tracked_ext_out_messages_root                        (= X.L8)
   ↓ (block-id tree, depth 4, opens L8; 3 constant siblings + h0..7 witness)
X.block_id                                             (PUBLIC on FinalProof)
   ↓ (L7 walk: L hops opened in diagram order X → B_{L-1} → … → B_1 → Y, i.e. oldest → newest, leaf → root;
       each hop's L7 opening commits its `hop_end.block_id` (leaf, older) to its `hop_start.block_id` (root, newer);
       bundle order is right-to-left in the §4.4 diagram, so snarks[0].hop_start = Y and snarks[L-1].hop_end = X)
Y.block_id                                             (Y in thread 0; when t=0, Y = X and L = 0)
                                                       (PUBLIC on FinalProof)
   ↓ (Poseidon96)
block_leaf(Y)  =  Poseidon96( Y.block_id ‖ Y.envelope_hash ‖ Y.tracked_ext_out_messages_root )
   ↓ (Poseidon dense-Merkle, depth 8, thread-0 layer-1 batch tree)
#L1(M_Y)
   ↓ (dense chain, ≤ MAX_CHAIN_LEN = 11)
#L<N>(...)  =  finalRoot                               (PUBLIC on FinalProof)
```

Key properties:

- **Event binding on the X-side is fully algebraic and self-contained.** The prover supplies (a) the raw 4-cell BOC preimages, (b) the SHA child-hash chain that links `wrapper ↔ body ↔ recipient / sender` (each parent cell embeds each child's 32-byte `repr_hash` at a known offset), (c) the ext-out-messages Merkle path from `ext_msg_leaf` to X's L8, and (d) the depth-4 L8 opening (three constant siblings + `h0..7` witness).
- **The L7 walk is a pure L7 traversal.** Each hop opens the outer depth-4 SHA-256 tree of some block B, extracts B.L7, and opens one slot of L7's inner Poseidon dense-Merkle to reveal an edge to some older block A. `h8..15` on hops is an opaque witness because hops don't bind L8. Slot 0 (`parent_block_id`) is excluded.
- **Y is a thread-0 block.** Y need not be a key block — only that the batch tree containing `block_leaf(Y)` is currently anchored in the on-chain window. Y's `envelope_hash` and `tracked_ext_out_messages_root` are unconstrained witnesses; Y is the anchor, not the event source.
- **`t = 0` case.** When X is in thread 0, X = Y. The FinalProof publishes `x_block_id_fr == y_block_id_fr`. The bundle contains **zero** hop snarks; the on-chain orchestrator sees `hopProofs.length == 0`, checks `x_block_id_fr == y_block_id_fr`, and proceeds to per-snark verification.
- **Nullifier binds the event block.** `nullifier = Poseidon(x_block_id_fr, tokenId, amount, recipientHi, recipientLo, senderAccFr, eventsPos)` — the withdrawal is uniquely identified by the event's location in the chain (X), plus the settled withdrawal fields, plus the in-block position of the event (§9.3).

---

### 5.H `H_HOPS_PER_PROOF` choice — locked at 1

Per **active** hop, the depth-4 block-Merkle opening runs `BLOCK_MERKLE_DEPTH = 4` SHA-256 calls on 64-byte `(child ‖ sibling)` inputs. SHA-256 padding pushes any input ≥ 56 B into a second compression block, so each call = 2 compressions ⇒ **8 SHA-256 compressions per hop**. The gosh-sha256-chip costs ≈ 354 K advice cells per compression.

| H | SHA compressions | SHA cells | ~Total cells | K=17 columns | Outer SHPLONK Yul size |
|---|---|---|---|---|---|
| 1 | 8 | ~2.83 M | ~2.84 M | ~25 | ~21 KB at `k_outer=21` (fits EIP-170) |
| 2 | 16 | ~5.7 M | ~7 M | ~50 | 33 213 B at `k_outer=21` (FAIL, 135 % EIP-170) |
| 3 | 24 | ~8.5 M | ~10 M | ~75 | tight even at `k_outer=22` |
| 5 | 40 | ~14 M | ~16 M | ~200 | OOMs at outer keygen on 16 GB + swap |

The outer SHPLONK Yul bytecode scales at ~450–500 B per inner advice column (measured empirically across the four existing production verifiers — see [`CIRCUIT_COMPLEXITY_COMPARISON.md`](../../docs/CIRCUIT_COMPLEXITY_COMPARISON.md) §4). The bridge's Solidity SHPLONK aggregator is capped by EIP-170 at **24 576 B**, which forces a tight inner column budget. **`H = 1` is the only choice that keeps the on-chain aggregator inside the EIP-170 envelope.**

Tradeoff: `N_BUNDLE_MAX = ⌈L_MAX / H⌉ = L_MAX` multi-hop snarks per claim at `H = 1`.
- Prototype `L_MAX = 20` → up to 20 hop snarks.
- Worst-case production `L_MAX = 300` → up to 300 hop snarks. The desired operating range is 1–20 hops per claim; 300 is a hard ceiling, not an expected shape.

Verification cost per bundle scales linearly. The multi-hop chaining logic that a larger `H` would have kept inside the circuit is instead moved to the smart contract (`withdrawByProofBundle`, §6.4): bundle adjacency `hopEnd[i] == hopStart[i+1]` is enforced by Solidity equality checks on the clear block-ids.

---

## 6. Bridge circuit embodiment — multi-proof composition on Ethereum

### 6.1 Why not one big circuit

The full scheme of §5 cannot fit in a single Halo2 circuit at server-feasible K without blowing past the Ethereum verifier size budget. The dominant cost is L7-walk SHA-256: each hop = 8 SHA-256 compressions ≈ 2.83 M advice cells. A chain of 20 hops alone is ≈ 57 M cells; a chain of 300 hops is ≈ 850 M cells — orders of magnitude past any K compatible with the on-chain SHPLONK verifier budget. Two ways to split:

- **(A) In-circuit aggregation** (`AggregationCircuit` from snark-verifier-sdk): rejected — see §8.
- **(B) Multi-proof composition with on-chain orchestration** (this design): produce one `BridgeEventFinalProof` and up to `N_BUNDLE_MAX` `BridgeMultiHopProof` snarks. The Ethereum contract checks continuity on the clear block-ids exposed as public inputs, then verifies each snark.

The bridge prover runs on server hardware, not a phone. The proving-cost pressure that would drive a very small K in a mobile setting is therefore not the binding constraint here — but the **on-chain verifier size** is. Ethereum SHPLONK verifier bytecode is a monotone function of the circuit shape (see the SHPLONK aggregator memory in the repo's operator notes), and the EIP-170 24 576 B cap is what forces `H = 1` (see §5.H).

### 6.2 Circuit definitions

| Circuit | Role | K | Snarks per claim |
|---|---|---|---|
| `BridgeEventFinalProof` | Withdrawal event binding + X-side L8/block-id reconstruction + Y-side thread-0 anchor. Exposes clear X-block-id and Y-block-id in addition to the existing 11 event/nullifier/anchor publics. | **17** (with column bump) or **18** — decided via K sweep before landing | 1 |
| `BridgeMultiHopProof` | A single hop segment (`H = 1`) with `is_active`. Exposes clear start/end block-ids. Per-hop constraints per §4.2. | **17** | `n = ⌈L / H⌉`, 0 when `L = 0` |

Both circuits live in the same `bridge-event-prove-circuit` crate as sibling `Circuit` implementations, sharing the same `BaseCircuitParams` conventions and `Sha256Chip`/`gosh-dense-balanced-tree` toolchain the current code uses.

**Shared helper — `dense_merkle_bound.rs`.** A Merkle-path walker needs a left/right direction bit at each level. The upstream walker in `gosh-dense-balanced-tree` lets the prover pick those bits freely (only `assert_bit`); that is unsound the moment the leaf's *position* is also exposed or range-checked, because the prover can declare one position and walk to another. The bridge crate already vendors a one-line-changed copy of the upstream walker (`bridge_event_prove_circuit.rs:116`, `walk_dense_merkle_bind_pos`): the direction bit at level `j` **is** bit `j` of the caller-supplied position decomposition, so declared position and walked path are the same object by construction. The current single-thread circuit uses it for the L8 events-tree walk (position → `PUB_EVENTS_POS`); the new `BridgeMultiHopProof` reuses it for the L7 `ref_index` per hop; the new L8 → `block_id` opening on the X-side uses a plain walker since the opened leaf position is the protocol constant `8`.

### 6.3 Clear (unsalted) endpoints — on-chain continuity

Every snark exposes clear 32-byte block-ids as its glue instances. Since the bridge has no anonymity requirement (§1.4, §9), exposing `X.block_id` and `Y.block_id` in the clear costs nothing.

#### Per-`BridgeMultiHopProof` public inputs (2 Fr)

```
inst[0] = hop_start_block_id   =  bytes_to_fr( B_0.block_id )        // Fr-encoded LE — newer end of this snark's segment (= Y for snarks[0])
inst[1] = hop_end_block_id     =  bytes_to_fr( B_H.block_id )        // Fr-encoded LE — older end of this snark's segment (= X for snarks[last])
```

**No `salt_commitment`, no `bundle_index`, no position tag.**

Cross-snark continuity is a plain field equality:
`hopProofs[i].publicInputs[1] == hopProofs[i+1].publicInputs[0]`
(each snark's `hop_end` — its older endpoint — equals the next snark's
`hop_start` — that snark's newer endpoint, one hop deeper into the past).

#### Per-`BridgeEventFinalProof` public inputs (13 Fr)

Delta from the current 11-slot single-thread layout: **append two new instances at the end** (`PUB_X_BLOCK_ID`, `PUB_Y_BLOCK_ID`). The ordering of the existing 11 slots is preserved so existing on-chain code and fixture consumers keep their offsets.

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

- **`y_block_id_fr`** is the **head** of the walk (newest, thread 0, on-chain-anchored) — `hopProofs[0].hop_start_block_id` must equal it. It's the block whose `block_leaf` feeds the Y-side dense-chain walk that produces `finalRoot`, so this is where the on-chain trust root plugs into the bundle.
- **`x_block_id_fr`** is the **tail** of the walk (oldest, thread t, event block) — `hopProofs[last].hop_end_block_id` must equal it. It is also the value the on-circuit nullifier binds to, so exposing it lets a fraud-proof verifier or an off-chain auditor re-derive the nullifier from the observable event and instantly detect a mismatch.
- **`t = 0` case** — `x_block_id_fr == y_block_id_fr` and the bundle contains zero hop snarks; the on-chain check degenerates to `require(x_block_id_fr == y_block_id_fr)`, no continuity walk.

**Destination identity pin.** The bridge already exposes `dappFr` / `accFr` as the destination (the Acki-Nacki-side bridge contract's dApp id and account id). The on-chain verifier compares them against pre-committed constants (`EXPECTED_BRIDGE_DAPP_FR`, `EXPECTED_BRIDGE_ACC_FR`), which closes off any attempt to forge a proof from an event emitted by a different account. No additional contract-identity pins are added on top.

### 6.4 Ethereum orchestration

The `AckiNackiBridge.sol` `withdrawByProof` entrypoint currently accepts a single Circuit-4 SHPLONK-aggregated proof. Under this spec it accepts a **bundle** — one `BridgeEventFinalProof` + `n ∈ [0, N_BUNDLE_MAX]` `BridgeMultiHopProof` snarks — and enforces continuity across them before delegating to the per-snark SHPLONK verifiers.

Ordering rationale: cheap consistency checks over public inputs first (fail-fast on any structural break — replay, chain break, anchor mismatch, wrong destination), and only then the expensive Halo2 KZG verifications.

```solidity
function withdrawByProofBundle(
    FinalProofData calldata finalProof,   // Circuit-4 replacement
    MultiHopProofData[] calldata hopProofs,
    // ...existing calldata (recipient signature, etc.)...
) external {
    // === Phase 1: cheap public-input consistency ==============================

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

    // 1c. Chain continuity — CLEAR block-id equality.
    //     Walk is leaf → root (X → … → Y). Bundle is ordered root-side
    //     first, so hopProofs[0] handles the hop with root Y (hop_start
    //     = Y, the anchored end) and hopProofs[last] handles the hop
    //     with leaf X (hop_end = X, the event end).
    bytes32 xBlockId = finalProof.publicInputs[PUB_X_BLOCK_ID];
    bytes32 yBlockId = finalProof.publicInputs[PUB_Y_BLOCK_ID];
    if (hopProofs.length == 0) {
        require(xBlockId == yBlockId, ERR_MISSING_HOP_FOR_CROSS_THREAD);
    } else {
        require(
            hopProofs[0].publicInputs[PUB_HOP_START] == yBlockId,
            ERR_Y_HEAD_MISMATCH
        );
        for (uint i = 0; i + 1 < hopProofs.length; ++i) {
            require(
                hopProofs[i].publicInputs[PUB_HOP_END]
                    == hopProofs[i+1].publicInputs[PUB_HOP_START],
                ERR_CHAIN_BREAK
            );
        }
        require(
            hopProofs[hopProofs.length - 1].publicInputs[PUB_HOP_END] == xBlockId,
            ERR_X_TAIL_MISMATCH
        );
    }

    // 1d. Destination identity: pinned dApp / account
    require(
        finalProof.publicInputs[PUB_DAPP_FR] == EXPECTED_BRIDGE_DAPP_FR &&
        finalProof.publicInputs[PUB_ACC_FR]  == EXPECTED_BRIDGE_ACC_FR,
        ERR_WRONG_BRIDGE_CONTRACT
    );

    // === Phase 2: expensive Halo2 KZG SHPLONK verifications ===================
    // Only reached after all public inputs are structurally consistent.
    require(verify_final(finalProof), ERR_INVALID_FINAL_PROOF);
    for (uint i = 0; i < hopProofs.length; ++i) {
        require(verify_multi_hop(hopProofs[i]), ERR_INVALID_HOP_PROOF);
    }

    // === Phase 3: settle ======================================================
    spent[nullifier] = true;
    _payoutWithdrawal(finalProof.publicInputs);
}
```

Phase 1 is cheap (field comparisons + one storage read for the window); phase 2 is the only heavy work (`1 + hopProofs.length` Halo2 KZG verifications).

**Verifier keys.** Two new Yul verifier contracts are generated by the existing `bridge-evm-aggregator` pipeline (`export-inner-aggregator` / `aggregate-proof`): one for `BridgeEventFinalProof` (replacing the current Circuit-4 verifier, since PIs grew from 11 to 13), one for `BridgeMultiHopProof` (new artifact). Both slot into [`contracts/ethereum/verifiers/`](../../../../contracts/ethereum/verifiers/). A verification-key rotation is a breaking change per repo policy — call it out in [`CHANGELOG.md`](../../../../CHANGELOG.md) under `## [Unreleased]` when the multi-thread PR lands.

**Alternative — single outer aggregation.** The bundle could be reduced to one on-chain verification by an additional outer SHPLONK aggregator that takes the FinalProof + up to `N_BUNDLE_MAX` hop snarks as inputs. This trades one more prover-side proof (server, minutes) for a large gas saving per withdrawal — worst case at `L = 300` today runs to `61 × ~750 K = ~46 M gas` per bundle, which becomes untenable long before the production cap. Recommended follow-up once the base design is in production; not on the critical path for the initial multi-thread landing.

### 6.5 Dynamic bundle size — no uniformity padding

`N_BUNDLE_MAX = ⌈L_MAX / H⌉` is the **upper bound**, not a fixed shape. Per-claim count:

| True chain length L | Hop snarks | Total snarks in bundle |
|---|---|---|
| 0  (t = 0)          | 0 | 1 (Final only) |
| 1                   | 1 | 2 |
| 2                   | 2 | 3 |
| ...                 | L | 1 + L |
| L_MAX = 20 (proto)  | 20 | 21 |
| L_MAX = 300 (prod)  | 300 | 301 |

Trailing snark padding: at `H = 1` a snark is either fully active (1 hop) or fully inactive (0 hops). Inactive snarks are never submitted — a chain of length `L` sends exactly `L` hop snarks.

The Solidity check at §6.4 accepts any `hopProofs.length ∈ [0, N_BUNDLE_MAX]`. Observers see L directly in `hopProofs.length`; that leak is acceptable in the bridge threat model (§9).

### 6.6 `BridgeMultiHopProof` circuit detail

At K = 17 with H = 1 hop. Structurally: the hop gadget of §4.2 wrapped in a `Circuit` implementation that publishes the two block-id endpoints as instances.

At H = 1 the "intra-snark continuity" rule (constraint 2 below) is vacuous — there is only one hop; bundle adjacency is enforced across snarks by the outer Solidity `withdrawByProofBundle` (§6.4). Constraint 2 is stated in the general form so a future switch to H > 1 (should EIP-170 room open up) needs no re-derivation.

```
witnesses:
  for h in 0..H:                                            (H = 1)
    is_active[h]                                            (bool, assert_bit)
    hop_current_block_id[h]                                 (32 bytes)
    hop_next_block_id[h]                                    (32 bytes)
    B_h.L0..L7_root, B_h.L8                                 (9 × 32 bytes; needed to reconstruct B_h.block_id via 4 SHA compressions)
    refs_tree_depth[h] ∈ [0, MAX_PROOF_BLOCK_REFS_DEPTH]    (u8; range-checked)
    ref_index[h] ∈ [1, 2^refs_tree_depth[h])                (u32; range-checked)
    L7_inner_path[h]                                        (8 × 32 bytes; unused steps ignored)

constraints:
  1. For each hop h in 0..H:
       when is_active[h]:
         - Reconstruct B_h.block_id from B_h.L0..L7, L8 via depth-4 SHA-256 tree (4 SHA compressions).
         - Constrain hop_current_block_id[h] == B_h.block_id (byte equality).
         - Tagged Poseidon leaf: ref_leaf = Poseidon(bytes_to_fr(REFERENCED_REF_BLOCK_TAG || A.block_id))
           where A.block_id = hop_next_block_id[h].
         - Variable-depth L7 fold: verify B_h.L7_root == open(ref_leaf, ref_index[h], L7_inner_path[h], refs_tree_depth[h]).
         - Direction bits inside the walker are bound to bit-decomposition of ref_index[h]
           via `dense_merkle_root_padded_bound` (§6.2).
       when !is_active[h]:
         - Byte equality: hop_next_block_id[h] == hop_current_block_id[h]  (identity propagation)
         - All other per-hop crypto constraints selector-multiplied off.

  2. Intra-snark continuity (unconditional; vacuous at H = 1):
       for h in 0..H-1: hop_current_block_id[h+1] == hop_next_block_id[h]

  3. Publish public instances (clear Fr, LE-packed):
       inst[0] = bytes_to_fr(hop_current_block_id[0])       ← PUB_HOP_START
       inst[1] = bytes_to_fr(hop_next_block_id[H-1])        ← PUB_HOP_END
       (Fr-encoding via the existing `gosh_dense_balanced_tree::bytes_to_fr` convention.)
```

**Cell budget.** H = 1 hop × 8 SHA compressions × 354 K + Poseidon overhead ≈ **2.83 M advice cells** — matches the shape of `historical-layer-hashes-movement-checker-circuit` (25 advice cols at K=17). The outer SHPLONK aggregator lands at `k_outer=21, Full` with predicted Yul ≈ 21 KB, well under the EIP-170 24 576 B cap. See [`CIRCUIT_COMPLEXITY_COMPARISON.md`](../../docs/CIRCUIT_COMPLEXITY_COMPARISON.md) §§2, 5 for the inner/outer-shape analysis and [`SHA256_INVOCATIONS.md`](../../docs/SHA256_INVOCATIONS.md) §4 for the SHA-cell budget.

### 6.7 `BridgeEventFinalProof` circuit detail

At K = 17 (with an advice-column bump; K sweep required — see §10.2.1). Evolution of the existing single-thread `BridgeEventProveCircuit` (`bridge_event_prove_circuit.rs`) with three additions.

**Two disjoint cryptographic subcircuits, glued by the withdrawal payload.**

- **X-side** (constraints 1–7 below): event BOC → `X.block_id`. Raw 4-cell BOC preimage bytes witnessed; SHA-256 in-circuit reconstructs `event_hash`, three parent-child hash links glue the wrapper / body / recipient / sender cells, ABI event id and `d1` refs-count sanity check the BOC descriptors, field extraction by byte-slice, Poseidon96 forms `ext_msg_leaf`, Poseidon dense-Merkle to L8, then a depth-4 SHA opening (4 SHA compressions) reconstructs `X.block_id`.
- **Y-side** (constraint 8): `Y.block_id` → `finalRoot`. Poseidon-based (Poseidon96 `block_leaf` + depth-8 Poseidon dense-Merkle to `#L1(M_Y)` + ≤ 11 dense-chain links to `finalRoot`).

The two sides share no block-side witness when `t ≠ 0`. When `t = 0` (X = Y), the prover passes identical bytes for `x_block_id` and `y_block_id`; the X-side L8 opening binds one, the Y-side `block_leaf` construction binds the other, and no special-case circuit logic is needed.

Witnesses and constraints:

```
witnesses:
  # X-side (event block; thread t; may equal Y when t=0)
  #
  # The event BOC is passed in as raw preimage bytes for its four cells (wrapper root
  # event cell + body + recipient + sender). Prover-side flattens the BOC; every hash
  # on the X-side is recomputed in-circuit.
  x_wrapper_cell_repr_data                          (variable length; SHA-256 preimage of root wrapper cell)
  x_body_cell_repr_data                             (variable length; SHA-256 preimage of body cell)
  x_recipient_cell_repr_data                        (variable length; SHA-256 preimage of recipient cell)
  x_sender_cell_repr_data                           (variable length; SHA-256 preimage of sender cell)
  x_body_hash_offset_in_wrapper                     (usize; structural constant per BOC layout)
  x_recipient_hash_offset_in_wrapper                (usize; structural constant per BOC layout)
  x_sender_hash_offset_in_wrapper                   (usize; structural constant per BOC layout)
  x_account_dapp_id, x_account_id                   (32 bytes each; ext-out-message endpoint identity)
  x_block_id                                        (32 bytes)
  X.L8_tracked_ext_out_messages_root                (32 bytes; identical to `ext_out_root` at :810 in current code)
  X_block_id_h07_sibling                            (32 bytes; the one live sibling of L8)
  events_pos                                        (u32; range-checked; events-tree slot index of the withdrawal event)
  X_ext_out_merkle_path                             (≤ MAX_EVENTS_TREE_DEPTH × 32 bytes; Poseidon siblings)

  # Y-side (anchor block; thread 0; equals X when t=0)
  y_block_id                                        (32 bytes)
  Y.envelope_hash                                   (32 bytes; unconstrained content)
  Y.tracked_ext_out_messages_root                   (32 bytes; unconstrained content)
  Y_block_leaf_path                                 (depth-8 Poseidon-dense siblings + leaf index)
  Y_dense_chain_links                               (≤ MAX_CHAIN_LEN = 11)

constraints:
  # ---- Unchanged from single-thread bridge circuit (see :71-93 in code) ----

  1. Event BOC SHA-256 chain (root + 3 child-hash equality links):
        event_hash        = SHA(x_wrapper_cell_repr_data)                              // in-circuit
        body_hash         = SHA(x_body_cell_repr_data)
        recipient_hash    = SHA(x_recipient_cell_repr_data)
        sender_hash       = SHA(x_sender_cell_repr_data)
        x_wrapper_cell_repr_data[body_off      .. body_off      + 32] == body_hash
        x_wrapper_cell_repr_data[recipient_off .. recipient_off + 32] == recipient_hash
        x_wrapper_cell_repr_data[sender_off    .. sender_off    + 32] == sender_hash

  2. ABI event id constraint:
        first 4 bytes of x_body_cell_repr_data     == 0x3c838959   // WithdrawalInitiated selector

  3. BOC descriptor sanity + field extraction:
        d1 bits of wrapper cell ⇒ refs_count == 3                    // wrapper embeds body, recipient, sender
        d1 bits of body / recipient / sender cells sanity-checked
        token_id      = BE u32  from x_body_cell_repr_data[54..58]
        amount        = BE u128 from x_body_cell_repr_data[38..54]
        dst_chain_id  = BE u256 from x_body_cell_repr_data[ 6..38]
        recipient_hi  = BE u80  from x_recipient_cell_repr_data[ 2..12]
        recipient_lo  = BE u80  from x_recipient_cell_repr_data[12..22]
        sender_acc_fr = algebraic decode from x_sender_cell_repr_data
        dapp_fr       = destination dApp id, Fr-encoded
        acc_fr        = destination account id, Fr-encoded

  4. Event → ext_out_tree leaf, then Poseidon-Merkle open to L8:
        ext_msg_leaf =  Poseidon96( x_account_dapp_id ‖ x_account_id ‖ event_hash )
        walk_dense_merkle_bind_pos(
            leaf = ext_msg_leaf,
            path = X_ext_out_merkle_path,
            pos  = events_pos,                                          // direction bits bound to bit-decomp of events_pos
        ) == X.L8_tracked_ext_out_messages_root

  # ---- NEW: X-side L8 opening (depth-4 SHA tree; 4 SHA compressions) ------

  5. X.block_id reconstruction:
        h89     = SHA(X.L8_tracked_ext_out_messages_root ‖ 0×32)   // L9 = 0×32 (constant)
        h8_11   = SHA(h89   ‖ H10_11_CONST)                         // sibling constant
        h8_15   = SHA(h8_11 ‖ H12_15_CONST)                         // sibling constant
        x_block_id_recomputed == SHA(X_block_id_h07_sibling ‖ h8_15)   // h0..7 live witness
        (No new ext_out witness is introduced; the existing ext_out_root produced by
        walk_dense_merkle_bind_pos at :810 IS the tree leaf being opened.)

  6. Bind X.block_id publication (constraint 13 numbering; see §6.3):
        constrain_equal(x_block_id_recomputed, x_block_id)
        x_block_id_fr_public == bytes_to_fr(x_block_id)          // instance [11]

  # ---- Y-side (existing single-thread flow, unchanged) -------------------

  7. Nullifier now binds X.block_id (rewire, no algorithmic change):
        block_id_fr_input_to_nullifier := bytes_to_fr(x_block_id)      // was: single conflated block_id
        nullifier = Poseidon(block_id_fr, tokenId, amount, recipientHi, recipientLo, senderAccFr, events_pos)
        nullifier_public == nullifier                                   // instance [8]

  8. Y-side anchor (unchanged from single-thread):
        block_leaf(Y)  =  Poseidon96( y_block_id ‖ Y.envelope_hash ‖ Y.tracked_ext_out_messages_root )
        block_leaf(Y) -- depth-8 Poseidon dense-Merkle path --> #L1(M_Y)
        #L1(M_Y)      -- dense chain (≤ 11 links)          --> finalRoot
        finalRoot_public   == finalRoot                                 // instance [9]
        anchorLayer_public == anchorLayer  (range-checked 1..=10)       // instance [10]

  9. Bind Y.block_id publication (NEW):
        y_block_id_fr_public == bytes_to_fr(y_block_id)                 // instance [12]

  10. Public voucher / withdrawal fields at instances [0..7] (unchanged).

  11. Destination identity — enforced on-chain (not in circuit):
        instance [6] == EXPECTED_BRIDGE_DAPP_FR
        instance [7] == EXPECTED_BRIDGE_ACC_FR
        (Comparison happens in `withdrawByProofBundle`, §6.4; the circuit only
        computes and exposes the values.)
```

**Cell budget.** Adds ~1.42 M advice cells (4 SHA compressions × 354 K each) to the single-thread K = 17 circuit. Current circuit already occupies most of K = 17 (~110 advice columns); the addition may require bumping to K = 18, **or** bumping the advice column count from 110 to ~150 within K = 17. K sweep required to lock the choice before landing (`test_k_sweep_benchmark` pattern from the `gosh_dark_dex_halo2_circuit` codebase provides a reusable template).

If the K bump is chosen, K = 18 grows the on-chain SHPLONK verifier proportionally — verify against EIP-170 during the aggregator export.

### 6.8 Bundle size and prover time

Prover runs on server hardware, so this is a throughput question, not a UX question.

| True chain length L | Hop snarks | Total snarks | Est. server prover time (parallel) |
|---|---|---|---|
| 0                    | 0 | 1  | ≈ 1–2 min                           |
| 1                    | 1 | 2  | ≈ 2–3 min                            |
| 5                    | 5 | 6  | ≈ 3–4 min (embarrassingly parallel)  |
| 20 (proto cap, desired upper end) | 20 | 21 | ≈ 5–10 min                   |
| 300 (worst-case ceiling, rare)    | 300 | 301 | ≈ 15–45 min (heavy parallelism) |

Numbers are order-of-magnitude, calibrated from stress runs of hop-shaped circuits of the same K = 17 shape. Hop snarks are embarrassingly parallel; the FinalProof is on the critical path.

### 6.9 Verifying-key set

Two VKs: `VK_BridgeEventFinal`, `VK_BridgeMultiHop`. No aggregation, no universal VK, no recursion. Both are consumed by the existing `bridge-evm-aggregator` SHPLONK pipeline to produce Yul verifier contracts under `contracts/ethereum/verifiers/`.

**Key rotation is a breaking change** per bridge repo policy ([`AGENTS.md`](../../../../AGENTS.md) §Changelog policy). The multi-thread landing PR must document:

- Retirement of the current single-thread Circuit-4 VK.
- Introduction of `VK_BridgeEventFinal` and `VK_BridgeMultiHop`.
- Any prover-side artifact schema changes (`proof_event_*.json`, witness JSON).

---

## 7. Synthetic test data generator

A binary in `bridge-event-prove-circuit/examples/` produces multi-thread fixtures for every supported configuration:

| Case | t     | L (real hops) | Hop snarks | Notes |
|------|-------|---------------|------------|-------|
| S0   | 0     | 0             | 0          | Same-thread; X = Y; hop-less bundle |
| S1   | ≠ 0   | 1             | 1          | Shortest cross-thread |
| S5   | ≠ 0   | 5             | 5          | Mid-range |
| S20  | ≠ 0   | 20            | 20         | Prototyping cap; also the desired upper end of normal operation |
| Sprod | ≠ 0  | 300           | 300        | Worst-case ceiling, not an expected shape — stress only (mark `#[ignore]`) |

Each fixture emits:

1. Witnesses + native proof for `BridgeEventFinalProof`.
2. Witnesses + native proofs for the applicable `BridgeMultiHopProof` snarks.
3. Native bundle verification: a pure-Rust mock of `withdrawByProofBundle` (`bundle_verifier.rs`) that replays the Solidity orchestration of §6.4 — same phase ordering, same error semantics.

All fixtures use one fixed `VK_BridgeEventFinal` + one fixed `VK_BridgeMultiHop`.

Reuse the existing `test_helpers::*` synthetic-witness builders where the shape overlaps with the single-thread flow. New helpers required: L7 walk fixture builder (produces a chain of blocks with consistent `refs` linkages and full L0..L7 leaf sets so the depth-4 SHA opening reconstructs each `block_id`), L8 depth-4 opening fixture builder (extends the current single-thread test_helpers with the four sibling constants and the `h0..7` witness sibling), bundle-level assembly (glue the fixtures together into a valid bundle with cross-snark continuity on the clear block-ids).

---

## 8. Why not `AggregationCircuit`

Rejection rationale. In-circuit aggregation via `AggregationCircuit` from snark-verifier-sdk was considered and rejected on the following grounds.

1. **Ethereum verifier size.** In-circuit KZG aggregation would push K to 21+ and pull in a heavyweight verifier column layout. Even after SHPLONK wrap the resulting Yul verifier does not fit under the EIP-170 24 576 B limit — the current on-chain path already relies on SHPLONK aggregation to fit an ordinary K=17 circuit; layering an in-circuit aggregator on top would either require nested SHPLONK or force a switch to a proxy-verifier pattern.
2. **SRS + toolchain cost.** K ≥ 21 requires 4–16 GB SRS files (the K=21 SRS was already an operational fix for the bridge — see the memory note on Hermez K=21 provisioning) and multi-GB working memory. The multi-proof composition of §6 reuses the existing K=17 halo2-base / halo2-ecc stack unchanged.
3. **Parallelism.** Aggregator proving is serial-dominant; the multi-proof design's `N` hop snarks prove concurrently.
4. **Bundle verification cost budget.** Per-snark SHPLONK verify ≈ 700–800 K gas. Worst case at L = 20 → 21 snarks → ~15 M gas. At L = 300 → 301 snarks → ~230 M gas — **untenable per single withdrawal well before the production cap**, so at some L ≥ threshold an outer-SHPLONK bundle aggregator becomes mandatory (§6.4 alternative). That trades one more prover-side aggregation round (server, minutes) for one on-chain KZG verify — cheaper than in-circuit aggregation because the outer aggregator stays outside the inner proof shape.
5. **Operational simplicity.** Multi-proof composition: 3 circuit definitions (Final + Hop + optional outer aggregator), 2 base VKs, plain Solidity continuity check. Aggregation: recursive composition, universal-VK hash chaining, in-circuit transcript matching, KZG accumulator unpacking — every one of which is an additional VK-rotation surface.

For the initial multi-thread landing, target the multi-proof composition. Enable outer-SHPLONK bundle aggregation as a follow-up once per-snark verification is landed and stable and once the L distribution on real deployment tells us where the per-bundle gas ceiling will actually bite.

---

## 9. What is exposed vs. hidden (no anonymity goal)

**The bridge does not aim for anonymity.** The withdrawal is settled by paying an Ethereum address, so the recipient is on-chain; the source of funds is on-chain in Acki Nacki. Every event field the bridge circuit binds is already public somewhere in the observable protocol state. This design section exists so future readers do not assume anonymity guarantees the design does not support, and so a future anonymity extension has a clean baseline to diff against.

### 9.1 Exposed on-chain

- **`X.block_id`, `Y.block_id`** — clear Fr-encoded 32-byte values as public inputs of `BridgeEventFinalProof` (§6.3). An observer learns exactly which AN block emitted the withdrawal event and which thread-0 anchor was used.
- **`hop_start_block_id`, `hop_end_block_id`** on every `BridgeMultiHopProof` — the entire cross-thread walk is public.
- **`hopProofs.length`** — reveals L directly (chain length from X to thread 0). Not a threat: L is a physical chain topology fact, unrelated to any user identity.
- **All withdrawal fields**: `tokenId`, `amount`, `recipient` (Ethereum address, split hi/lo), `dstChainId`, `senderAccFr` — public per the event ABI; the bridge is a public-good primitive on both sides.
- **`nullifier`** — public per spent-set tracking.
- **`finalRoot`, `anchorLayer`** — public per the layer-window mechanism.
- **`dappFr`, `accFr`** — the AN-side bridge contract's dApp and account id, compared on-chain against pinned constants.

### 9.2 Hidden

- **`X.envelope_hash`, `X.tracked_ext_out_messages_root`** — witnessed, not exposed (would leak nothing useful, but also serve no purpose to expose).
- **`Y.envelope_hash`, `Y.tracked_ext_out_messages_root`** — unconstrained witnesses; Y is only used as an anchor, not as an event source.
- **`events_pos`** — private witness bound to the events-tree walker's direction bits (`walk_dense_merkle_bind_pos`), enters the nullifier for replay disambiguation only.
- **Individual `refs[]` list entries** at each hop — only the specific `ref_block_id` opened per hop is disclosed (as `hop_next_block_id` = clear public input); the other refs stay opaque.

### 9.3 Replay protection (nullifier)

`nullifier = Poseidon(x_block_id_fr, tokenId, amount, recipientHi, recipientLo, senderAccFr, events_pos)`.

Unchanged from the current single-thread circuit modulo the `block_id → x_block_id` rewire. Because `x_block_id_fr` is now also a public input, the nullifier is verifier-computable from the public instance vector, which lets an off-chain observer audit the spent-set independently.

The nullifier binds the withdrawal to:

- the source event block (`x_block_id_fr`) — no block-level anonymity, per §9.1;
- every settled withdrawal field (`tokenId`, `amount`, `recipient`, `sender`) — full-tuple replay protection;
- the event's in-block position (`events_pos`) — disambiguates two identical `WithdrawalInitiated` events emitted in the same AN block.

### 9.4 What this design intentionally omits

For a bridge developer working on a future extension: several primitives that a privacy-preserving variant would need are **deliberately absent** here.

- **No salt witness.** No `voucher_secret_seed`, no `salt = Poseidon(DOMAIN_TAG, seed)` derivation, no `salt_commitment` public.
- **No position-tagged salted endpoints.** No `salted_id(salt, block_id, position)` construction; endpoints are `bytes_to_fr(block_id)` directly.
- **No `bundle_index`.** With `H = 1` and dynamic bundle length there is no meaningful bundle-slot to hide anyway.
- **No uniformity padding.** `hopProofs.length` varies per claim; the bundle shape leaks L.

A future anonymity-preserving variant would restore these primitives, pad bundles to constant `N_BUNDLE`, and re-hide `X.block_id` / `Y.block_id` behind salted endpoints. This is out of scope for the current design.

---

## 10. Locked parameters and open questions

### 10.1 Locked

| Parameter | Value | Source / rationale |
|---|---|---|
| BWS | 128 | `HISTORY_PROOF_WINDOW_SIZE` (canonical) |
| Layer-1 batch tree depth | 8 (130 real leaves + 126 padding = 256) | Existing `verify_chain_of_dense_proofs` flow |
| Block-id tree depth | **4** (16 leaves) | [`BLOCK_ID_ALG_NEW.md`](../../docs/BLOCK_ID_ALG_NEW.md) |
| L9..L15 padding | `[0u8; 32]` | Chain constant (`node/src/types/ackinacki_block/mod.rs:556`) |
| L8 semantics | `tracked_ext_out_messages_root` (Poseidon dense-Merkle root) | Current bridge circuit; §2.4 |
| Ext-out-messages tree | Poseidon dense-Merkle, depth ≤ 8 (`MAX_EVENTS_TREE_DEPTH`), padded with `[0u8; 32]` | `bridge_event_prove_circuit.rs:124` |
| L7 outer opening depth per hop | **4** SHA-256 sibling combines (8 compressions) | §4 |
| `MAX_PROOF_BLOCK_REFS` | **256** leaves padded, depth 8 | Protocol cap |
| `H` (hops per `BridgeMultiHopProof`) | **1** | Forced by EIP-170 24 576 B cap on Yul verifier size (§5.H) |
| `L_MAX` (max real chain length) | prototype **20**; worst-case ceiling **300** (node-team hard upper bound, not a target); ~128 same-thread parent hops also possible via slot 0; **desired average 10–20, ideally 1–5** | Prover wall time scales linearly in L |
| `N_BUNDLE_MAX` (max hop snarks per claim) | prototype **20**; worst-case **300** | Dynamic per-claim, upper bound only; = `⌈L_MAX / H⌉`; typical claims land in the 1–20 range |
| `MAX_CHAIN_LEN` (thread-0 dense chain) | **11** | `gosh-dense-balanced-tree` |
| `MAX_ANCHOR_LAYER` | **10** | Must equal `MAX_LAYER_HASHES` in `AckiNackiBridge.sol` |
| `MAX_EVENTS_TREE_DEPTH` | **8** | `bridge_event_prove_circuit.rs:124` |
| `BridgeMultiHopProof` K | **17** | See §5.H table |
| `BridgeEventFinalProof` K | **17 (with column bump)** or **18** — decide via K sweep | +4 SHA compressions vs. current single-thread K=17 (§6.7) |
| SHA-256 chip | `gosh-sha256-chip` | Existing dependency |
| On-chain verifier | per-snark Halo2 KZG via existing SHPLONK aggregator | No aggregation initially |
| Public inputs (`BridgeEventFinalProof`, 13) | see §6.3 | Preserves 11-slot prefix; appends X/Y block-ids |
| Public inputs (`BridgeMultiHopProof`, 2) | see §6.3 | New artifact |

### 10.2 Open questions

1. **K sweep on `BridgeEventFinalProof`.** Decide K=17 with wider advice columns vs. K=18. Blocker: run a `test_k_sweep_benchmark` before locking the SHPLONK aggregator wiring.
2. **Outer-SHPLONK bundle aggregation.** Whether to ship the multi-thread landing with per-snark on-chain verification (simple; ~15 M gas at L=20; ~230 M gas at L=300 — the latter is untenable) or with outer aggregation (one on-chain verify; extra prover round). See §6.4 alternative. Recommendation: ship per-snark first, add outer aggregation as a follow-up before L exceeds the ~30-hop-per-bundle gas ceiling in practice.
3. **Naming convention for the new final circuit.** Keep `BridgeEventProveCircuit` (existing name evolved) vs. rename to `BridgeEventFinalProofCircuit` (matches the "Final + Hop" taxonomy used throughout this doc). Downstream consumers (`bridge-event-prover-lib`, `bridge-event-witness`) will need mechanical updates either way.
4. **Ethereum-side entrypoint.** Whether to add a new function `withdrawByProofBundle` (backwards-compatible during rollout) or repurpose `withdrawByProof` (cleaner, but rotates the ABI). Coordinate with `contracts/ethereum/` and [`EVM-contracts-spec.md`](../../../../docs/EVM-contracts-spec.md).
5. **Handling `t = 0` on Ethereum without a special-case branch.** The layout in §6.4 requires `xBlockId == yBlockId` when `hopProofs.length == 0`. Confirm this Solidity branch is well-formed under gas / calldata reasoning — an alternative is to always require at least one hop snark, using an "identity hop" for `t = 0`, at the cost of one extra 2-instance snark per claim.

---

## 11. Circuit maintenance notes

The multi-thread circuit code described above has landed on `feature/multithreading` — the extended `bridge_event_prove_circuit.rs` plus the sibling files `multi_hop_proof.rs`, `multi_hop_witness.rs`, `bundle_verifier.rs` and `test_helpers.rs` under `bridge-event-prove-circuit/src/`.

### 11.1 Refresh the on-chain verifier whenever the circuits change

Any change to a circuit's constraints, public-input layout, `K`, or advice column count rotates its verification key and therefore its on-chain Yul verifier. When that happens:

- **Re-export the SHPLONK verifier(s).** Run `bridge-evm-aggregator export-inner-aggregator` for every affected circuit; the fresh Yul source + bytecode lands under `contracts/ethereum/verifiers/`. Confirm the bytecode stays within EIP-170 via `scripts/check_verifier_sources.sh` (and the `verifier_sources.yaml` CI job that compiles each `*AggregatorVerifier.sol` with `solc` 0.8.19 to its `.bin` byte for byte).
- **Register the new VK** on the Ethereum side (`contracts/ethereum/`) and record the rotation in [`CHANGELOG.md`](../../../../CHANGELOG.md) under *Breaking Changes* — per [`AGENTS.md`](../../../../AGENTS.md), a rotated VK is always a breaking change because proofs produced for the previous circuit stop verifying.
- **Refresh any pinned proof fixtures** that embed the old VK (e.g. `bridge-snark-utils/proofs/bound/` via `export-bound-block-proofs`, and any `#[ignore]`d real-prover fixtures in this crate).

### 11.2 Known deferrable items

- **Upstream 4-bit hardcode** in `gosh-dense-balanced-tree::dense_merkle_root_circuit_padded`. The `is_less_than(j_const, num_active_levels, 4)` call inside the padded walker hard-codes a 4-bit range for the depth witness. Values in `[8, 16)` collapse to "all levels active" via that comparison, so there is no cheating window at the current `MAX_PROOF_BLOCK_REFS_DEPTH = 8`, but the hardcode couples the upstream helper to an assumption of the consumer. A cross-repo fix in `gosh-halo2-crypto-lib` should either parameterise the bit-width or accept it as an argument. Track when the upstream is next touched.
- **Outer-SHPLONK bundle aggregator** (§6.4 alternative). Deferred until per-snark verification is landed and stable.

---

## 12. Migration notes from the single-thread bridge circuit

For reviewers who know the current `BridgeEventProveCircuit` code, this section enumerates the diff-level changes:

| Current (`bridge_event_prove_circuit.rs`) | Multi-thread |
|---|---|
| Single `block_id: [u8; 32]` witness (line 346) | Split into `x_block_id` (bound via L8 opening) and `y_block_id` (bound via `block_leaf`) |
| `ext_out_root` (line 810) → discarded after feeding `block_leaf` (line 822) | Same value now ALSO fed as L8 leaf into a new 4-SHA depth-4 opening producing `x_block_id_fr` |
| `block_id_fr` witness feeds both `block_leaf` (line 822) and nullifier (line 972) | `x_block_id_fr` → nullifier; `y_block_id_fr` → `block_leaf` |
| `TOTAL_PUBLIC_INPUTS = 11` (line 152) | `TOTAL_PUBLIC_INPUTS = 13`; append `PUB_X_BLOCK_ID = 11`, `PUB_Y_BLOCK_ID = 12` |
| Circuit K = 17, ~110 advice columns | K = 17 with wider advice OR K = 18; K sweep decides |
| Circuit file structure: single circuit | Add sibling `multi_hop_proof.rs`, `multi_hop_witness.rs`, `bundle_verifier.rs` |
| On-chain: one Circuit-4 SHPLONK verifier | Two Yul verifiers (final + hop); orchestration in `withdrawByProofBundle` |
| Downstream witness builder produces one witness per event | Produces a bundle (1 final + `L` hop witnesses) |

Every existing public-input slot 0..10 keeps its current byte-for-byte semantics — downstream consumers that hardcode `PUB_TOKEN_ID..PUB_ANCHOR_LAYER` do not need to move, they only need to grow their vector length from 11 to 13 and read the two new tail slots.

---

## 13. Cross-repository references

- **Bridge single-thread circuit being extended:** `crates/bridge-circuits/bridge-event-prove-circuit/src/bridge_event_prove_circuit.rs`.
- **Bridge block-id doc:** [`crates/bridge-circuits/docs/BLOCK_ID_ALG_NEW.md`](../../docs/BLOCK_ID_ALG_NEW.md).
- **Bridge global-history-data doc (Y-side anchor infrastructure):** [`crates/bridge-circuits/docs/GLOBAL_HISTORY_DATA_SPEC.md`](../../docs/GLOBAL_HISTORY_DATA_SPEC.md).
- **Bridge thinning spec (earlier fixed-stride model; superseded for the circuit by the layer-N key-block cadence in `GLOBAL_HISTORY_DATA_SPEC.md`, still partially reflected in `crates/bridge-relayer-daemon`):** [`crates/bridge-circuits/docs/BRIDGE_PROVER_THINNING_SPEC.md`](../../docs/BRIDGE_PROVER_THINNING_SPEC.md).
- **Bridge SHA-256 accounting:** [`crates/bridge-circuits/docs/SHA256_INVOCATIONS.md`](../../docs/SHA256_INVOCATIONS.md).
- **Bridge circuit complexity analysis:** [`crates/bridge-circuits/docs/CIRCUIT_COMPLEXITY_COMPARISON.md`](../../docs/CIRCUIT_COMPLEXITY_COMPARISON.md).
- **AN node source of truth for block Merkle leaves:** `node/src/types/ackinacki_block/{mod.rs, merkle.rs}`.
- **AN node source of truth for L7 / layer-N trees:** `node/libs/history-proof/src/lib.rs`.
- **Off-chain reference chain-walk implementation:** acki-nacki `helpers/proof_helper/src/gql_proof.rs`.
- **Ethereum on-chain verifier:** `contracts/ethereum/src/AckiNackiBridge.sol` (`_isKnownLayerAnchor`, `withdrawByProof`).
- **SHPLONK aggregator tooling:** `crates/bridge-evm-aggregator`.
- **DEX companion spec (same primitives, different threat model — anonymity, salt, uniformity):** `dexdo-halo2-kit/MULTITHREAD_DEX_CIRCUIT_SPECIFICATION.md`.
