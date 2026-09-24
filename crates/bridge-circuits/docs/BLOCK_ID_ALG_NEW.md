# Canonical `block_id` — 16-leaf, depth-4 SHA-256 Merkle tree

**Scope.** This document records the canonical block-id construction
adopted by `acki-nacki@poseidon_profile_new` (HEAD `36cd98721`,
2026-07-07) and pinned by this branch of the bridge circuits
(`feature/block_id_upgrade_for_16_leafs`). Only the block-id
construction is recorded here — nothing else. For rolling-window
history-proof accumulation, per-thread windows, layer roots, and
their Ethereum-side mirroring, read `GLOBAL_HISTORY_DATA_SPEC.md`.

**Source of truth.** `MULTITHREAD_CIRCUIT_SPEC.md` §2 in the
sibling repo `dexdo-halo2-kit`. This file is the block-id-only slice
of that spec, distilled for the bridge circuit repo.

**Node-side reference implementation.** `Self::block_merkle_leaves()`
at `node/src/types/ackinacki_block/mod.rs:400`
on `acki-nacki@poseidon_profile_new` — with
`pub const BLOCK_MERKLE_LEAF_COUNT: usize = 16`.

---

## Construction

Every block has

```
block_id = root of a 16-leaf SHA-256 Merkle tree of depth 4
```

The 16 leaves are:

| Leaf | Definition | Hash family |
|------|------------|-------------|
| L0  | Poseidon of the block's thread layer-root snapshot (`Poseidon(history_proofs_preimage)`, `LAYER_PREIMAGE_SIZE = 331 B`) | Poseidon |
| L1  | `SHA-256(bincode(CommonSection))` | SHA-256 |
| L2  | `Poseidon(old_bk_set_hash)` — zero if no BK change | Poseidon |
| L3  | `Poseidon(new_bk_set_hash)` — zero if no BK change | Poseidon |
| L4  | TVM block representation hash | SHA-256 / TVM |
| L5  | `SHA-256(bincode(durable_state_update))` | SHA-256 |
| L6  | `SHA-256(tx_cnt.to_be_bytes())` | SHA-256 |
| L7  | Poseidon dense-Merkle root of `[parent_block_id, refs…]` — index-tagged Poseidon leaves | Poseidon |
| L8  | `tracked_ext_out_messages_root` — SHA-256 root of this block's ext-out messages tree | SHA-256 |
| L9..L15 | `[0u8; 32]` — fixed zero padding | — |

Combine rule at every level: `SHA-256(left_32B ‖ right_32B)`. Fifteen
SHA-256 invocations total to fold 16 leaves into `block_id`.

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

### Right-subtree constants (L9..L15)

L9..L15 are all `[0u8; 32]`, so the right-hand subtree collapses to
constants that a circuit can hard-code:

- `h10-11 = SHA-256([0;32] ‖ [0;32])`
- `h12-13 = h10-11`
- `h14-15 = h10-11`
- `h12..15 = SHA-256(h12-13 ‖ h14-15) = SHA-256(h10-11 ‖ h10-11)`
- `L9 = [0;32]`

Opening any leaf costs **4 SHA-256 compressions** — one per tree
level.

---

## What matters for this repo

- **L0 sits at index 0.** Its content is
  `Poseidon(history_proofs_preimage)` — the single leaf that Circuit 2
  opens.
- **The block-id tree has depth 4** (16 leaves, `sha256_pair` at every
  level). Circuit 2 (`historical-layer-hashes-movement-checker-circuit`)
  is pinned to this shape by commit `655cec3`:
  `NUM_MERKLE_SIBLINGS = 4`, Section B built with 4 SHA-256 stages.
- **Circuits 1A / 1B / 4 take `block_id` opaquely** and are
  unaffected by the shape change.
- **L1..L15 are opaque to Circuit 2.** Circuit 2 walks `L0 → h01 →
  h0..3 → h0..7 → block_id` with 4 witness siblings (`L1`, `h23`,
  `h4..7`, `h8..15`); it never parses L1..L15 individually.
