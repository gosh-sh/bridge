# `GlobalHistoryData` — Spec & Algorithm

**Status:** Source-grounded reverse-engineering of the acki-nacki node, intended as a foundation for designing how the Ethereum-side bridge contract should store layer hashes.

**Source repo:** `acki-nacki` at the `poseidon_profile_new` branch (HEAD `36cd98721`). This branch pins the canonical 16-leaf depth-4 SHA-256 block-id Merkle tree consumed by this repo — see `BLOCK_ID_ALG_NEW.md` (sibling doc in this directory) for the leaf layout.

**Why this document exists.** Circuit 4 (`bridge-event-prove-circuit`) commits to a single `finalRoot` (public input slot 9) plus a prover-supplied `anchorLayer` routing hint (slot 10, `1..=MAX_ANCHOR_LAYER`). Anchor identity is enforced **off-circuit** by `AckiNackiBridge.sol` scanning `layerWindows[anchorLayer]` for `finalRoot`; there is no in-circuit `public_latest_layer_hashes` array or private index any more. See the module-level comment of `bridge_event_prove_circuit.rs` for the current 11-slot public-inputs layout. This document was originally written for the earlier `NUM_LAYER_HASHES`-wide candidate-vector design, so anywhere it says "the proof commits to a vector of latest layer hashes and picks one privately", read it as "the proof commits to a single `finalRoot` and the on-chain contract does the membership check against `layerWindows[anchorLayer]`" — the underlying question this document answers ("which subset of layer hashes must the Ethereum-side contract keep?") is unchanged, and §8's `layerWindows[L]` model is already the current shape. The rest of the document — what `GlobalHistoryData` is, how the node accumulates it, and how it discards entries — is what the "minimal subset" argument at the end rests on.

---

## TL;DR

- `GlobalHistoryData` is an in-memory, per-node, per-thread, per-layer **rolling-window** of 256-bit hashes.
- Hashes are arranged in **layers 0..=10**:
  - **Layer 0** holds Poseidon-block-leaf hashes for individual blocks (`Poseidon(block_id ‖ envelope_hash ‖ ext_out_messages_root)`). Appended **once per finalized block** — not gated on any modulus, not gated on whether the block actually emitted any tracked external out messages. When the block has no events, `ext_out_messages_root` is the all-zero `[0u8; 32]` value (see `tracked_ext_out_messages_root` doc-comment: *"Zero if there were no such messages."*), so the leaf is still well-defined and still appended.
  - **Layer L ≥ 1** holds the Poseidon-Merkle root of the **previous full Layer (L-1) window**. Appended only when the producing block's height satisfies `height % W^L == 0` (the layer-L "key block of order L"), so layer-L's window grows `W^L` times more slowly than layer 0's.
- Each layer is a **fixed-size circular buffer** of size `W = HISTORY_PROOF_WINDOW_SIZE`. **In reality `W = 128`** — that is the production value and also the value the bridge is locked to, including for all bridge integration tests. The number `8` you'll see in worked examples throughout this document (and in the source snippet at §1.2) is a **pedagogical convenience**: small enough to draw circular-buffer pictures with, but mechanically identical to `W = 128`. The counter `data_len` only tracks "how many appends in the current cycle"; the underlying `[[u8;32]; W]` array is **never zeroed**. Result: at any instant the array physically contains the **last W distinct hashes** pushed at that layer, even though `data_len` resets to 0 every W appends.
- Layers do **not** interact during pruning. When a new layer-(L+1) hash is produced, nothing at layer L is discarded — layer L continues filling/overwriting its own window independently. The only coupling is the *value* of the new layer-(L+1) entry, which is a Poseidon-Merkle root computed from layer L's current physical contents.
- `common_section.history_proofs` only carries layer-≥1 roots, **never** layer-0 leaves. Layer 0 is re-derived by the receiver from `(block_id, envelope_hash, tracked_ext_out_messages_root)`, all of which the block already carries.
- Maximum reach (per thread): top-layer window of W hashes covers `W^{MAX_LAYERS+1}` blocks — with `W=128`, `MAX_LAYERS=10`, that is `128^{11} ≈ 9.4 × 10^{23}` blocks. So even a tiny contract-side mirror of the top layers covers astronomical history.

---

## 1. Data structure

### 1.1 Types

`node/src/types/history_proof.rs:139–162`

```rust
pub type LayerNumber = u8;                                       // valid range 0..=10

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, TypedBuilder, Getters)]
pub struct ProofLayerRootHash {
    layer: LayerNumber,
    root_hash: [u8; 32],
    block_height: BlockHeight,
    block_id: BlockIdentifier,
}

pub type HistoryLayerData = BTreeMap<LayerNumber, HistoryBlockData>;
pub type GlobalHistoryData =
    Arc<RwLock<HashMap<ThreadIdentifier, Arc<RwLock<HistoryLayerData>>>>>;
pub type GlobalHistoryDataSnapshot =
    HashMap<ThreadIdentifier, HistoryLayerData>;
```

`node/src/types/history_proof.rs:207–213`

```rust
#[derive(Clone, Getters)]
pub struct HistoryBlockData {
    thread_id: ThreadIdentifier,
    last_processed_block_height: BlockHeight,
    data_len: usize,
    data: [[u8; 32]; HISTORY_PROOF_WINDOW_SIZE],   // fixed-size circular buffer
}
```

### 1.2 The window constant

`node/src/types/history_proof.rs:23–24`

```rust
pub const HISTORY_PROOF_WINDOW_SIZE: usize = 8;
```

> **About the value `8`.** The node source has carried `8` as the in-tree constant for as long as this spec has tracked it. The deployed Acki Nacki value — and the value the bridge is hard-wired to in both production and tests — is **`128`**. The spec keeps `W = 8` only because the worked examples below (circular-buffer pictures, layer-cadence tables, climb/forward/descend transcripts) are far easier to read with eight slots than with one hundred and twenty-eight. Every algorithmic claim is independent of the specific value: substitute `W = 128` and the same equations, the same trigger conditions, and the same chain-shape catalogue hold.
>
> Concretely for the bridge: `history_proof::HISTORY_PROOF_WINDOW_SIZE = 128` (defined in `acki-nacki/node/libs/history-proof/src/lib.rs`), and `bridge-prover-lib::THINNING_FACTOR_P = 4`, so one bundle covers `W·P = 512` source blocks. The bridge prover vendors the constant in `bridge_prover_lib::poseidon_dense` (byte-identical to the node's `history-proof` crate) so the two cannot drift.

The constant is the **only knob** for window size. The on-chain layout, the TVM opcode, the circuit, and the bridge prover are all derived from it.

### 1.3 Where it lives

`node/src/repository/repository_impl.rs:271`

```rust
pub struct RepositoryImpl {
    ...
    history_proof_data: GlobalHistoryData,
    ...
}
```

The data structure is **node-local**, not on-chain. It is rebuilt on node restart from disk via `load_history_data` and persisted via `save_history_data` (both in `repository_impl.rs`, around line 1086 and 1486). The on-chain manifestation is in each block's `common_section.history_proofs`: a `BTreeMap<LayerNumber, ProofLayerRootHash>` carrying only the **layer roots** for layers `L ≥ 1` that are appended on this very block (i.e., for the block heights where `height % W^L == 0`). **Layer 0 leaves are never carried in `history_proofs`** — they are re-derived by the receiver from `(block_id, envelope_hash, tracked_ext_out_messages_root)`, all of which the block already carries (the last as a standalone field on `common_section`, not under `history_proofs`).

### 1.4 Conceptual shape

```
GlobalHistoryData
└── thread_id_1 ──► HistoryLayerData (BTreeMap)
│       ├── 0 ──► HistoryBlockData { data_len, data: [hash; W] }   // block leaves
│       ├── 1 ──► HistoryBlockData { data_len, data: [hash; W] }   // L0 roots
│       ├── 2 ──► HistoryBlockData { data_len, data: [hash; W] }   // L1 roots
│       └── ...up to 10
└── thread_id_2 ──► ...
```

---

## 2. What a "layer hash" actually is

### 2.1 Layer 0 — block-leaf hash (Poseidon over 96 bytes)

`node/src/types/history_proof.rs:181–192`

```rust
pub fn compute_block_leaf_hash(
    block_id: &[u8; 32],
    envelope_hash: &[u8; 32],
    ext_out_messages_root: &[u8; 32],
) -> [u8; 32] {
    let hasher = PoseidonHasher::new();
    let mut buf = [0u8; 96];
    buf[..32].copy_from_slice(block_id);
    buf[32..64].copy_from_slice(envelope_hash);
    buf[64..96].copy_from_slice(ext_out_messages_root);
    hasher.digest(&buf)
}
```

This is **the same** `Poseidon96` used by `bridge-event-prove-circuit::poseidon_hash_96_native` (the `(block_id, envelope_hash, ext_out_root)` block leaf inside the events Merkle proof). So a layer-0 hash *is* the event-tree block-leaf hash for that block.

### 2.2 Layer L ≥ 1 — Poseidon-Merkle root of the previous layer's full window

When the producer encounters `block_height % W^L == 0` (and `block_height ≠ 0`), it builds a fresh Poseidon Merkle tree whose leaves are:

```
[0]        higher_layer_root     ← chain link upward
[1]        same_layer_root       ← chain link to previous L-window
[2..W+1]   W hashes from layer (L-1)
           (padded with zeros up to next power of two)
```

The root of that tree is then `update_from_pure_data`'d into layer `L` and embedded into the block's `common_section.history_proofs[L]` (see §3.2).

This layout is also what Circuit 2 (`historical-layer-hashes-movement-checker-circuit`) takes as its in-circuit witness — see §8.

---

## 3. Append algorithm

There are **two distinct cadences**, and they must not be confused:

### 3.1 Layer 0 — appended per finalized block

The function `repository_impl.rs::add_finalized_block_history_data` (see §3.3) runs unconditionally on every finalized block and pushes one hash into layer 0:

```rust
// Always — no modulus check at this site.
let leaf_hash = compute_block_leaf_hash(block_id, envelope_hash, ext_out_root);
base_layer_data.update_from_pure_data(leaf_hash, block_height)?;
```

So in steady state: **every finalized block deposits exactly one new entry into layer 0**. The append is not conditional on the block carrying any external out messages — when there are none, `ext_out_root = [0u8; 32]` (per the doc-comment on `tracked_ext_out_messages_root`) and the leaf still hashes cleanly. So layer 0 grows monotonically with finalized block height, regardless of event activity.

### 3.2 Layer L ≥ 1 — produced only on key blocks of order L

The producer (`block_producer.rs:1250`) gates layer-≥1 production behind a modulus check on the candidate block's own height:

```rust
if *candidate_block.common_section().block_height() % HISTORY_PROOF_WINDOW_SIZE == 0
    && *candidate_block.common_section().block_height().height() != 0 { ... }
```

The block where this condition fires is called a **key block** (height divisible by `W`, height ≠ 0). On a key block the producer:

1. **Always emits a layer-1 hash.** It computes a Merkle root over layer 0's current physical window and inserts a `ProofLayerRootHash { layer: 1, root_hash, ... }` into `common_section.history_proofs`.
2. **Conditionally emits higher-layer hashes.** It then iterates: divide `height` by `W` once and check `% W == 0` again — if so, emit a layer-2 hash; divide again, check again — emit layer 3; and so on. The loop terminates at the first non-key division. (`block_producer.rs:1345–1404`.)

The result is that layer L's append cadence is:

| Layer | Trigger | Cadence at `W = 128` (prod) |
|-------|---------|------------------------------|
| 0 | every finalized block | every block |
| 1 | `height % W   == 0` and `height ≠ 0` | every 128 blocks |
| 2 | `height % W²  == 0` and `height ≠ 0` | every 16 384 blocks |
| L | `height % W^L == 0` and `height ≠ 0` | every `W^L` blocks |
| 10 | `height % W¹⁰ == 0` and `height ≠ 0` | every `≈ 1.18 × 10²¹` blocks |

Each layer-L hash therefore *summarises `W^L` blocks*; a full window of `W` layer-L hashes covers `W^{L+1}` blocks.

On finalization, all of the `(layer, root_hash)` pairs embedded in `common_section.history_proofs` are folded back into the node's `GlobalHistoryData` (§3.3) — but layer 0 is *also* updated in the same pass, this time per individual block.

The producer's backfill loop (`block_producer.rs:1271–1308`) handles a special case: on restart, layer 0's `data_len` may be below `W`, so the producer walks back through ancestors and re-appends their leaf hashes to fill the current window before computing the new layer-1 root.

### 3.3 The fold-back on finalization

`node/src/repository/repository_impl.rs:1563–1609`

```rust
fn add_finalized_block_history_data(&self, block_state, block) -> anyhow::Result<()> {
    let leaf_hash = compute_block_leaf_hash(block_id, envelope_hash, ext_out_root);
    base_layer_data.update_from_pure_data(leaf_hash, block_height)?;      // always layer 0
    for (layer, data) in block.common_section().history_proofs() {        // possibly layer ≥ 1
        let layer_data = history_lock.entry(*layer).or_insert(...);
        layer_data.update_from_pure_data(*data.root_hash(), *data.block_height())?;
    }
    Ok(())
}
```

So a non-key block deposits exactly **one** entry (into layer 0). A key block deposits **two** entries (layer 0 from the block itself + layer 1 from `history_proofs`). A key block whose height is divisible by `W²` deposits **three** entries (layers 0, 1, 2), and so on.

Note the asymmetry made visible by the snippet: the loop iterates over `block.common_section().history_proofs()`, and that map *only ever has entries for layers ≥ 1*. The layer-0 leaf is **always** recomputed by the receiver from `(block_id, envelope_hash, ext_out_root)` — it is never carried on the wire under `history_proofs`. The producer side (`block_producer.rs:1335-1404`) inserts only `layer: 1` and higher; there is no path that inserts `layer: 0`.

### 3.4 The push itself: `update_from_pure_data`

`node/src/types/history_proof.rs:267–292`

```rust
pub fn update_from_pure_data(&mut self, leaf_hash: [u8; 32],
                             block_height: BlockHeight) -> anyhow::Result<()> {
    ensure!(*self.last_processed_block_height.height() == 0
            || block_height.height() > self.last_processed_block_height.height());
    if self.data_len == HISTORY_PROOF_WINDOW_SIZE {
        self.clear_data();              // <- only resets `data_len`, see §4
    }
    self.data[self.data_len] = leaf_hash;
    self.data_len += 1;
    self.last_processed_block_height = block_height;
    Ok(())
}
```

Pseudocode:

```
APPEND(layer L, hash H, height bh):
  require bh > last_processed_height[L]   // monotone
  if data_len[L] == W:
      data_len[L] = 0                     // logical reset
  data[L][ data_len[L] ] = H              // overwrite slot
  data_len[L] += 1
  last_processed_height[L] = bh
```

---

## 4. Discard / prune — what really happens when a window fills

The prune routine is one line:

`node/src/types/history_proof.rs:294–296`

```rust
pub fn clear_data(&mut self) {
    self.data_len = 0;
}
```

That is the **entire** discard algorithm. It does **not** zero `self.data`. The trick: the physical `[[u8;32]; W]` array is recycled in place; the counter just rewinds and subsequent writes overwrite the oldest slots first.

### 4.1 What happens within a single layer — diagram

Take `W = 8` (this document's exposition value — recall the bridge actually runs at `W = 128` in both prod and tests; see §1.2). Layer 0 starts empty and receives one hash per finalized block. Numbers `h₁, h₂, …` denote successive block-leaf hashes:

```
After block #1  (data_len=1):   [h₁,  ⋅ ,  ⋅ ,  ⋅ ,  ⋅ ,  ⋅ ,  ⋅ ,  ⋅ ]
After block #2  (data_len=2):   [h₁,  h₂,  ⋅ ,  ⋅ ,  ⋅ ,  ⋅ ,  ⋅ ,  ⋅ ]
...
After block #8  (data_len=8):   [h₁,  h₂,  h₃,  h₄,  h₅,  h₆,  h₇,  h₈ ]   ← window now full
                                                                             this is also a key block
                                                                             (height 8 % W == 0)

Block #9 arrives → update_from_pure_data is called:
  ─ data_len == W = 8  → clear_data():  data_len  := 0     (array UNTOUCHED)
  ─ write data[0] := h₉                 data_len  := 1

After block #9  (data_len=1):   [h₉,  h₂,  h₃,  h₄,  h₅,  h₆,  h₇,  h₈ ]
                                  ↑
                       the ONLY slot that changed — h₁ is now lost forever
                       (unless it was already aggregated into a higher layer; see §4.3)

After block #10 (data_len=2):   [h₉,  h₁₀, h₃,  h₄,  h₅,  h₆,  h₇,  h₈ ]
After block #15 (data_len=7):   [h₉,  h₁₀, h₁₁, h₁₂, h₁₃, h₁₄, h₁₅, h₈ ]
After block #16 (data_len=8):   [h₉,  h₁₀, h₁₁, h₁₂, h₁₃, h₁₄, h₁₅, h₁₆]
After block #17 (data_len=1):   [h₁₇, h₁₀, h₁₁, h₁₂, h₁₃, h₁₄, h₁₅, h₁₆]   ← h₂ evicted
                                  ↑
                       same trick — slot 0 overwritten; the array always
                       physically holds the W=8 most-recent block leaves.
```

**Effective semantics: a true sliding window of size W.** At any point in time, layer 0's physical array contains exactly the last `W` finalized-block leaves. `data_len` is bookkeeping for "where the next write goes"; it does not narrow what's physically there.

### 4.2 What happens between layers — diagram

This is the part that was previously unclear: **adding a new layer-(L+1) hash does NOT touch layer L's contents**. The two windows are completely independent stores. The only relationship is that, *at the moment* a layer-(L+1) hash is computed, its value is a Poseidon-Merkle root over layer L's current physical array — but layer L is not mutated, cleared, or reset by that computation.

Continuing the example with `W = 8`:

```
At block height 8 (key block, height % W = 0):
  ─ Layer 0's physical array is [h₁, h₂, h₃, h₄, h₅, h₆, h₇, h₈]
  ─ Producer computes  R₁ := Poseidon-Merkle([h₁ .. h₈])
  ─ Embeds (layer=1, root=R₁) in common_section.history_proofs
  ─ On finalization, fold-back appends R₁ to layer 1 — *not* to layer 0:

      Layer 0  (data_len=8):   [h₁, h₂, h₃, h₄, h₅, h₆, h₇, h₈]   ← UNCHANGED
      Layer 1  (data_len=1):   [R₁, ⋅,  ⋅,  ⋅,  ⋅,  ⋅,  ⋅,  ⋅]   ← new entry

At block height 9 (non-key block, 9 % W ≠ 0):
  ─ Layer 0 receives h₉, evicting h₁ as in §4.1.
  ─ Layer 1 stays the same — no key-block trigger, no append.

      Layer 0  (data_len=1):   [h₉, h₂, h₃, h₄, h₅, h₆, h₇, h₈]
      Layer 1  (data_len=1):   [R₁, ⋅,  ⋅,  ⋅,  ⋅,  ⋅,  ⋅,  ⋅]

At block height 16 (next key block):
  ─ Layer 0's physical array now holds [h₉, h₁₀, …, h₁₆].
  ─ Producer computes  R₂ := Poseidon-Merkle([h₉ .. h₁₆])  and appends to layer 1.

      Layer 0  (data_len=8):   [h₉,  h₁₀, h₁₁, h₁₂, h₁₃, h₁₄, h₁₅, h₁₆]
      Layer 1  (data_len=2):   [R₁,  R₂,  ⋅,   ⋅,   ⋅,   ⋅,   ⋅,   ⋅  ]

…fast-forward to height 64 (still W = 8, so 64 = W²; this is the first
"layer-2 key block")…

      Layer 0  (data_len=8):   [h₅₇, h₅₈, h₅₉, h₆₀, h₆₁, h₆₂, h₆₃, h₆₄]
      Layer 1  (data_len=8):   [R₁,  R₂,  R₃,  R₄,  R₅,  R₆,  R₇,  R₈ ]
                                                                     ↑
                       all 8 layer-1 roots accumulated; producer now
                       merkles them into a layer-2 root S₁:

      Layer 0  (data_len=8):   [h₅₇ .. h₆₄]   ← UNCHANGED
      Layer 1  (data_len=8):   [R₁  ..  R₈ ]  ← UNCHANGED
      Layer 2  (data_len=1):   [S₁, ⋅, ⋅, ⋅, ⋅, ⋅, ⋅, ⋅]   ← new
```

So the rule is simple:

> When a layer-(L+1) hash is produced, **layer L is read but not modified**. Layer (L+1)'s window receives one new entry via the same `update_from_pure_data` / counter-reset trick from §4.1. Pruning at any layer is a purely local event scoped to that layer.

### 4.3 Failure mode the spec inherits

Because each layer holds only `W` hashes, a layer-0 hash is "directly recoverable" only for the most recent `W` blocks. Once block `B` is more than `W` blocks behind the head, `B`'s leaf no longer lives in layer 0 — but its information is **already baked into** the layer-1 root that covers it (and, transitively, into the higher layer that covers that). To prove `B` to the bridge contract, the prover must therefore climb through the layer hierarchy until it reaches a layer-L root the contract still has in its window — exactly the role of Circuit 4's `verify_chain_of_dense_proofs` step.

In particular: a block whose height aligns with `W^L` for some `L ≤ MAX_LAYERS` ends up "anchored" in layer L's window for `W` × `W^L = W^{L+1}` blocks of head-progress, then anchored in layer (L+1), and so on. The deeper a layer, the longer the anchoring persists.

---

## 5. Initialization and recovery

`node/src/repository/repository_impl.rs:1510–1557`

When the node boots at a non-genesis block height `H`, `init_history_proof_data` seeds each layer with `count` zero-hashes computed from a fixed-base-W decomposition of `H`:

```
INIT(height H):
  max_level = ⌊log_W(H)⌋
  for i in 0..=max_level:
    modulus = W^(i+1)
    step    = W^i
    count   = ⌊(H mod modulus) / step⌋
    start   = ⌊H / modulus⌋ * modulus
    if start ≠ 0 or i == 0:
        count += 1
    else:
        start += step

    hd = HistoryBlockData::new()
    for c in 0..count:
        hd.update_from_pure_data([0;32], BlockHeight(start))
        start += step
    layer_map[i] = hd
```

The seeded entries are zero hashes whose only purpose is to make `data_len` match where we are in the current cycle of each layer; they are immediately overwritten as real data lands during catch-up.

If the on-disk snapshot exists, it is preferred (`load_history_data`).

---

## 6. Persistence

`node/src/repository/repository_impl.rs:1086–1134, 1486`

- Format: bincode of `GlobalHistoryDataSnapshot = HashMap<ThreadIdentifier, HistoryLayerData>`.
- Location: `{data_dir}/{history_path}/{DEFAULT_OID}`.
- Write trigger: `dump_state()` at finalization.

There is **no GraphQL surface** for the structure itself. The `gql-server` does decode each block's `common_section.history_proofs` (block-local, not the rolling state) — `gql-server/src/schema/graphql_shared/block/mod.rs:152–272`. The rolling state must be reconstructed by replaying blocks, which is exactly what Circuit 2 attests to.

---

## 7. Cross-reference to Circuit 2 (`historical-layer-hashes-movement-checker-circuit`)

The circuit proves an **append-only state transition** of `GlobalHistoryData` for one thread:

> *Given the previous top-layer hash `prev_max_level_layer_hash` and a Poseidon dense-Merkle chain of length ≤ `MAX_CHAIN_LEN = 11`, the **new** top-layer hash equals `layer_hash_frs[num_layers - 1]`.*

### 7.1 Public instances (14)

| Index | Value |
|-------|-------|
| 0 | `block_id` (SHA-256 Merkle root of the block) |
| 1 | `bk_set_poseidon_hash` (passthrough) |
| 2 | `num_layers` (1..10) |
| 3..12 | `layer_hash_frs[0..9]` (inactive = 0) |
| 13 | `prev_max_level_layer_hash` |

### 7.2 Critical circuit constants

- `MAX_LAYERS = 10` — matches the TVM opcode's `1..=10` range and source's `LayerNumber` cap.
- `LAYER_PREIMAGE_SIZE = 1 + 10 * 33 = 331` bytes — the encoding of `(num_layers, [(layer_no, root_hash); 10])` that gets Poseidon-hashed into leaf `L0` of the 16-leaf depth-4 block-id SHA-256 Merkle tree. The circuit re-derives `L0 = Poseidon(preimage)` and walks the depth-4 SHA path to `block_id` with 4 opaque 32-byte siblings.
- `tree_depth` (per real-prover test, **not** a circuit-shape parameter) = `ceil(log2(W + 2))` — depth-specific VK/PK, see the circuit's README.

---

## 8. Implications for the Ethereum bridge contract

The bridge contract reproduces `GlobalHistoryData` for **all** layers `L ≥ 1` faithfully, key-block-by-key-block, with the prover/relayer protocol tightened to make that mirror authoritative.

The core decisions, summarised up-front:

1. The contract holds a `HashMap<layer, HistoryWindow>` keyed by `layer ∈ {1, 2, …, MAX_LAYERS = 10}`.
2. Each `HistoryWindow` stores `data_len` (active-entry count, `0..W`) and an array of up to `W` layer hashes. This is a 1:1 mirror of the node's `HistoryBlockData` from §1.1 — minus the unused `thread_id` field, which is encoded by the outer per-thread mapping.
3. **The prover is contractually obliged to submit a (Circuit 1A or 1B) + Circuit 2 bundle for every key block** (height `h` with `h ≠ 0` and `h % W == 0`) of the source chain.
4. **A Circuit 4 (withdrawal) proof is admissible only after the (Circuit 1+2) bundles for the key blocks anchoring its target layer-hash have been relayed.** This gating is automatic: the slot the prover wants to anchor against does not appear in `layerWindows` until the producing key block's Circuit 2 has been ingested, and Circuit 4 verification fails on any other slice.

The rest of this section justifies each decision against the spec above and gives the concrete storage layout, control-flow, and open issues.

### 8.1 The append cadence the contract sees

The contract's view of cadence follows §3 directly, with the prover enforcing it externally:

| Trigger | Bridge interaction required |
|---------|-----------------------------|
| Key block of order 1 (`h % W == 0`, `h % W² ≠ 0`) | (Circuit 1A or 1B) + Circuit 2 bundle. Circuit 2's public instances carry one layer-1 root. |
| Key block of order L (`h % W^L == 0`, `h % W^{L+1} ≠ 0`) | (Circuit 1A or 1B) + Circuit 2 bundle. Circuit 2's public instances carry roots for layers `1..L`. |

On a key block of order L, the producer (§3.2) emits one `ProofLayerRootHash` per layer in `1..L` inside `common_section.history_proofs`. Circuit 2's public outputs include `num_layers` and the active `layer_hash_frs[0..num_layers-1]`. The contract calls `appendLayer(layer, root, h)` for each of them, into the corresponding `layerWindows[layer]` slot.

This is exactly the §3.3 fold-back, executed on Ethereum instead of in the node, with the constraint that the relayer **must not skip key blocks**. Skipping a key block would desync the contract's `layerWindows` from the node's `GlobalHistoryData`, and the contract enforces monotone per-layer heights (`require(blockHeight > w.lastHeight)`) which would prevent the gap from being silently papered over by a later submission. The relayer either keeps up with key blocks or `verifyBlock` rejects subsequent submissions until the gap is filled.

### 8.2 Why Circuit 4 must wait for the relevant key block(s)

A withdrawal event is committed at some block `B`. To prove it, the off-chain prover walks Circuit 4's Poseidon chain up to a layer-L root the contract already has stored.

That root only exists in `layerWindows[L]` after the (Circuit 1+2) bundle for the *producing key block* of layer L (height `⌈B / W^L⌉ × W^L`) has been relayed and verified. So the user-facing latency for a withdrawal is now decomposed cleanly:

1. **Wait for the next layer-1 key block** to be produced on the source chain: at most `W − 1` blocks, mean `W/2`. At `W = 128`, 4 s/block → mean ~4 min.
2. **Wait for the relayer to submit the (Circuit 1+2) bundle** for that key block. With a properly-incentivised relayer this is constant + one consensus round.
3. **Submit Circuit 4.** Anchors against `layerWindows[1]`'s newly-appended entry.

If the user takes too long and their layer-1 entry rolls out of `layerWindows[1]` (after `W² ≈ 16 K` blocks ~18 hours at `W = 128`), the layer-2 root that summarises it is still in `layerWindows[2]` — assuming the layer-2 key block at height `⌈B / W²⌉ × W²` has *also* been relayed by then. This pattern recurses up to `layerWindows[MAX_LAYERS = 10]`. With the full mirror in place, the bridge's effective withdrawal-age horizon is bounded only by `W^{MAX_LAYERS+1}` blocks — astronomical at `W = 128`.

Concretely: the prover stops climbing the chain at the smallest layer L such that the key block of order L summarising `B` has been relayed and the resulting root is still in `layerWindows[L]`. Circuit 4's `verify_chain_of_dense_proofs` step enforces the climb is correct; the contract enforces the anchor exists.

> **Caveat — anonymisation.** This "stop at smallest L" rule is acceptable for first testing only. The bridge's primary goal is hiding *which* block a user is proving; a deterministic stop layer leaks the block-age range (the prover's `L` discloses that `B` falls in the layer-L window's coverage band). For production we need a non-deterministic anchor-layer policy — open design question, to be revisited.

### 8.3 Storage layout (per thread)

```solidity
uint256 constant W           = 128;     // matches HISTORY_PROOF_WINDOW_SIZE
uint8   constant MAX_LAYERS  = 10;      // matches Acki Nacki's LayerNumber cap

struct HistoryWindow {
    bytes32[W] data;          // fixed-size W-slot circular buffer of layer-L roots
    uint256    dataLen;       // how many of the W slots have ever been written (clamps at W)
    uint256    writeCursor;   // index of the slot that the NEXT append will overwrite (0..W-1)
    uint256    lastHeight;    // last block height that appended into this window (monotone)
}

// key = layer number, ∈ {1, 2, ..., MAX_LAYERS}.  Layer 0 is never stored.
mapping(uint8 => HistoryWindow) internal layerWindows;
```

Semantics:

- `data[0..W-1]` is a fixed-size circular array. It is never resized and never zeroed; old entries are overwritten in place when the cursor wraps.
- `dataLen` is the *count of real entries* currently held in `data`. It starts at `0`, increments by one on every `appendLayer` call, and clamps at `W` — after the first `W` appends every slot holds a real entry forever.
- `writeCursor` is the *next-write index*. Each `appendLayer` writes to `data[writeCursor]` and then advances `writeCursor = (writeCursor + 1) mod W`. So once `dataLen ≥ 1`, the newest entry is at `data[(writeCursor + W − 1) mod W]`, and once `dataLen == W`, the oldest entry is at `data[writeCursor]` (the slot about to be overwritten).
- `dataLen` and `writeCursor` together encode the same thing the node tracks as a single position counter `data_len` (which the node interprets `mod W` as the write index). Splitting them lets the contract answer "how many real hashes are in this window?" without having to remember whether `data_len` has wrapped yet.

Total per-thread storage:

| Component | Size at `W = 128`, `MAX_LAYERS = 10` |
|-----------|---------------------------------------|
| `data` arrays | `10 × 128 × 32 B = 40 960 B` (40 KB) |
| `dataLen`, `writeCursor`, `lastHeight` per layer | `10 × 3 × 32 B = 960 B` |
| **Total** | **~41 KB / thread** |

With `T` threads, this scales linearly. The single-thread devnet (current target) pays 41 KB once.

### 8.4 The append routine

```solidity
function appendLayer(uint8 layer, bytes32 hashValue, uint256 blockHeight) internal {
    require(layer >= 1 && layer <= MAX_LAYERS, "layer out of range");
    HistoryWindow storage w = layerWindows[layer];
    require(w.lastHeight == 0 || blockHeight > w.lastHeight, "non-monotone height");

    w.data[w.writeCursor] = hashValue;
    w.writeCursor = (w.writeCursor + 1) % W;
    if (w.dataLen < W) {
        w.dataLen += 1;
    }
    w.lastHeight = blockHeight;
}
```

Equivalent to the node's `update_from_pure_data` (§3.4) under the encoding mapping of §8.3. Key invariants preserved:

- Monotone per-layer heights: `appendLayer(L, ·, h)` requires `h > lastHeight[L]`. Mirrors §3.4's `ensure!`.
- Bounded `dataLen`: ≤ `W` at all times. After the first `W` calls per layer it saturates at `W`.
- Circular overwrite: physical slot `data[writeCursor]` is overwritten, mimicking §4.1's "the array is never zeroed; the counter just rewinds".

