# M2 — SSZ merkleization, signing root, Merkle branches

**Status: GREEN** (compiles + MockProver on n14 against a real mainnet `finality_update`).

M2 builds the SSZ layer that turns raw beacon fields into the roots M1 needs, all
**in-circuit** over the `gosh-sha256-chip` eDSL SHA-256 (no trusted precompute):

- `hash_tree_root(BeaconBlockHeader)` → the message object root.
- `ForkData` → `domain` → `SigningData` → **`signing_root`** (the exact message the
  sync committee signed; M1's `hash_to_curve` input).
- `verify_merkle_branch(leaf, branch, gindex, root)` — SSZ single-leaf proof for
  `finality_branch` / `execution_branch` / `next_sync_committee_branch`.
- `hash_tree_root(SyncCommittee)` — binds the 512 committee pubkeys + aggregate to
  `active_sync_committee_root`.

## Files

| File | Contents |
|------|----------|
| `src/ssz.rs` | `Node`, `sha256_pair`, `merkleize`, `container_root`, `bytes_root`, `uint64_root`, `verify_merkle_branch`, `merkle_depth` + `native_*` twins |
| `src/signing.rs` | `beacon_header_root`, `fork_data_root`, `compute_domain`, `signing_root`, `sync_committee_signing_root`, fork versions, mainnet GVR + `native_*` twins |
| `src/committee.rs` | `sync_committee_root` (512 pubkeys + agg) + `native_sync_committee_root` |
| `tests/ssz_mock_prover.rs` | off-circuit fixture validation + 4 in-circuit MockProver tests |

## SHA-256 chip API used

`gosh_sha256_chip::Sha256Chip::new(range)` → `chip.digest_bytes(ctx, &input_bytes)`
returns 32 big-endian bytes as `AssignedValue`. Inputs are byte witnesses
(range-checked to 8 bits internally). A node is `Vec<AssignedValue<F>>` of len 32;
`sha256_pair` concatenates two nodes (64 bytes) and digests.

## gindex table validated on real data

The off-circuit test `native_finality_branch_reconstructs_attested_state_root`
recomputes the finalized-header root, applies the fixture's 7-element
`finality_branch` at **`FINALIZED_ROOT_GINDEX = 169`**, and asserts the result
equals `attested_header.beacon.state_root`. It passes on the real fulu fixture —
so both our header `hash_tree_root` and the M0 Electra/Fulu gindex are correct
against live mainnet consensus data. `finality_branch_verifies_in_circuit`
re-proves the same inside the circuit.

Fork/gindex constants live in `m0_spec.md §3`; the fixture is fork `fulu`.

## Key cost finding — the 512-committee root is a rotation-only cost

This eDSL SHA-256 is ~354k advice cells per 64-byte block. Cost per SSZ operation:

| Operation | # SHA-256 | Feasibility |
|-----------|-----------|-------------|
| `beacon_header_root` (5→8 container) | 7 | cheap (k≈19) |
| `signing_root` pipeline (header + fork_data + signing) | ~10 | cheap (k≈19) |
| `finality_branch` verify | 7 | k≈20 |
| `execution_branch` verify | 4 | cheap |
| **`sync_committee_root` (512 pubkeys)** | **~1023** | **~k26 — infeasible per-update** |

**Decision:** the 512-pubkey committee root is computed on a **separate "rotate"
proof** at sync-committee period boundaries (proving `next_sync_committee` against
`next_sync_committee_branch` at gindex 87, then committing the committee root), and
the cheap per-update "step" proof reuses the **anchored** committee root without
recomputing it. This is the standard rotate/step split (cf. Telepathy/Succinct
`SyncCommittee` handling) and is the central architecture decision carried into M3.
M2 therefore MockProver-tests the per-update path on real data and tests the
committee-root building blocks (`bytes_root` on a 48-byte pubkey, small
`merkleize`) in-circuit, while the full 512 assembly is validated natively.

## M2 → M3 seams (not yet in-circuit)

1. **Compressed pubkey ↔ G1 point binding.** `sync_committee_root` takes pubkeys as
   48-byte compressed witnesses. M3 must add a compressed-point decode gadget
   (big-endian x + 3 flag bits, on-curve) constraining these bytes to the exact
   `G1Affine` points M1 aggregates — otherwise the SSZ root and the pairing use
   independent witnesses.
2. **ExecutionPayloadHeader htr.** The `execution_branch` leaf is
   `hash_tree_root(ExecutionPayloadHeader)` (a large container incl. variable-length
   `extra_data`, `transactions_root`, `withdrawals_root`, blob fields). M2 tests the
   branch primitive via the finality branch (real data); the execution-payload
   container htr lands in M3 where the deposit block hash is bound.
3. **Public-input binding.** M3 wires the roots produced here (`signing_root`,
   `attested`/`finalized` slots, `execution.block_hash`, committee root) into the
   frozen PI layout (m0_spec §2).

## n14 measurements (MockProver)

```bash
cd /mnt/data/gosh/sergey-bridge/eth-light-client-prover
cargo test --test ssz_mock_prover -- --ignored --nocapture
```

| Test | k | Result |
|------|---|--------|
| `header_root_matches_native` | 19 | ok |
| `signing_root_matches_native` | 19 | ok |
| `finality_branch_verifies_in_circuit` | 20 | ok |
| `bytes_root_and_merkleize_building_blocks` | 18 | ok |

All 4 pass; wall clock **36.7 s**, peak RSS **~8.6 GB** for the batch.
Off-circuit tests (`cargo test`) all pass and validate the fixture + gindex table.
