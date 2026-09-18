# M1 — BLS core: implementation notes

**Status:** GREEN — compiled + all MockProver tests pass on n14 (2026-08-20) ·
**Depends on:** M0 spec (`m0_spec.md`) · **Code:** `src/bls_core.rs`, `tests/bls_mock_prover.rs`

## 1. Key decision — reuse, don't reimplement

Ethereum sync-committee verification is the **same primitive** the AN→ETH Circuit 1A
already verifies for the Block-Keeper committee. So M1 is a thin Ethereum wrapper over
two already-built, already-used gadgets:

| Gadget | Crate | What we get for free |
|---|---|---|
| committee BLS aggregate | `gosh-bls-verification` | on-curve(G1), MSM(pk·weight) with shift-correction, `(index,count)` signer constraints (distinct, strictly increasing, in-range), `3n ≥ 2N` supermajority, on-curve(G2 sig), pairing `e(-G1,sig)·e(aggPk,H)=1` |
| RFC-9380 hash-to-curve → G2 | `halo2-ecc::bls12_381` | `HashToCurveChip::hash_to_curve::<ExpandMsgXmd>` (SHA-256 XMD, SSWU map, isogeny, cofactor clear) |

`verify_sync_aggregate` in `src/bls_core.rs` is ~40 lines of glue over
`load_bk_set_pubkeys` + `compute_all_pub_sum` + `verify_bls_attestation_with_assigned_msghash`.

## 2. Verified upstream API (gosh `bump-halo2-lib-v0.4.1`)

```rust
// halo2-ecc::bls12_381::bls_signature
BlsSignatureChip::new(&fp_chip, &pairing_chip)
    .is_valid_signature(ctx, sig: G2Point, msghash: G2Point, pubkey: G1Point) -> AssignedValue

// halo2-ecc::bls12_381::pairing
PairingChip::{new, load_private_g1_unchecked, load_private_g2_unchecked, batched_pairing}

// halo2-ecc::ecc::hash_to_curve
HashToCurveChip::new(&hash_chip, &fp2_chip)
    .hash_to_curve::<ExpandMsgXmd>(pool, msg: impl Iterator<QuantumCell>, dst) -> Result<G2Point>

// gosh-bls-verification
load_bk_set_pubkeys(ctx, range, &[G1Affine], limb_bits, num_limbs) -> Vec<G1Point>  // + on-curve
compute_all_pub_sum(ctx, range, &pks, limb_bits, num_limbs) -> G1Point
verify_bls_attestation_with_assigned_msghash(pool, range, sig, msghash, &pks,
    signers: &[(u16,u16)], max_signers, limb_bits, num_limbs, ThresholdMode, all_pub_sum, actual_npk)
```

Field decomposition: `limb_bits = 104`, `num_limbs = 5` (gosh reference tests).

## 3. Ethereum ≠ GoshBLS deltas (the actual M1 work)

| Aspect | GoshBLS | Ethereum (M1) | Where |
|---|---|---|---|
| hash-to-curve DST | `…_SSWU_RO_NUL_` | `…_SSWU_RO_POP_` | `ETH_BLS_DST` |
| signature encoding | 192 B uncompressed | **96 B compressed** G2 | `deserialize_signature_compressed` |
| signer selection | `(index,count)` list | 512-bit `Bitvector` | `bits_to_signers_data`, `decode_sync_committee_bits` (SSZ LSB-first) |
| committee size | dynamic | fixed **512** | `SYNC_COMMITTEE_SIZE` |
| supermajority | `ThresholdMode::Primary` (3n≥2N) | identical ⇒ n≥342 | reused as-is |

`ThresholdMode::Primary`'s `3·n_signers ≥ 2·N` is **exactly** the sync-committee
supermajority (`ceil(2·512/3)=342`), so no new threshold code is needed.

## 4. Soundness gap carried from upstream — G2 subgroup check (BLS-1 / FORK-2)

M0 spec §4 mandates G1 **and** G2 subgroup checks. Current status:

- **G1 pubkeys:** on-curve checked; subgroup membership is transitively trusted once
  M2 binds the 512 pubkeys to the anchored `active_sync_committee_root` via SSZ (the real
  committee's pubkeys are in-subgroup by construction).
- **G2 signature:** `verify_bls_attestation_with_assigned_msghash` checks on-curve **but
  not subgroup**. The signature is attacker-influenceable, so this is a real gap.

M1 mitigation: `assert_witness_subgroup` (off-circuit `is_torsion_free` guard the
relayer/prover MUST run before proving). **Required before M3:** add an in-circuit G2
subgroup check (scalar-mul by group order → identity, or the ψ-endomorphism fast path)
either upstream in `gosh-bls-verification` or as a wrapper here. Tracked as an M1 hardening
item; do not ship a production VkBlob without it.

## 5. Circuit size / `k` (measured on n14, 2026-08-20)

MockProver (BN254, gosh `bump-halo2-lib-v0.4.1`), all green:

| test | signers | `k` / lookup | wall | peak RSS |
|---|--:|--:|--:|--:|
| `hash_to_curve_matches_native` | — | 18 / 17 | ~16 s (with next) | — |
| `small_committee_aggregate` | 4 | 20 / 19 | (both 16.6 s) | — |
| `full_512_synthetic` | 512 | **22 / 21** | **66 s** | **~11.5 GB** |

So the full-width sync-committee aggregate fits at `k=22`. This is the working number
for the M3 VkBlob `k` and the `ZKHALO2VERIFYWITHVK` gas re-bench (real proving, not
MockProver, will be slower — measure at M3). Try `k=21` later to shave the VkBlob.

## 6. What M1 defers

- **Real in-circuit SHA-256** for hash-to-field: M1 uses a `Sha256MockChip` (off-circuit
  digest) to exercise wiring. M2 swaps in `gosh-sha256-chip` so the digest is constrained.
- **`signing_root` derivation**: supplied as a 32-byte witness in M1. M2 computes it via
  SSZ merkleization (ForkData → domain → SigningData) and the finality/exec Merkle branches.
- **PI exposure**: M1 verifies the aggregate; wiring the 13 public inputs (m0_spec §2) is M3.

## 7. Running on n14

```bash
# on n14 (heavy gosh halo2 stack; needs network for git deps)
cd eth-light-client-prover
cargo build                                            # compile-check the graph
cargo test                                             # off-circuit unit tests (fast)
cargo test --test bls_mock_prover -- --ignored --nocapture   # MockProver (minutes, GBs)
```

Excluded from the main Cargo workspace and from `cargo test --workspace` in CI (own
`[workspace]` + `[patch]`, same pattern as `deposit-prover`).
