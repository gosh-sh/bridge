# Circuit 2: Historical Layer Hashes Movement Checker

Proves that layer hash root values are committed in the block ID Merkle tree (leaf L0) and verifies the Poseidon dense Merkle chain linking previous layer hashes to current ones.

## Public Instances (14)

| Index | Value |
|-------|-------|
| 0 | `block_id` (root of the 16-leaf depth-4 SHA-256 Merkle tree) |
| 1 | `bk_set_poseidon_hash` (passthrough) |
| 2 | `num_layers` (1..10) |
| 3..12 | `layer_hash_frs[0..9]` (inactive = 0) |
| 13 | `prev_max_level_layer_hash` |

## Block-ID Merkle Tree

`block_id` is the SHA-256 Merkle root of the canonical **16-leaf, depth-4** block-id tree from the `poseidon_profile_new` branch of `acki-nacki`. Combine rule at every internal node: `SHA-256(left_32B || right_32B)`.

| Leaf | Semantic | Notes |
|------|----------|-------|
| L0 | `Poseidon(layer_hashes_preimage)` | **The one leaf this circuit opens.** |
| L1 | `SHA-256(bincode(CommonSection))` | opaque here |
| L2 | `old_bk_set_hash` | `[0u8;32]` if no BK-set change |
| L3 | `new_bk_set_hash` | `[0u8;32]` if no BK-set change |
| L4 | TVM block representation hash | opaque here |
| L5 | `SHA-256(bincode(durable_state_update))` | opaque here |
| L6 | `SHA-256(tx_cnt.to_be_bytes())` | opaque here |
| L7 | Poseidon Merkle root of `[parent, refs…]` | opaque here |
| L8 | `tracked_ext_out_messages_root` | event-binding target for **Circuit 4**, not verified here |
| L9…L15 | `[0u8; 32]` | protocol-fixed zero padding |

Circuit 2 opens **L0 only**: it re-derives `L0 = Poseidon(preimage)` in-circuit, then walks the depth-4 path up to `block_id` using **4 opaque 32-byte siblings** (each a witness):

```
sibling[0] = L1                        (level 0: pair with L0)
sibling[1] = sha_pair(L2, L3)          (level 1)
sibling[2] = subtree(L4..L7)           (level 2)
sibling[3] = subtree(L8..L15)          (level 3)
```

Total in-circuit SHA-256 cost for the opening: **4 compressions**. SHA-256 emits big-endian bytes; the circuit reverses to LE before packing to `Fr` for public instance `[0]`.

Semantics of L1..L15 are *not* checked here. In particular L8 (`tracked_ext_out_messages_root`) is the event-binding leaf verified by Circuit 4 (`bridge-event-prove-circuit`); Circuit 2 only carries `subtree(L8..L15)` as an opaque top-level sibling.

## Circuit Parameters

- **K = 17** (2^17 = 131072 rows)
- **lookup_bits = 16**
- Proof size: **8,064 bytes** (16-leaf depth-4 block-id opening: 4 SHA compressions)
- Proof generation: ~11s @ 10 layers / 10 chain steps (release profile)
- Verification: **~4ms**

## Build & Test

```bash
# Build
cargo build -p historical-layer-hashes-movement-checker-circuit

# MockProver tests (fast, ~3s total)
cargo test -p historical-layer-hashes-movement-checker-circuit --lib -- --nocapture

# Real KZG prover test — sweeps num_layers (1-10) and num_chain_steps (1-10)
# First run: keygen ~7s, then 6 proofs; subsequent runs: loads cached VK/PK
cargo test -p historical-layer-hashes-movement-checker-circuit --test real_prover -- --nocapture

# Same in release mode (faster proof generation)
cargo test -p historical-layer-hashes-movement-checker-circuit --release --test real_prover -- --nocapture

# Single specific MockProver test
cargo test -p historical-layer-hashes-movement-checker-circuit -- test_layer_hashes_circuit_mock --nocapture
```

Cached VK/PK are stored in `test_cache_real_prover/` (gitignored). The VK/PK are specific to the current circuit shape; if a stale cache predates a shape change, **delete `test_cache_real_prover/` once before re-running** so keygen regenerates against the current shape.

## Poseidon Chain Tree Depth

Production configuration — **the only supported configuration** — is `HISTORY_PROOF_WINDOW_SIZE = 128` ⇒ `tree_depth = 8`. Depth is part of the circuit shape (each Poseidon Merkle level emits its own constraints, so VK/PK are depth-specific), so any smaller-`W` fixture would exercise a shape we will never deploy.

### How tree_depth is derived from HISTORY_PROOF_WINDOW_SIZE

```
Each window's Poseidon Merkle tree has:
  [0]            higher_layer_root     <- chain link to layer above
  [1]            same_layer_root       <- chain link within same layer
  [2..W+1]       W block hashes        <- the actual window data (W = WINDOW_SIZE)
  ────────────────────────────────────
  = W + 2 leaves total

Padded to next power of 2 → tree_depth = ceil(log2(W + 2))
```

| HISTORY_PROOF_WINDOW_SIZE | Leaves | Padded to | tree_depth | Siblings per link | Status |
|--------------------------|--------|-----------|-----------|-------------------|--------|
| **128** | 130 | 256 = 2^8 | **8** | 8 | **Production (current and only supported)** |

### Why depth is part of the circuit shape

`verify_chain_of_dense_proofs` calls `dense_merkle_root_circuit` which iterates over `proof.levels` (= `siblings.len()` = tree_depth). Each level generates ~10 constraints (conditional swap, chunk decomposition, range checks, Poseidon hash). With `MAX_CHAIN_LEN = 11` links:

- depth = 8: 11 × 8 = 88 Poseidon levels

Changing depth changes the constraint count → different VK. A padded variant (`dense_merkle_root_circuit_padded`) exists in gosh-dense-balanced-tree for depth-universal VK but is not currently used.

Synthetic test inputs are produced by [`bridge_test_data_gen::layer_hashes::build_synthetic_layer_hashes_input`](../test-data-gen/src/layer_hashes.rs), which hard-codes `TREE_DEPTH = 8` so the test path cannot drift from production.

## Real Prover Test Cases

The `real_prover` test sweeps these configurations:

| num_layers | chain_steps | Description |
|-----------|------------|-------------|
| 1 | 1 | Minimal |
| 3 | 1 | Moderate layers, minimal chain |
| 5 | 3 | Moderate layers, moderate chain |
| 10 | 1 | Max layers, minimal chain |
| 10 | 5 | Max layers, moderate chain |
| 10 | 10 | Max layers, max chain |

## Performance (release profile, Poseidon TREE_DEPTH=8, block-id depth-4 / 16 leaves)

| Metric | Value |
|--------|------|
| num_advice_per_phase | 25 |
| Proof size | 8,064 bytes |
| Prove time (10 layers, 10 steps) | ~11.1s |
| Verify time | ~4.0ms |
| Keygen VK | ~5.4s |
| Keygen PK | ~2.6s |

### Sweep results (block-id depth-4, Poseidon TREE_DEPTH=8)

| num_layers | chain_steps | prove(s) | verify(ms) | proof(B) |
|-----------|------------|---------|-----------|---------|
| 1 | 1 | 11.3 | 3.81 | 8064 |
| 3 | 1 | 10.8 | 3.86 | 8064 |
| 5 | 3 | 10.9 | 4.00 | 8064 |
| 10 | 1 | 11.2 | 3.71 | 8064 |
| 10 | 5 | 11.3 | 4.05 | 8064 |
| 10 | 10 | 11.1 | 4.05 | 8064 |

Key finding: `num_layers` has no effect on proof time. `num_chain_steps` also has essentially no effect at these sizes (all cases ~11s).
