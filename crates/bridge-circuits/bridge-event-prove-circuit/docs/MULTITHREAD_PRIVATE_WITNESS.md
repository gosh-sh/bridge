# Cross-thread private witness — spec for Misha

**Audience:** Misha, author of the AN-side service/library that will serve
Bridge Circuit 4's cross-thread private witness on demand.
**Scope:** only the cross-thread leg (`Y → … → X`). The X-side event binding
and the Y-side layer ladder are **already handled on our side** and are
summarised at the bottom (§4) only so we agree on how all three pieces glue.

Canonical references:
- Circuit witness types: `crates/bridge-circuits/bridge-event-prove-circuit/src/multi_hop_witness.rs`
- JSON mirrors we deserialise: `crates/bridge-prover-libraries/bridge-event-witness/src/schema.rs`
  (`BlockWitnessJson`, `HopWitnessJson`, `MultiHopProofWitnessJson`,
  `MultiHopBundleWitnessJson`)
- Full crypto rationale: `crates/bridge-circuits/bridge-event-prove-circuit/docs/MULTITHREAD_BRIDGE_EVENT_CIRCUIT_SPECIFICATION.md`
- Already-canonical server-side primitives Misha can reuse:
  `acki-nacki/helpers/proof_helper/src/gql_proof.rs`
- Prototype BFS walker (Python, drives the research collector):
  `bridge/multithreading/research/multipath_collector.py`


---

## 1. What we need in one sentence

### Arrow convention — read before every diagram below

Throughout this document, an arrow `A → B` reads **"A is a leaf in `B`'s
L7, i.e. `A.block_id` appears in `B.proof_block_refs` at some slot
`k ≥ 0`"**. The tail of the arrow is the *older* block (the leaf of
the per-block L7 Poseidon tree); the head is the *newer* block (the
root direction, which carries that leaf in its refs). Reading
left-to-right walks **leaf → root → leaf → root …** i.e. oldest →
newest.

**Both edge kinds are valid**, as spelled out in
`bridge/multithreading/README.md` §2.0:

- **slot 0** = `parent_block_id` — same-thread parent (previous `seq_no`
  in the same thread). Chain-side `proof_block_refs[0]` always carries it
  and it lives inside the same L7 Poseidon tree as the cross-thread refs
  (`acki-nacki/helpers/proof_helper/src/gql_proof.rs:76,109`).
- **slots ≥ 1** = cross-thread refs (strictly older blocks in another
  thread).

Opening either kind is identical in shape and cost; the walker chooses
whichever slot yields the shortest total path. For example, path
topology (b) in the README (same-thread walk-back + one cross-thread
hop) is almost entirely slot-0 edges.

### The chain

Given an event block **X** (any thread `t`, possibly `t = 0`) identified
by its `block_id`, produce a bundle that proves there is a chain of
cross-thread `refs` edges

```
    X   →   B_{L-1}   →   …   →   B_2   →   B_1   →   Y
   ^(thread t,                                        (thread 0,
    event block)                                      anchored on Ethereum)
```

i.e. **X is a ref inside `B_{L-1}.L7`, which is a ref inside
`B_{L-2}.L7`, …, which is a ref inside `Y.L7`**. The walk reads
left-to-right as "the event block, then successively newer blocks that
contain the previous block as a cross-thread ref, until a thread-0
block Y is reached". We want **the shortest such chain**, because each
hop costs ~8 SHA-256 compressions in the SNARK and the aggregator fee
is linear in `L`.

When `t = 0` the bundle is empty (`L = 0`, no hops). We detect this
ourselves — the service only needs to answer the cross-thread case.

---

## 2. Shortest-path selection

### 2.1 The input

```
pick_shortest_path(x_block_id: [u8;32]) -> MultiHopBundleWitnessJson
```

Everything else (anchor layer selection on the Y side, event binding on
the X side) is derived afterwards.

### 2.2 The algorithm (reverse BFS over the block DAG)

Already prototyped in `multipath_collector.py` (§ "BFS on the reverse
graph"). Port the idea to Rust:

1. Build / cache an index
   `parents_of[child_block_id] = {(container_block_id, slot_k)}` where
   `container_block_id.proof_block_refs[slot_k] == child_block_id`.
   Index every slot (both the slot-0 same-thread parent chain and the
   slot-≥1 cross-thread refs — see §1). The reverse index is a view of
   the full ref DAG; you can keep it in memory + lazily hydrate from
   GQL.
2. BFS newest-ward from `x_block_id` (follow the reverse index: at each
   step jump from a block to blocks that contain it as a cross-thread
   ref — in diagram terms, follow the arrows). The **first** node
   reached whose `thread_id == 0` is the shortest Y. Record predecessors
   to reconstruct the chain `[X, B_{L-1}, …, B_1, Y]` (same order as
   §1's diagram, left-to-right).
3. Multiple-anchor diversity (several Y's at the same shortest length)
   doesn't matter — pick any one. We only need **one** path.
4. Caveats:
   - **Emit the shortest path even when it uses slot-0 edges.** Per §1,
     slot 0 and slots ≥ 1 are equally legitimate chain-side.
     `bridge-event-prove-circuit` and `bridge-event-witness` accept any
     `ref_index ∈ [0, 2^refs_tree_depth)` and select the Poseidon leaf
     tag (`REFERENCED_PARENT_BLOCK_TAG` vs `REFERENCED_REF_BLOCK_TAG`)
     on `is_zero(ref_index)`.
   - If no Y is reachable within a hop budget `L_MAX = 300`
     (production — spec §0.2 and §4.4), return a hard error. For
     prototyping, `L_MAX = 20` is enough.
   - Shortest-path minimisation matters: a naive "walk the parent
     chain only" path can be `L_MAX`-ish, while BFS over every `refs`
     edge (both kinds) finds short indirect paths through a sibling
     thread (README §2.0 topology (c); `multipath_algorithm.md`
     §"Why BFS over every edge").

### 2.3 Why the shortest matters

Hop count determines bundle size. Each hop = one `BridgeMultiHopProof`
SNARK (at `H_HOPS_PER_PROOF = 1`, spec §5), ~8 SHA-256 compressions +
one SHPLONK aggregation slot. Doubling the hops ≈ doubles the aggregator
wall time. We will accept a longer path **only** if BFS proves no
shorter one exists.

---

## 3. The witness bundle format

### 3.1 Shape

Wire format: JSON, exactly `MultiHopBundleWitnessJson` from
`bridge-event-witness::schema`. One `MultiHopProofWitnessJson` per hop
(because `H_HOPS_PER_PROOF = 1`). For a chain of length `L` the bundle
has `L` elements in `snarks[]`.

Order (**fixed by the circuit — do not flip**): `snarks[0]` is the
Y-end hop (`hop_start = Y`, rightmost arrow in §1's diagram),
`snarks[L-1]` is the X-end hop (`hop_end = X`, leftmost arrow). Each
hop carries `hop_start` (root-side) and `hop_end` (leaf-side) per the
§1 convention. Cross-snark continuity:
`snarks[i].hops[0].hop_end_block_id_hex == snarks[i+1].hops[0].hop_start_block_id_hex`.

Mapping between the diagram and the bundle (walk order newest → oldest,
i.e. the order the SNARKs are indexed):

| diagram arrow (leaf → root) | which snark | hop_start (newer) | hop_end (older) |
|---|---|---|---|
| `B_1 → Y`                   | `snarks[0]` | `Y`               | `B_1`           |
| `B_2 → B_1`                 | `snarks[1]` | `B_1`             | `B_2`           |
| …                           | …           | …                 | …               |
| `X → B_{L-1}`               | `snarks[L-1]` | `B_{L-1}`       | `X`             |

### 3.2 One hop — full field map

For each hop `i` (where `i = 0` is the Y-end, `i = L-1` is the X-end
per §3.1), `B_i_newer = hop_start` is the root-side block whose L7
we open, and `B_i_older = hop_end` is the leaf-side block picked out
of `B_i_newer.proof_block_refs` at any slot `k ∈ [0, refs.len())` (see
§1 for why both edge kinds are allowed):

```jsonc
{
  "is_active": true,                         // false only for trailing pad; we never emit padded hops at H=1
  "block": {
    // `block` describes `B_i_newer` — the newer (root-side) block of the hop,
    // the one whose L7 we open to extract `B_i_older`.
    "block_id_hex":                 "<B_i_newer.block_id, 32B hex>",
    "block_merkle_tree_leaves_hex": [        // all 16 leaves of B_i_newer's block-id tree (spec §2)
      "<L0 — history_proofs Poseidon root; zero if B_i_newer is not a thread-0 key block>",
      "<L1 — SHA256(bincode(CommonSection))>",
      "<L2 — old_bk_set Poseidon root; zero if no BK change>",
      "<L3 — new_bk_set Poseidon root; zero if no BK change>",
      "<L4 — TVM sub-block Poseidon root>",
      "<L5 — SHA256(bincode(DurableThreadAccountsStateDiff))>",
      "<L6 — SHA256(tx_cnt.to_be_bytes())>",
      "<L7 — proof_block_refs Poseidon root; the one we're opening>",
      "<L8 — tracked_ext_out_messages_root>",
      "0000…00",                             // L9
      "0000…00",                             // L10
      "0000…00",                             // L11
      "0000…00",                             // L12
      "0000…00",                             // L13
      "0000…00",                             // L14
      "0000…00"                              // L15 — all seven are fixed zero on chain
    ],
    "proof_block_refs_hex": [                // = [parent_block_id, refs[0], refs[1], …] of B_i_newer
      "<B_i_newer.parent_block_id>",         // slot 0 (same-thread parent)
      "<B_i_newer.refs[0]>",                 // slot 1 (cross-thread)
      "<B_i_newer.refs[1]>",                 // slot 2 (cross-thread)
      "…"                                    // any entry here may be B_i_older.block_id
    ]
  },
  "block_merkle_leaf_proof_l7_hex": [        // SHA-256 depth-4 opening of L7 against B_i_newer.block_id
    "<sibling at level 0 = L6>",
    "<sibling at level 1 = SHA(L4||L5)>",
    "<sibling at level 2 = SHA(SHA(L0||L1)||SHA(L2||L3))>",
    "<sibling at level 3 = SHA(subtree over L8..L15)>"
  ],
  "ref_index": 3,                            // index of B_i_older.block_id inside proof_block_refs_hex; k ∈ [0, refs.len())
  "refs_tree_depth": 2,                      // = ceil(log2(proof_block_refs.len())); range [0, 8]
  "proof_block_ref_inner_path_hex": [        // Poseidon dense-Merkle opening of ref_index against L7, padded to 8
    "<sibling_level_0>",                     // only the first `refs_tree_depth` entries are real
    "<sibling_level_1>",
    "0000…00",                               // levels [refs_tree_depth .. 8) are zero-padding (gated off in circuit)
    "0000…00",
    "0000…00",
    "0000…00",
    "0000…00",
    "0000…00"
  ],
  "hop_start_block_id_hex": "<B_i_newer.block_id>",  // == block.block_id_hex for active hops; the root-side block
  "hop_end_block_id_hex":   "<B_i_older.block_id>"   // == proof_block_refs_hex[ref_index]; the leaf-side block
}
```

**On wire-format redundancy.** The JSON record intentionally carries
fields that are derivable from other fields in the same record — the
circuit does not need both halves, but the native builder uses the
redundancy as a defense-in-depth gate:

- `block_merkle_leaf_proof_l7_hex` (4 SHA siblings) is derivable from
  `block_merkle_tree_leaves_hex` via `block_merkle_leaf_proof(leaves, 7)`
  (`multi_hop_witness.rs:309-323`). The native builder re-derives it
  and verifies against `block_id_hex` before shipping
  (`enrich.rs:915-927`).
- `proof_block_ref_inner_path_hex` (8-level Poseidon siblings,
  zero-padded) is derivable from `proof_block_refs_hex` + `ref_index`
  via `proof_block_ref_inner_path_native(refs, ref_index)`. The native
  builder re-derives it and verifies against L7 (`enrich.rs:928-936`).

The circuit itself reads only `block_merkle_tree_leaves_hex[7]` (= L7),
the two sibling paths, `ref_index`, `refs_tree_depth`, and the clear
endpoints (`multi_hop_proof.rs:235, 697`). The other 15 leaves of
`block_merkle_tree_leaves_hex` and the full `proof_block_refs_hex` list
are **not consumed in-circuit** — they stay on the wire so the Rust
builder can recompute both openings and bail with a clear error if the
chain data is inconsistent. If a slimmer JSON (siblings-only, no leaves
/ no full refs list) is preferable on the service side, flag it before
trimming — the current Rust ingest expects the full fields.

### 3.3 Field-level sourcing notes

| Field | How to derive it |
|---|---|
| `block_id_hex` | GQL `Block.hash` (= `block_id`) of `B_i_newer`. |
| `block_merkle_tree_leaves_hex` | Request `block_merkle_tree_leaves` from GQL. **Michael Vlasov already landed this field on the node side** (bridge devlog memory `bridge_gql_multithread_client_gap`); please confirm it is served on the shellnet/devnet build you target. L9..L15 are literally `[0u8; 32]` on chain (`node/src/types/ackinacki_block/mod.rs:556`). |
| `proof_block_refs_hex` | GQL `Block.proof_block_refs` — matches `[parent_block_id, refs[0..n]]`. Served alongside `block_merkle_tree_leaves` per the devlog note above. |
| `block_merkle_leaf_proof_l7_hex` | Re-derive natively from the 16 leaves using the recipe already implemented in `proof_helper::gql_proof::block_merkle_leaf_proof(leaves, leaf_index = 7)`. Four SHA-256 siblings. |
| `ref_index` | Position of `B_i_older.block_id` in `proof_block_refs_hex` of `B_i_newer`. Any `k ∈ [0, proof_block_refs.len())`; emit the shortest-path value even when `k = 0` (§2.2 caveat covers the circuit-side gating). |
| `refs_tree_depth` | `ceil(log2(max(proof_block_refs.len(), 1)))`. |
| `proof_block_ref_inner_path_hex` | Re-derive natively: tag each leaf with `REFERENCED_PARENT_BLOCK_TAG` (slot 0) or `REFERENCED_REF_BLOCK_TAG` (slot ≥ 1), run `compute_referenced_blocks_root` from `node/libs/history-proof/src/lib.rs`, collect the sibling chain at `ref_index`. Pad to 8 with `[0u8; 32]`. |
| `hop_start_block_id_hex` / `hop_end_block_id_hex` | Clear block-ids: `B_i_newer.block_id` (the one whose L7 we open — root side) and `proof_block_refs[ref_index]` (= `B_i_older.block_id`, leaf side) respectively. |

Everything here already exists as native helpers in acki-nacki's
`proof_helper` + `history-proof` crates. The service is a thin assembler
over them — no new crypto to implement.

### 3.4 Sanity checks the service must do before returning

- `is_active == true` for every emitted hop.
- `ref_index ∈ [0, proof_block_refs.len())` — do not special-case `k = 0`.
- `refs_tree_depth ∈ [0, 8]`.
- `hop_end_block_id_hex == proof_block_refs_hex[ref_index]`.
- Cross-hop continuity:
  `snarks[i].hop_end == snarks[i+1].hop_start` for all `i`.
- Final hop: `snarks[L-1].hop_end == x_block_id`.
- First hop: look up `snarks[0].hop_start`'s `thread_id`; must be `0`.

A failure in any of these means the Rust prover will reject the bundle
at circuit ingest, so catching them server-side turns a cryptic
MockProver constraint failure into a clear error for the operator.

---

## 4. The two edges we handle ourselves — context only

So we agree on the full picture, but **you do not need to produce these**:

### 4.1 X-side: `event_hash → tracked_ext_out_messages_root → X`

- `event_hash = repr_hash(ExtOut Message wrapper cell)`.
- Opening into L8 of X's block-id tree.
- Already implemented in `bridge-event-witness` (crate `schema::PrivateWitness`,
  fields `events_tree_proof`, `h07_sibling_hex`, `block_context`,
  `entries[4]`).
- Produced today by `bridge-event-private-witness-export` (hermetic) +
  `bridge-event-witness-builder` (daemon enrichment). Both compose into
  the final JSON we send to the prover.

### 4.2 Y-side: `block_leaf(Y) → #L1(M_Y) → … → #L<N> = finalRoot → layerWindows[]`

- Poseidon dense-Merkle up the layer ladder, exact scheme in spec §3.
- Already implemented single-thread in `bridge_event_prove_circuit.rs:848-876`.
- The daemon picks the smallest `anchorLayer` whose root is still inside
  the on-chain `layerWindows[]` window (spec §1.3, memory
  `bridge_history_proofs_multithread_delta`).
- Y here is the block the service returns as `snarks[0].hop_start`.
  Once we know `Y.block_id` + its `block_seq_no`, we drive the
  layer-ladder witness ourselves from existing node data; no new
  interface needed from Misha for this piece.

---

## 5. Proposed service surface

Shape of what we would consume (feel free to pick different names / wire
format, this is the semantic contract):

```rust
/// Returns the shortest cross-thread witness bundle for a given event block.
/// Empty `snarks` iff X is on thread 0.
fn get_cross_thread_witness(x_block_id: [u8; 32])
    -> Result<MultiHopBundleWitnessJson, CrossThreadWitnessError>;

enum CrossThreadWitnessError {
    EventBlockNotFound,
    NoAnchorWithinBudget { budget: usize, best_so_far: Option<usize> },
    GqlError(…),
    InternalInvariantViolation(…),  // e.g. hop_end_block_id_hex != proof_block_refs_hex[ref_index]
}
```

Alternatively, a tiny HTTP service:
`GET /cross_thread_witness?x_block_id=<hex>` → JSON above. Up to you.

We just need **one shortest bundle**, served quickly enough that the
bridge daemon can call it inline per withdrawal (expect sub-second on a
hot cache, up to a few seconds cold — a one-off per withdrawal, not a
hot loop).

---

## 6. Priority / order of landing

1. **Correctness of a single-hop case first.** A withdrawal from a
   non-zero thread whose event block is referenced directly by a
   thread-0 block (`L = 1`). This stresses the full field set but no
   BFS beyond depth 1. If we can prove this end-to-end, every longer
   case is purely more hops of the same shape.
2. **Short indirect paths (`L = 2..4`)** via a sibling non-zero thread.
   These exercise the BFS layer and prove it does more than a parent
   walk.
3. **Longer paths up to `L_MAX = 20`** for the prototype cap.
4. **Production `L_MAX = 300`** — same code, bigger ceiling + whatever
   caching the DAG index needs to stay sub-second.

Open the single-hop case against the shellnet node that already serves
`block_merkle_tree_leaves` and `proof_block_refs` (the fields Michael
landed per `bridge_gql_multithread_client_gap`). Everything else here
is downstream of those two fields plus `thread_id`.
