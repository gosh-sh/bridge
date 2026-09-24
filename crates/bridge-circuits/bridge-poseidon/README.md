# bridge-poseidon

Single source of truth for native (off-circuit) Poseidon hashing on the
Acki Nacki → Ethereum bridge: byte-encoding helpers, BK-set commitment
(`compute_bk_set_poseidon` / `compute_bk_set_poseidon_padded_to`), and the
in-circuit counterpart (`compute_bk_set_commitment_padded`).

## BK-set commitment: two variants

| Function | Padding | Use case |
|---|---|---|
| `compute_bk_set_poseidon(bk_set)` | hard-coded to `MAX_SIGNERS` (=300) | Production / canonical bridge hash. **Matches acki-nacki's `BlockKeeperSet::poseidon_commitment()`.** |
| `compute_bk_set_poseidon_padded_to(bk_set, max_signers)` | caller-controlled | Tests that exercise alternative padding (e.g. 1000). **Does NOT match acki-nacki for `max_signers != 300`.** |

The default variant is a one-line wrapper over the parameterized one with
`max_signers = MAX_SIGNERS`. Bridge runtime (prover daemon, etc.) should
keep calling the default. Test code within this repo passes `max_signers`
explicitly — most tests use `MAX_SIGNERS`, scaling tests can pass other
values.

All parameters are exported as `pub const` from `src/lib.rs`:

| Constant | Value | Purpose |
|---|---|---|
| `POSEIDON_T` | 3 | Sponge state width |
| `POSEIDON_RATE` | 2 | Sponge rate |
| `POSEIDON_R_F` | 8 | Full rounds |
| `POSEIDON_R_P` | 57 | Partial rounds |
| `LIMB_BITS` | 104 | CRT limb size for BLS12-381 x-coordinate |
| `NUM_LIMBS` | 5 | 5 × 104 = 520 bits ⊇ 381 bits |
| `MAX_SIGNERS` | 300 | Padded BK set size (fixed circuit structure) |
| `PADDING_SIGNER_INDEX` | `0xFFFF` | Sentinel for padding entries |

## ⚠️ Out-of-tree duplicate in acki-nacki — must stay in sync

`compute_bk_set_poseidon` in this crate is **reimplemented** in acki-nacki at:

- `node/src/block_keeper_system/mod.rs` →
  `BlockKeeperSet::poseidon_commitment()` (+ `extract_bls_x_coordinate_be` helper)

acki-nacki cannot depend on `bridge-poseidon` directly (would pull halo2-base /
halo2-axiom / halo2curves into the node binary), so it reimplements the same
spec against its existing `tvm_vm::executor::zk_stuff::bn254::poseidon::PoseidonSponge`.

The two implementations **must produce bit-identical output** for any given BK
set. Anything in the following list that changes here MUST be mirrored in
acki-nacki (and vice versa):

- Any of the constants in the table above
- Sort order of signer indices (currently: ascending by `u16`)
- Padding strategy (currently: sentinel index `0xFFFF` + 5 zero limbs)
- Index encoding (currently: 32-byte LE buffer = `Fr::from(idx as u64)`)
- BLS pubkey x-coordinate extraction (currently: BE 48-byte compressed, top
  3 flag bits cleared, then `BigUint::from_bytes_be` — equivalent to
  halo2curves `G1Affine::from_compressed_be().x.to_bytes()` LE)
- Limb decomposition (currently: 5 × 104-bit LE chunks of the x-coordinate)
- Poseidon sponge initialization (currently: fresh sponge, single `update`
  with the full input vector, single `squeeze`)

### API difference (non-substantive)

- **bridge-poseidon:** builds `Vec<Fr>` (1800 elements for MAX_SIGNERS=300)
  and feeds `pse_poseidon::Poseidon` via `update` / `squeeze`.
- **acki-nacki:** builds `Vec<Vec<u8>>` (each entry a 32-byte LE buffer) and
  calls `PoseidonSponge::new().hash_bytes_axiom(&input)`.

Each 32-byte LE buffer parses to the same Fr on both sides, so the absorbed
field-element sequence is identical.

### Flow into block_id Merkle root (acki-nacki side)

1. `BlockKeeperSet::poseidon_commitment()` →
   `node/src/block/producer/producer_service/block_producer.rs` (`old_bk_set_hash`, `new_hash`)
2. Wrapped into `BlockKeeperSetChangeProofData` in the block's common section.
3. `block_merkle_leaves()` in `node/src/types/ackinacki_block/mod.rs` places
   these as **L2 (old)** and **L3 (new)** of the 16-leaf (depth-4) SHA-256
   Merkle tree canonicalised by the `poseidon_profile_new` branch of
   `acki-nacki`.
4. `merkle_root()` finalizes the block_id.
