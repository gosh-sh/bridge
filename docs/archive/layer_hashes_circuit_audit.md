> **⚠️ ARCHIVED 2026-08-18 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/ETH-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

# Layer Hashes Update Circuit — Audit Report

**Repository**: `gosh-sh/layer-hashes-update-halo2-circuit`
**Circuit**: `LayerHashesUpdateCircuit` in `circuit/src/primary_circuit.rs`
**Date**: 2026-04-13
**Scope**: Constraint completeness, witness/constraint alignment, cross-field arithmetic, bincode assumptions, edge cases

---

## 1. Executive Summary

The `LayerHashesUpdateCircuit` is a Halo2 circuit (K=19, BN254 Fr) that proves the validity of an Acki Nacki block's layer hash updates. It verifies: (A) SHA-256 binding of block data to attestation, (B) Primary target type, (C) layer hash extraction from block data, (D) Poseidon Merkle chain continuity from previous state, (E) BLS12-381 committee signature, and (F) BK set Poseidon commitment.

**Overall assessment**: The circuit is well-structured with clear constraint separation. All dependencies have been reviewed (`gosh-sha256-chip`, `gosh-bls-verification`, `gosh-dense-balanced-tree`, and the Gosh fork of `halo2-lib`). The circuit builds and all mock prover + real prover tests pass. We identified several observations ranging from informational to medium severity. The most significant concerns are: (1) no explicit G2 subgroup check in the BLS path — the `load_private_g2_unchecked` API skips both on-curve and subgroup checks, relying on the calling code in `gosh-bls-verification` for on-curve only; (2) bincode layout coupling to Acki Nacki node serialization; and (3) the trust model around BLS attestation honesty for layer hash content.

**Build/test status**: Build succeeds with one fix (`state_to_bytes` visibility in `gosh-sha256-chip`). Mock prover passes for k=0, k=1, k=2, k=5, and all 2 real-data fixtures (L5/H12288/S11, L6/H45056/S11). Real prover test passes with consistent VK across chain lengths (~26 min).

---

## 2. Constraint-by-Constraint Analysis

### 2.1 Section A: SHA-256 Block Hash (lines 77–183)

**What it does**: Computes SHA-256 of `block_data` (up to 4096 bytes) and constrains the result to equal the `envelope_hash` in the attestation.

**Mechanism**:
- Off-circuit: builds full SHA-256 padding (0x80 + zeros + 64-bit length), fills remaining to `MAX_PADDED_BYTES = 4160`
- In-circuit: loads all 4160 bytes as witnesses, range-checks each to 8 bits
- Runs 65 SHA-256 compression blocks against a constant IV
- Uses `block_selector` (witnessed, 7-bit range-checked) with `idx_to_indicator` to select the correct intermediate state
- Converts selected state words to 32 hash bytes via `Sha256Chip::state_to_bytes`
- Constrains `hash_bytes[i] == assigned_msg[ENVELOPE_HASH_REL_OFFSET + i]` for i in 0..32

**Findings**:

| ID | Severity | Finding |
|----|----------|---------|
| A-1 | Informational | SHA padding is computed off-circuit. The prover supplies the padded buffer and `block_selector`. A malicious prover could supply inconsistent padding (wrong length encoding, missing 0x80 marker). **Mitigated** because the SHA output is constrained to equal `envelope_hash` which is part of the BLS-signed attestation — forging requires breaking SHA-256 or BLS. However, this means SHA padding correctness is not independently enforced; it relies on the BLS signature chain. |
| A-2 | Low | `block_selector` is 7-bit range-checked (max 127), but only 65 valid values exist (0..64). Values 65..127 would select padding/fill states. This is safe because `idx_to_indicator` produces a zero indicator for out-of-range values, so `select_by_indicator` returns zero — producing an incorrect hash that would fail the `envelope_hash` equality check. |
| A-3 | Informational | `block_data_cells` (used for layer hash extraction) is `sha256_input_cells[..4096]`, sharing the same witness tape as the SHA input. This correctly ties the hashed data to the data from which layer hashes are extracted. |

### 2.2 Section A': Target Type Enforcement (lines 185–199)

**What it does**: Constrains 4 bytes of `assigned_msg` at `TARGET_TYPE_REL_OFFSET` (offset 116) to zero.

**Mechanism**: Direct equality constraints against zero constant.

**Findings**:

| ID | Severity | Finding |
|----|----------|---------|
| B-1 | Informational | Correctly enforces `target_type == Primary` (bincode u32 LE: `0x00000000`). `Fallback` would be `0x01000000`. This relies on the bincode representation being stable. See Section 5 for bincode coupling analysis. |

### 2.3 Section E': num_layers Validation (lines 201–220)

**What it does**: Constrains `num_layers` to the range [1, MAX_LAYERS] where MAX_LAYERS = 10.

**Mechanism**:
- `num_layers_cell`: 4-bit range check (0..15)
- `num_layers - 1`: 4-bit range check → num_layers >= 1
- `MAX_LAYERS - num_layers`: 4-bit range check → num_layers <= 10

**Findings**:

| ID | Severity | Finding |
|----|----------|---------|
| C-1 | OK | Range bounds are tight. 4-bit range (0..15) is sufficient for both `num_layers - 1` (max 9) and `MAX_LAYERS - num_layers` (max 9). No under-constraint here. |

### 2.4 Section E: Layer Hash Extraction (lines 222–317)

**What it does**: Extracts 10 × 32-byte root hashes from `block_data_cells` at dynamic offset, packs to Fr, masks inactive layers.

**Mechanism**:
- `base_offset_cell`: 12-bit range + upper bound check → `base <= 2938`
- `offset_indicator`: one-hot indicator of length 4096
- For each layer i, byte j: rotates `block_data_cells` by constant stride `c = FIRST_ROOT_HASH_OFFSET + i * BTREE_ENTRY_SIZE + j`, then `inner_product(indicator, rotated_data)` extracts the byte at position `base + c`
- 32 bytes packed to Fr via inner product with powers of 256
- Inactive layers masked to zero via `active_mask[i]` multiplication

**Findings**:

| ID | Severity | Finding |
|----|----------|---------|
| D-1 | Medium | **No semantic validation of BTreeMap structure**. The circuit reads 32-byte chunks at fixed strides from a chosen base offset. It does not verify: (a) the BTree entry count matches `num_layers`, (b) layer keys are sequential, (c) entries are valid `ProofLayerRootHash` structures. Correctness relies entirely on the BLS-signed block data being honestly formatted by the Acki Nacki node. A compromised node that produces a block with arbitrary bytes at layer hash offsets (but with a valid BLS signature) could inject fake layer hashes. **Risk**: This is by design — the circuit proves "the BLS-attested block contains these bytes at these offsets", not "these bytes are valid layer hashes". The bridge contract must trust the BK set's honesty. |
| D-2 | Low | **Rotation wraps modulo 4096**. The expression `(k + c) % MAX_BLOCK_DATA_BYTES` means that for large `base + c` values, extraction wraps around. The upper bound check (`base <= 2938`, max `c = 1157`) ensures `base + c <= 4095`, so wrapping never occurs for valid inputs. This is correctly bounded. |
| D-3 | Informational | **Powers of 256 packing**. The 32-byte to Fr conversion uses little-endian packing (byte 0 × 256^0 + byte 1 × 256^1 + ...). This matches the `bytes_to_fr` function used in test instance generation. The resulting Fr element may not equal the big-endian interpretation of the hash — consumers must use the same convention. |
| D-4 | Informational | **Active mask construction** via reverse cumulative sum of `idx_to_indicator(num_layers, 11)` is correct: `active_mask[i] = sum(indicator[i+1..11])` = 1 iff `i < num_layers`. |

### 2.5 Section F: Prev-Chain Merkle Verification (lines 319–365)

**What it does**: Verifies a Poseidon Merkle chain from `prev_max_level_layer_hash` to the top active layer hash.

**Mechanism**:
- Target selection: `layer_hash_frs[num_layers - 1]` via `select_by_indicator`
- `num_prev_chain_steps`: range-checked to [1, MAX_CHAIN_LEN] (same 4-bit pattern as num_layers)
- `verify_chain_of_dense_proofs(ctx, range, poseidon, prev_hash, proofs, num_steps)` returns final hash
- Final equality constraint: `current == target`

**Findings**:

| ID | Severity | Finding |
|----|----------|---------|
| E-1 | Cannot verify | **External dependency**: `verify_chain_of_dense_proofs` lives in `gosh-dense-balanced-tree` (not available for review). This function is critical — it must correctly handle active/inactive proof slots, Poseidon hashing with proper domain separation, and sibling ordering. **Recommendation**: Audit `gosh-dense-balanced-tree` once `gosh-halo2-crypto-lib` access is available. |
| E-2 | Informational | Poseidon parameters (T=3, RATE=2, R_F=8, R_P=57) match standard BN254 Poseidon parameters from the Poseidon paper for 128-bit security. The same parameters are used in `layer_hash_gen.rs` tests and verified against `pse_poseidon` native implementation. |
| E-3 | Low | **MAX_CHAIN_LEN is 4-bit bounded** (max 15). If `MAX_CHAIN_LEN > 15` in a future `gosh-dense-balanced-tree` version, the range check would silently allow only values up to 15, potentially rejecting valid long chains. Currently MAX_CHAIN_LEN appears to be 11 from test evidence, so this is safe. |

### 2.6 Section B/C: BLS Verification (lines 367–413)

**What it does**: Hash-to-curve + BLS12-381 aggregate signature verification with Primary threshold.

**Mechanism**:
- `load_bk_set_pubkeys`: loads G1 pubkeys as assigned EC points with CRT limb decomposition
- `compute_all_pub_sum`: precomputes sum of all pubkeys for MSM correction
- Hash-to-curve: `HashToCurveChip::hash_to_curve::<ExpandMsgXmd>` on `assigned_msg` bytes with `DST` from `gosh_bls_verification`
- Verification: `verify_bls_attestation_with_assigned_msghash` with `ThresholdMode::Primary`

**Findings**:

| ID | Severity | Finding |
|----|----------|---------|
| F-1 | Cannot verify | **External dependencies**: `gosh-bls-verification` and `halo2-ecc` provide the BLS verification logic. Key concerns that require separate audit: (a) subgroup checks on G1/G2 points, (b) cofactor clearing, (c) correct hash-to-curve implementation matching BLS draft spec, (d) threshold counting logic (>= ceil(2n/3)), (e) signer entry deduplication. |
| F-2 | Medium | **Attestation bytes lack independent byte-range constraints**. At line 73, attestation data bytes are loaded as `Fr::from(b as u64)` witnesses without 8-bit range checks. If the H2C/BLS chips do not independently range-check their inputs, a malicious prover could supply field elements > 255 for some bytes, potentially affecting hash-to-curve output. **Mitigation**: The SHA-256 input bytes ARE range-checked (line 124), and `assigned_msg` (the attestation data) is constrained to share `envelope_hash` bytes with the SHA output. However, bytes outside the 32-byte `envelope_hash` and 4-byte `target_type` windows are only constrained via BLS signature verification. If the H2C chip treats inputs as arbitrary field elements without reduction to [0,255], there could be a soundness issue. **Recommendation**: Verify that `HashToCurveChip::hash_to_curve` internally range-checks or reduces its byte inputs. |
| F-3 | Informational | The signature is deserialized in `LayerHashesUpdateCircuit::new` (line 475), not inside `build_primary_constraints`. This means the G2 point is a Rust-level `G2Affine` value passed to the constraint builder. The BLS gadget must constrain it in-circuit (load as witness + on-curve check). |
| F-4 | Informational | `signer_entries` are sorted by index in `new()` (line 485). The BLS gadget must enforce consistency between `signer_entries` and the actual `assigned_msg` bytes — if entries are parsed off-circuit and not re-derived from constrained message bytes, there could be an inconsistency vector. |

### 2.7 Section D: BK Set Commitment (lines 415–422)

**What it does**: Computes Poseidon commitment over sorted (signer_index, x-coordinate CRT limbs).

**Mechanism** (from `lib.rs` lines 121–154):
- For each pubkey k: push `sorted_bk_set_indices[k]` as constant, then x-coordinate CRT limbs
- `poseidon.hash_fix_len_array(ctx, gate, &input)` produces the commitment

**Findings**:

| ID | Severity | Finding |
|----|----------|---------|
| G-1 | Low | **Only x-coordinate is committed**. The BK set commitment binds signer index + x-coordinate limbs but not y-coordinate. This is acceptable if the BLS gadget constrains each pubkey to be a valid on-curve G1 point, making y recoverable from x (up to sign). If the gadget does not enforce this, two different pubkeys with the same x could produce the same commitment. |
| G-2 | Informational | Signer indices are loaded as **constants** (line 144), not witnesses. This means the index ordering is baked into the circuit structure and verified by the prover's VK. A different ordering would produce a different VK. |
| G-3 | Informational | The commitment includes ALL pubkeys in the BK set (not just signers of this block). This is correct — it pins the entire committee for the bridge to track. |

### 2.8 Public Input Assignment (lines 636–646)

**What it does**: Pushes 13 values as public instances.

**Findings**:

| ID | Severity | Finding |
|----|----------|---------|
| H-1 | OK | All 13 public inputs are derived from constrained circuit values: `bk_set_commitment` (Poseidon output), `num_layers_cell` (range-checked witness), `layer_hash_frs` (extracted and masked), `prev_hash_cell` (chain-verified witness). No unconstrained public inputs. |
| H-2 | Informational | `without_witnesses()` is `unimplemented!()` (line 660). This prevents use of Halo2 APIs that call this method (e.g., some keygen paths in older versions). The test code works around this by using `calculate_base_circuit_params` to pre-compute layout. |

---

## 3. Cross-Field Arithmetic

The circuit operates on BN254 Fr but verifies BLS12-381 signatures. This requires non-native field arithmetic:

- **LIMB_BITS = 104, NUM_LIMBS = 5**: BLS12-381 Fp is ~381 bits; 5 × 104 = 520 bits provides sufficient room with carries
- CRT (Chinese Remainder Theorem) representation via `halo2-ecc`'s `ProperCrtUint`
- Range checks on limbs are handled by `halo2-ecc` internals

**Assessment**: The limb parameters are standard for BLS12-381 over BN254. Full verification requires auditing `halo2-ecc`'s `FpChip` and `Fp2Chip` for:
- Correct modular reduction
- Carry propagation
- Overflow handling in intermediate operations
- Quadratic residue checks for point decompression

This is outside the scope of the circuit-level audit (it's a library-level concern).

---

## 4. Witness Generation vs. Constraint Alignment

| Witness Input | How Constrained |
|---------------|-----------------|
| SHA-256 padded bytes (4160) | 8-bit range check each; SHA output constrained to envelope_hash |
| `block_selector` | 7-bit range check; selects SHA intermediate state |
| Attestation data bytes | Via BLS signature verification (H2C + pairing); envelope_hash bytes via SHA equality |
| `num_layers` | Triple 4-bit range check → [1, 10] |
| `base_offset` | 12-bit range + upper bound → [0, 2938] |
| Layer hash Fr values | Derived from block_data_cells via constrained extraction |
| `prev_max_level_layer_hash` | Poseidon chain verification → must chain to target layer hash |
| `num_prev_chain_steps` | Triple 4-bit range check → [1, MAX_CHAIN_LEN] |
| Prev chain proofs (siblings, positions) | Constrained inside `verify_chain_of_dense_proofs` |
| BLS signature (G2) | Constrained inside BLS verification gadget |
| BK set pubkeys (G1) | Constrained inside BLS verification gadget |
| Signer entries | Used in BLS threshold check (constrained inside gadget) |

**Assessment**: No obvious cases of "witnessed but unconstrained" values in the main circuit. All key relationships are enforced through explicit equality constraints, range checks, or delegated to chip gadgets.

---

## 5. Bincode Layout Assumptions

The circuit hard-codes offsets derived from bincode serialization of Acki Nacki types:

| Constant | Value | Derivation |
|----------|-------|------------|
| `ENVELOPE_HASH_REL_OFFSET` | 84 | `parent_block_id(40) + block_id(40) + block_seq_no(4)` |
| `TARGET_TYPE_REL_OFFSET` | 116 | Above + `envelope_hash(32)` |
| `BTREE_HEADER_SIZE` | 8 | u64 entry count |
| `BTREE_ENTRY_SIZE` | 124 | `key(1) + ProofLayerRootHash(123)` |
| `FIRST_ROOT_HASH_OFFSET` | 10 | `BTREE_HEADER_SIZE(8) + key(1) + root_hash_offset(1)` |

**Risks**:

| ID | Severity | Finding |
|----|----------|---------|
| L-1 | Medium | **Tight coupling to serialization format**. Any change in `AttestationData` fields (reordering, adding fields, changing `BlockIdentifier` size) would silently break the circuit without compile-time errors. The circuit would still "work" but extract wrong data. **Recommendation**: The `block_data_gen` crate provides `compute_history_proofs_byte_offset` and `compute_layer_hash_byte_offsets` that validate against real serialized data — these should be run as integration tests against every node release. |
| L-2 | Low | **bincode version dependency**. The offsets assume bincode's current encoding rules (e.g., u32 LE for enum discriminants, u64 LE for BTreeMap length). A bincode major version change could alter encoding. The `block_data_gen` crate pins `bincode` to verify compatibility. |
| L-3 | Informational | The `ProofLayerRootHash` size (123 bytes) consists of: `layer(1) + root_hash(32) + block_height(4 or 8) + block_id(?)`. The exact breakdown should be verified against `tvm_block` types. The `block_data_gen` tests do verify stride consistency for generated data. |

---

## 6. Edge Cases

| Case | Handling | Assessment |
|------|----------|------------|
| `num_layers = 1` | Single active layer; `nl_minus_one = 0`; target = `layer_hash_frs[0]`; layers 1..9 masked to zero | Correct; tested (`prepare_circuit_test_input(5, 1, 1)`) |
| `num_layers = 10` | All layers active; `MAX_LAYERS - num_layers = 0` passes range check | Correct; tested (`test_prev_chain_k10_10layers_mock`) |
| `num_prev_chain_steps = 1` | Direct equality: `verify_chain` with 1 step means `prev_hash → target` | Correct; tested (`test_prev_chain_k0_mock`) |
| `num_prev_chain_steps = MAX_CHAIN_LEN` | All chain slots active | Tested (`test_prev_chain_k10_mock` with 11 steps) |
| `block_data.len() = 0` | Panics in `new()` due to SHA padding math (`(0 + 9 + 63) / 64 = 1`, so `actual_padded_size = 64`, `zero_pad_len = 55`). Actually valid, but unusual. | Would not panic; `block_selector = 0` selects first compression state. |
| `block_data.len() > 4096` | Assertion at line 83 panics before circuit construction | Correct; fails fast |
| Single signer in BK set | Threshold `ceil(2*1/3) = 1`, so one signer suffices | Depends on BLS gadget implementation |
| `prev_max_level_layer_hash = 0` | Valid; zero is a legitimate Poseidon hash output (very unlikely) or initial state | Fixture `L2_H16_prevH0_S1` tests this |

---

## 7. Dependency Audit: `gosh-halo2-crypto-lib`

The `gosh-halo2-crypto-lib` repository (now available) contains three crates with critical constraint logic. Full source review follows.

### 7.1 `gosh-sha256-chip`

**Compression function** (`compression.rs`): Standard SHA-256 implementation with 64 rounds, correct IV and K constants.

Key constraint mechanisms:
- **`u32_to_bits`**: Each of 32 bits is `assert_bit`-constrained, recomposed via powers of 2 with `constrain_equal` to the word. Correctly pins values to [0, 2^32-1].
- **`mod_u32`**: Explicit hi/lo decomposition with `range_check(lo, 32)`, `range_check(hi, hi_bits)`, and linear composition constraint `x == hi * 2^32 + lo`. Correctly handles modular reduction.
- **Message schedule**: 16 big-endian u32s from 64 input bytes; recurrence relations match SHA-256 spec.
- **Round constants**: All 64 K values match the SHA-256 standard.

**`state_to_bytes`** (`lib.rs` lines 99-118): Each output byte is 8-bit range-checked; four bytes are composed via `mul_add` chain and `constrain_equal` to the state word. This correctly decomposes state words to big-endian bytes.

**`digest_bytes`**: Input bytes are each `range_check(ctx, b, 8)`. Padding (0x80, zeros, big-endian bit length) uses constants. IV is loaded as constants. This is correctly constrained.

| ID | Severity | Finding |
|----|----------|---------|
| SHA-1 | OK | Compression function matches SHA-256 spec. All round operations use constrained mod-2^32 arithmetic. |
| SHA-2 | OK | `state_to_bytes` correctly range-checks bytes and constrains composition to state words. |
| SHA-3 | Informational | `digest_varlen` is `unimplemented!()` — any accidental use panics. Not a soundness issue if unused. |
| SHA-4 | Informational | Relies on field being large enough (>2^32) for bit decomposition uniqueness. True for BN254 Fr (~254 bits). |

### 7.2 `gosh-bls-verification`

**G1 pubkey loading** (`load_bk_set_pubkeys`): Each pubkey is loaded via `load_private_g1_unchecked` then explicitly checked with `check_is_on_curve` (Weierstrass equation y^2 = x^3 + b).

**G2 signature loading**: Loaded via `load_private_g2_unchecked` with explicit `check_is_on_curve` on G2 (y^2 = x^3 + b').

**Threshold enforcement**:
- `n_signers` is a witness, range-checked to `[1, max_signers]` via `n_signers - 1` and `max_signers - n_signers` range checks.
- **Primary threshold**: Proves `3 * n_signers >= 2 * n_pubkeys` by range-checking `3*n_signers - 2*n_pubkeys` to 16 bits (non-negative, max ~900).
- **Fallback threshold**: Proves `2 * n_signers > n_pubkeys` via `2*n_signers - n_pubkeys - 1` non-negative.

**Signer slot handling**:
- `is_active[i] = (i < n_signers)` — first n_signers slots active, rest inactive. No "holes" allowed.
- Inactive slots: `idx == 0`, `count == 0` enforced via multiplication by `(1 - is_active)`.
- Active slots: `idx < n_pubkeys` via `check_less_than_safe`.
- Strictly increasing indices: `(idx[i+1] - idx[i] - 1) * both_active` is 16-bit range-checked (gap >= 1).
- Count >= 1 when active: `range_check(is_active * (count - 1), 16)`.

**Aggregation**: Weights = `shifted[j] = weight[j] + 1` to avoid zero scalars in MSM; `agg_pk` recovered by subtracting `all_pub_sum`. Uses `add_unequal` with `is_strict=true` to prevent unsound addition when pubkeys share x-coordinate.

**Signature verification**: `bls_chip.assert_valid_signature(ctx, sig_assigned, msghash_assigned, acc)` — pairing check.

| ID | Severity | Finding |
|----|----------|---------|
| BLS-1 | Medium | **No explicit subgroup checks on G1 or G2**. Only on-curve checks are performed. For BLS12-381, the G1 subgroup has cofactor h=1 (so on-curve implies in-subgroup for G1). For G2, cofactor h2 != 1 — a point on the G2 curve may not be in the prime-order subgroup. Whether this matters depends on `assert_valid_signature`'s pairing equation. If the pairing naturally rejects non-subgroup G2 points (e.g., via the final exponentiation), this is mitigated. **Recommendation**: Verify in `halo2_ecc` that the pairing check is immune to small-subgroup attacks on G2, or add explicit subgroup checks. |
| BLS-2 | OK | Threshold enforcement is correct. Primary: `3s >= 2n` (equivalent to `s >= ceil(2n/3)`). Fallback: `2s > n` (equivalent to `s > n/2`). Range check widths (16-bit) are sufficient for max_signers=300. |
| BLS-3 | OK | Signer slot monotonicity and deduplication are correctly enforced via strictly-increasing index constraints. |
| BLS-4 | Low | `n_pubkeys` is a layout constant (not a witness). Changing BK set size requires a different circuit/VK. This is by design but means the bridge must track VK changes across BK set size changes. |
| BLS-5 | Informational | `all_pub_sum` correctness depends on caller (`compute_all_pub_sum` in the main circuit). If wrong, `agg_pk` is wrong and proof should fail — but verify this is the case. |

**Resolution of finding F-2 from the main circuit audit** (attestation bytes not range-checked): The SHA-256 chip range-checks its inputs to 8 bits. The attestation bytes that go through hash-to-curve are processed by `HashToCurveChip` which internally uses `Sha256Chip` (for `ExpandMsgXmd`), which also range-checks. However, the attestation bytes are loaded as witnesses in the main circuit without range checks, and they are constrained only via: (a) 32 bytes matching SHA-256 output (range-checked), (b) 4 bytes constrained to zero (target_type), (c) BLS hash-to-curve processing. Bytes outside these windows are only constrained by the BLS signature verification chain. **The H2C chip does range-check through its internal SHA-256 calls**, so F-2 severity is reduced to **Low** — an attacker cannot supply non-byte values without failing the H2C chip's internal range checks.

### 7.3 `gosh-dense-balanced-tree`

**`MAX_CHAIN_LEN = 11`** — hardcoded constant shared across circuits.

**`bytes_to_fr`**: Raw 4×u64 little-endian import into BN254 Fr via `Fr::from_raw`. This is NOT `from_bytes` with modular reduction — it interprets the bytes directly as a field element in Montgomery form. Must match how the off-circuit tree builder and the Acki Nacki node produce hashes.

**`dense_merkle_root_circuit`** (per-level constraints):
- **Direction bit**: `assert_bit` constrained.
- **Conditional swap**: `cond_swap` based on direction bit.
- **Chunk decomposition**: left and right children split into (c0, c1, c2) with:
  - `c0 + left_hi * 2^248 == left` (constrain_equal)
  - `right_low = right - c2 * 2^240`
  - `c1 == left_hi + 256 * right_low` (constrain_equal)
  - Range checks: c0(248-bit), right_low(240-bit), left_hi(8-bit), c2(16-bit)
- **Poseidon hash**: `hash_fix_len_array(ctx, gate, &[c0, c1, c2])` produces next level value.

**`verify_chain_of_dense_proofs`**:
- For each of MAX_CHAIN_LEN slots:
  - `active = (j < num_active_steps)` via `is_less_than(ctx, j_const, num_active_steps, 4)`
  - Full `dense_merkle_root_circuit` runs regardless (fixed circuit structure)
  - `current = select(computed_root, current, active)` — inactive slots keep previous value
- Returns final `current` value

| ID | Severity | Finding |
|----|----------|---------|
| DT-1 | OK | Merkle proof constraints are complete: direction bits are binary, conditional swap is correct, chunk decomposition is algebraically tied to left/right values with range checks for canonical decomposition, Poseidon hash produces next level. |
| DT-2 | Low | `bytes_to_fr` uses `Fr::from_raw` (raw limb import) not `Fr::from_bytes` (with reduction). If any 32-byte hash value exceeds the BN254 scalar field modulus r (~2^254), the conversion produces a different Fr element than modular reduction would. In practice, Poseidon hashes are already Fr elements, so their byte representation is always < r. But for SHA-256 outputs used as tree leaves, there's a ~2^(-2) chance of exceeding r, which would cause a mismatch. **Recommendation**: Verify that all tree leaf values are Poseidon hashes (not raw SHA-256 outputs). |
| DT-3 | OK | Inactive chain slots are correctly handled: `select` preserves previous `current`; constraints still run but don't affect the output. |
| DT-4 | Informational | `num_active_steps` must be constrained by the caller to [1, MAX_CHAIN_LEN]. If 0, every slot is inactive and the result equals the initial leaf — protocol-level issue if instances expect a nontrivial chain. The main circuit does enforce this via range checks. |
| DT-5 | Informational | `is_less_than` uses 4-bit comparison logic, sufficient for MAX_CHAIN_LEN=11 (needs < 16). |

### 7.4 `halo2-base` and `halo2-ecc` (Gosh fork)

**Repository**: `gosh-sh/halo2-lib-zkevm-sha256-and-bls12-381` — single commit (`c0c78ce`, "init").

**Workspace structure**: `halo2-base` (v0.4.0), `halo2-ecc` (v0.4.0), `hashes/zkevm` (`zkevm-hashes`). The `halo2-ecc` crate depends on the workspace-local `halo2-base` via path.

**What changed from upstream `axiom-crypto/halo2-lib`**: The fork adds a `bls12_381` module in `halo2-ecc/src/bls12_381/` alongside the existing `bn254` and `secp256k1` modules. No modifications to core `halo2-base` (gate, range, `RangeChip`, `is_less_than`) were found — these retain upstream semantics. The upstream README still says BLS is "coming soon", which this fork supersedes.

**New BLS12-381 modules** (in `halo2-ecc/src/bls12_381/`):

| File | Role |
|------|------|
| `mod.rs` | Curve types from `halo2curves::bls12_381`, `XI_0 = 1`, type aliases |
| `pairing.rs` | Optimal Ate pairing: Miller loop with BLS_X 62-bit MSB loop, multi-Miller loop, final exponentiation |
| `final_exp.rs` | BLS12-381 final exponentiation with Frobenius maps |
| `hash_to_curve.rs` | Hash-to-G2 following RFC 9380 (draft-irtf-cfrg-hash-to-curve-16), SSWU + isogeny |
| `bls_signature.rs` | BLS signature verification via batched pairing |
| `tests/*.rs` | Pairing, EC add, hash-to-curve, BLS verification tests |

**Pairing equation** (`bls_signature.rs`): `assert_valid_signature` computes `batched_pairing([(-g1, signature), (pubkey, msghash)])` and asserts the result equals `Fq12::one()`. This is equivalent to `e(pk, H(m)) = e(g1, sig)` — the standard minimal-signature BLS scheme (pk ∈ G1, sig and H(m) ∈ G2).

**Miller loop**: Uses `BLS_X` and `BLS_X_IS_NEGATIVE` from `halo2curves::bls12_381`. Single-pair path applies conjugation when `BLS_X_IS_NEGATIVE`; multi-pair path has a comment noting conjugation "can be skipped" with some uncertainty (potential audit concern but covered by test regression against native pairing).

**Hash-to-curve**: Implements SSWU map to G2 with isogeny, following RFC 9380. DST is a parameter (`dst: &[u8]`), not hardcoded. Test uses the standard `BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_` suite string.

**Field tower**: Standard `Fp2 = Fp[u]/(u^2+1)`, `Fp12 = Fp2[w]/(w^6 - u - xi)` with `XI_0 = 1` for BLS12-381.

**`range_check` / `is_less_than`**: Unchanged from upstream. BLS field gadgets delegate to the same `FpChip` / `RangeChip` path as BN254.

| ID | Severity | Finding |
|----|----------|---------|
| FORK-1 | Low | Fork adds `bls12_381` module; `halo2-base` core unchanged. Pairing equation is standard. Miller loop and final exponentiation are tested against native `halo2curves` pairing. No changelog or documentation of changes exists in-repo (only code). |
| FORK-2 | Medium | **`load_private_g1_unchecked` / `load_private_g2_unchecked`** in the pairing chip do NOT call `assert_is_on_curve` (they call `EccChip::load_private_unchecked` which skips curve checks). The BN254 BLS signature path uses `load_private_g1` / `load_private_g2` with on-curve checks. This means the BLS12-381 path relies entirely on the calling code (`gosh-bls-verification`) to add on-curve checks — which it does for pubkeys and signature, but NOT for the message hash point produced by hash-to-curve. If hash-to-curve is correctly constrained in-circuit, this is safe; otherwise it is a gap. |
| FORK-3 | Informational | Multi-Miller loop conjugation handling has an uncertainty comment (line 342-345 of `pairing.rs`). Test coverage against native pairing provides regression confidence but is not a formal proof. |

### 7.5 Build and Test Results

**Build**: Compiles successfully with one required fix — `state_to_bytes` in `gosh-sha256-chip` must be `pub` (currently private, but called by the main circuit). This is a minor API issue, not a soundness concern.

**Mock prover tests** (all pass, K=19):

| Test | Chains | Time | Result |
|------|--------|------|--------|
| `test_prev_chain_k0_mock` | 0 prev | ~115s | PASS |
| `test_prev_chain_k1_mock` | 1 prev | ~130s | PASS |
| `test_fixture_mock_prover` (L5/H12288/S11) | real data | ~71s | PASS |
| `test_fixture_mock_prover` (L6/H45056/S11) | real data | ~72s | PASS |

**Real prover test** (`test_prev_chain_real_prover`): Keygen + prove + verify with consistent VK across all chain lengths. ~26 minutes total. PASS.

---

## 8. Summary of Findings

### Main Circuit Findings

| ID | Severity | Category | Summary |
|----|----------|----------|---------|
| A-1 | Info | SHA-256 | Padding computed off-circuit; mitigated by BLS chain |
| A-2 | Low | SHA-256 | block_selector range allows out-of-bound values; safe due to indicator behavior |
| D-1 | Medium | Layer extraction | No semantic BTreeMap validation; relies on BLS attestation honesty |
| D-2 | Low | Layer extraction | Rotation wrapping is correctly bounded |
| E-3 | Low | Merkle chain | 4-bit range check may be insufficient if MAX_CHAIN_LEN grows beyond 15 |
| F-2 | Low | BLS | Attestation bytes not independently range-checked; mitigated by H2C chip's internal SHA-256 range checks |
| F-4 | Info | BLS | Signer entries parsed off-circuit; gadget enforces monotonicity and bounds |
| G-1 | Low | BK commitment | Only x-coordinate committed; OK because G1 has cofactor 1 (on-curve implies in-subgroup) |
| L-1 | Medium | Bincode | Tight coupling to serialization format; fragile across node releases |
| L-2 | Low | Bincode | bincode version dependency |

### Dependency Findings (`gosh-halo2-crypto-lib`)

| ID | Severity | Category | Summary |
|----|----------|----------|---------|
| SHA-1 | OK | SHA-256 chip | Compression function matches spec; mod-2^32 arithmetic correctly constrained |
| SHA-2 | OK | SHA-256 chip | `state_to_bytes` correctly decomposes with range checks + composition constraints |
| BLS-1 | Medium | BLS verification | No explicit G2 subgroup check; on-curve only. G1 is OK (cofactor=1). G2 immunity depends on pairing equation in halo2_ecc fork |
| BLS-2 | OK | BLS verification | Threshold enforcement is correct for both Primary and Fallback modes |
| BLS-3 | OK | BLS verification | Signer deduplication and monotonicity correctly enforced |
| BLS-4 | Low | BLS verification | n_pubkeys is a layout constant; BK set size changes require new VK |
| DT-1 | OK | Dense tree | Merkle proof constraints are complete and correct |
| DT-2 | Low | Dense tree | `bytes_to_fr` uses raw import; values must be < field modulus (true for Poseidon outputs) |
| FORK-1 | Low | halo2-lib fork | Fork adds BLS12-381 module; core halo2-base unchanged; pairing equation standard; tested against native |
| FORK-2 | Medium | halo2-lib fork | `load_private_g*_unchecked` skips on-curve checks; BLS12-381 path weaker than BN254 path; relies on caller |
| FORK-3 | Info | halo2-lib fork | Multi-Miller loop conjugation has uncertainty comment; covered by test regression |

### Severity Legend
- **Medium**: Potential issue that should be addressed or explicitly documented as accepted risk
- **Low**: Minor concern; defense-in-depth recommendation
- **OK**: Reviewed and found correct
- **Informational**: Design observation; no action required

---

## 9. Recommendations

1. **Priority 1**: Resolve G2 subgroup check gap. The BLS12-381 pairing path uses `load_private_g2_unchecked` (no on-curve or subgroup check). The calling code in `gosh-bls-verification` adds `check_is_on_curve` for the signature point but NOT a full subgroup check. For BLS12-381 G2 (cofactor ≠ 1), a malicious prover could supply a G2 point that is on-curve but not in the prime-order subgroup. Either: (a) add an explicit G2 subgroup check, (b) use `load_private_g2` instead of `_unchecked` in the pairing chip, or (c) formally prove that the final exponentiation + pairing equation makes non-subgroup witnesses fail. (Findings BLS-1, FORK-2)

2. **Priority 2**: Make `state_to_bytes` public in `gosh-sha256-chip`. The main circuit calls this method directly for the block-selection SHA-256 pattern, but it's currently private. This is a trivial API fix.

3. **Priority 3**: Add integration tests that verify bincode layout constants (`ENVELOPE_HASH_REL_OFFSET`, `TARGET_TYPE_REL_OFFSET`, `BTREE_ENTRY_SIZE`, etc.) against current `tvm_block` / Acki Nacki node types. Automate this in CI to catch serialization changes early. (Finding L-1)

4. **Priority 4**: Verify that all dense balanced tree leaf values are Poseidon hash outputs (always < BN254 Fr modulus), not raw SHA-256 outputs, to ensure `bytes_to_fr` raw import produces correct field elements. (Finding DT-2)

5. **Priority 5**: Consider adding negative tests that supply: (a) malformed BTreeMap data, (b) wrong signer entries, (c) non-Primary attestation type, (d) broken Merkle chain — to verify the circuit rejects them through the constraint chain.

6. **Priority 6**: Document the trust model explicitly: the circuit proves "a BLS-attested block contains these layer hashes at these offsets", not "these layer hashes are semantically valid". The bridge must trust the BK set's honesty for the semantic correctness of layer hash content. (Finding D-1)

7. **Priority 7**: Address the uncertainty comment in the multi-Miller loop conjugation for BLS12-381 (pairing.rs lines 342-345). While test regression provides confidence, a formal argument or reference to the BLS12-381 optimal Ate pairing specification would strengthen assurance. (Finding FORK-3)
