# SHA-256 invocations across `bridge-circuits/`

Where SHA-256 is called in each AN→ETH circuit, how many compressions it costs,
and why that number matters for the outer SHPLONK aggregator budget.

All in-circuit SHA-256 goes through `gosh_sha256_chip::Sha256Chip::digest_bytes`
(measured ~354 000 advice cells per compression block on the gosh eDSL chip).

## 0. Padding math (why some calls cost more than one compression)

SHA-256 processes 64-byte blocks. Padding appends `0x80` + zeros + an 8-byte
big-endian length ⇒ **≥ 9 bytes of overhead**. So a raw input of `L` bytes needs

```
ceil((L + 9) / 64)   compression blocks
```

Cutoffs: `L ≤ 55` → **1** block; `56 ≤ L ≤ 119` → **2**; `120 ≤ L ≤ 183` → **3**;
`184 ≤ L ≤ 247` → **4**; and so on.

The recurring 64-byte case (two concatenated 32-byte hashes) always spills into
**2 compressions**: 64 + 9 = 73 > 64.

## 1. Per-circuit summary

| Circuit | Sub-crate / file | SHA sites | Compressions | Notes |
|---|---|---|---|---|
| **1A** primary attestation | `attestation-bls-checker-circuit/src/primary_circuit.rs` | 1× `hash_to_curve<ExpandMsgXmd>` for BLS12-381 G2 | ~5 | Buried inside BLS; dominated by pairing + G1 MSM |
| **1B** fallback attestation | `attestation-bls-checker-circuit/src/fallback_circuit.rs` | 2× `hash_to_curve` (primary + fallback signatures) | ~10 | Same structure as 1A ×2 |
| **2** historical-layer-hashes | `historical-layer-hashes-movement-checker-circuit/src/circuit.rs` (lines 132–150) | 4× `digest_bytes` — depth-4 block-id opening at position **L0** | **8** | Binds `L0 = Poseidon(layer_hashes_preimage)` → `block_id` |
| **4a** `BridgeEventFinalProof` | `bridge-event-prove-circuit/src/bridge_event_final_proof.rs` (lines 461–464) + `block_id_tree::assert_depth4_l8_opening_circuit` | 4 BOC preimage hashes + 4 depth-4 L8-opening steps | **15** (fixed shape) | See §3 breakdown |
| **4b** `BridgeMultiHopProof` | `bridge-event-prove-circuit/src/multi_hop_proof.rs::prove_hop_block_merkle_sha256` | H × 4 depth-4 openings at position **L7** | **8·H** | The knob — see §4 |
| bridge-poseidon | reference/native only | — | 0 | No in-circuit SHA |
| cross-circuit-block-id-test, test-data-gen | test scaffolding | — | 0 | |

The L7 ref-tree walk inside `BridgeMultiHopProof::prove_hop_ref_tree_opening`
(`MAX_PROOF_BLOCK_REFS_DEPTH = 8`) is **Poseidon**, not SHA — no SHA cost.

## 2. The block-id Merkle tree opening (shared shape, three positions)

The 16-leaf depth-4 SHA-256 tree defined in
`bridge-event-prove-circuit/src/block_id_tree.rs` is opened in three circuits at
three different leaf positions. Every opening runs the same 4 SHA calls on
64-byte inputs ⇒ **8 SHA-256 compressions per opening**.

| Circuit | Leaf position opened | What is bound | Where |
|---|---|---|---|
| **2** | **L0** | `Poseidon(layer_hashes_preimage) → block_id` | `historical-layer-hashes-movement-checker-circuit/src/circuit.rs:114-151` |
| **4a** | **L8** | `Poseidon(ext_out_root) → block_id` | `bridge_event_final_proof.rs:726` calling `assert_depth4_l8_opening_circuit` |
| **4b** | **L7** (per hop, H times) | ref-tree Poseidon root → `block_id` | `multi_hop_proof.rs::prove_hop_block_merkle_sha256`, iterating over `BLOCK_MERKLE_DEPTH = 4` levels |

The shared native + in-circuit gadget lives in
`bridge-event-prove-circuit/src/block_id_tree.rs`; Circuit 2 duplicates the
pattern inline (same 4-level SHA walk, hard-coded sibling directions for
position 0 instead of 8). Circuit 4b's per-hop walk XORs `(7 >> level) & 1`
into `cur_on_right` to pick sibling side for leaf position 7.

Two of the four intermediate nodes are `SHA(0 || 0)` and `SHA(H10_11 || H10_11)`
— pure constants — because chain-side `L9..=L15 = [0u8; 32]` (see
`node/src/types/ackinacki_block/mod.rs:556`). They're hard-coded in
`block_id_tree::{h10_11_const, h12_15_const}`.

## 3. Circuit 4a fixed SHA breakdown

`BridgeEventFinalProof` hashes 4 BOC cell preimages (fixed shape for the
`WithdrawalInitiated` ABI) and then opens the L8 block-id tree. Lengths from
`event_primitives.rs`:

| Cell | `cell_repr_data` len | Padded | Compressions | Const |
|---|---|---|---|---|
| body | 126 B | 135 B | **3** | `BODY_CELL_LEN` |
| recipient | 22 B (`2 + RECIPIENT_LEN_FIXED (=20)`) | 31 B | **1** | `RECIPIENT_CELL_LEN` |
| sender | 36 B (`2 + 34` for 267-bit std_addr) | 45 B | **1** | `SENDER_CELL_LEN` |
| wrapper (ext-out msg root, 1 ref) | not const-asserted; typical ~80-100 B | ~90-110 B | **2** | — |
| **BOC sub-total** | | | **7** | |
| L8 opening (4 × 64 B → 2 comp each) | | | **8** | via `assert_depth4_l8_opening_circuit` |
| **Total** | | | **15** | fixed |

At ~354 k advice cells per compression that is ~5.3 M SHA cells — the bulk of
the circuit's advice bill.

## 4. Circuit 4b — `H_HOPS_PER_PROOF` is the knob

`prove_hop_block_merkle_sha256` runs `BLOCK_MERKLE_DEPTH = 4` SHA calls on
64-byte inputs per **active** hop (padding hops are gated inactive but still
consume the cells). One SHA call = 2 compressions ⇒ **8 compressions per hop**.

| H | SHA compressions | SHA cells | Total circuit cells (approx) | Fit at K=17 | Outer SHPLONK Yul size |
|---|---|---|---|---|---|
| **1 (current)** | 8 | ~2.83 M | ~2.84 M | ~25 advice cols | ~21 KB at `k_outer=21` (fits EIP-170) |
| 2 (H=2 rejected) | 16 | ~5.7 M | ~7 M | ~50 advice cols | **33 213 B at `k_outer=21` (FAIL, 135 % EIP-170)** |
| 3 | 24 | ~8.5 M | ~10 M | ~75 advice cols | tight even at `k_outer=22` |
| 5 (DEX default) | 40 | ~14 M | ~16 M | ~200 advice cols | OOMs at outer keygen on 16 GB + swap |

Outer SHPLONK Yul bytecode scales at ~450–500 B per inner advice column
(measured empirically across the four existing production verifiers — see
`CIRCUIT_COMPLEXITY_COMPARISON.md` §4). The bridge's Solidity SHPLONK
aggregator is capped by EIP-170 at 24 576 B, so `H = 1` was forced when
`H = 2` came out at 33 213 B. At H = 1 the multi-hop inner circuit matches
`historical-layer-hashes-movement-checker-circuit`'s shape and lands where
the other three verifiers already land.

Tradeoff: `N_BUNDLE = ⌈L_MAX / H⌉` multi-hop snarks per claim.
- L_MAX = 20 (prototype): H=5 → 4 bundles; H=2 → 10 bundles; H=1 → 20 bundles.
- L_MAX = 300 (production): H=5 → 60 bundles; H=2 → 150 bundles; H=1 → 300 bundles.

Bundle adjacency (`hopEnd[i] == hopStart[i+1]`) is enforced across snarks by
the outer Solidity `withdrawByProofBundle` (`AckiNackiBridge.sol`), so
dropping intra-snark chaining at H=1 does not weaken the on-chain check.

Verification cost per bundle scales linearly. Choosing H trades one heavy
inner+outer keygen against a larger `n` at withdraw time.

## 5. What is *not* SHA in bridge-circuits

- **All Poseidon** work (block leaves, ext-out leaves, layer combines, DEX-style
  dense-Merkle walks in `BridgeMultiHopProof::prove_hop_ref_tree_opening`) is
  cheap by comparison and not counted here.
- **BLS12-381 pairing + G1 MSM** in the attestation circuits dominates their
  cost profile; SHA there is a rounding error.

## 6. Aggregator relevance

`bridge-evm-aggregator::export-inner-aggregator` (SHPLONK) wraps each inner
snark into a Yul verifier for `AckiNackiBridge.sol`. The outer cost is
governed by the inner VK's column count, not directly by the SHA count. But
in Circuit 4b, SHA calls **are** the reason for the wide column budget: at
H=5 the ~14 M SHA cells forced K=17 with num_advice ≈ 200 to fit inside a
manageable PK size, which in turn made the outer keygen too heavy for a
16 GB host. Reducing H (or bumping inner K to shrink columns) restores
outer parity with Circuits 1A/1B/2/4a.
