# Light-client fixtures

Real Ethereum beacon `LightClientUpdate` data for M0 spec validation and M1–M3 tests.
Regenerate with [`../scripts/fetch_lc_fixtures.sh`](../scripts/fetch_lc_fixtures.sh).

## Committed

- `mainnet/finality_update.json` — live mainnet `finality_update`, fork **`fulu`**
  (post-Electra), captured 2026-08-20 from `lodestar-mainnet.chainsafe.io`.
  - `attested_header.beacon.slot = 15033585`, `signature_slot = 15033586`
    → period `15033586 // 8192 = 1835`.
  - `finalized_header.execution.block_hash =
    0x8cb2053ab0b719b6b2dc9cd3458acd0d7f6184a6c14f073dee51a9eca36055e1`
    (block_number `25796094`) — the canonical L1 blockHash a deposit would bind to.
  - **Empirically pins the Electra gindex table** (`docs/m0_spec.md §3`):
    `finality_branch` length = **7** = `floor(log2(169))`; each `execution_branch`
    length = **4** = `floor(log2(25))`.
  - `sync_committee_bits` popcount ≈ 509/512 (well above the 342 = 2/3 threshold).

## Gitignored (regenerate on demand — large / mutable)

- `*/update_period_*.json` — full update (~56 KB: 512 pubkeys + `next_sync_committee_branch`).
- `*/bootstrap.json` — WS anchor (`current_sync_committee` + branch).

## Deterministic CI vectors

For fork-tagged, reproducible vectors use `ethereum/consensus-spec-tests`
(`light_client/single_merkle_proof`, `light_client/sync`) — see the tail of
`fetch_lc_fixtures.sh`.

## Opcode VkBlobs (`gosh.zkhalo2VerifyWithVK`)

Emitted on n14 against the **Hermez** ceremony SRS (`s_g2` head `928fafb3…`,
the point the opcode embeds). Each dir carries `*_vk_blob.bin` (Base v1: magic,
version=1, transcript=Blake2b, config JSON, `VerifyingKey(RawBytes)`),
`*_proof_blake2b.bin`, `*_public_inputs.bin` (LE `Fr` ×N), plus a
`*_base_circuit_params.json` and `.sha256` for each blob.

- `step_vkblob/` — deposit-path **step** circuit, Hermez **k=19**, Blake2b.
  Public inputs = **10 × Fr** (`STEP_INSTANCE_LEN`): committee commitment at
  `inst[5]` is the **2-level** Poseidon scheme (`commit_sync_committee_2level`,
  matching rotate's `next_commit`); attested `state_root` at `inst[8|9]`.
  Regenerate: `examples/export_step_vk_blob.rs`.
- `rotate_vkblob/` — recursive **rotate** root (2-to-1 tree over 8 shards +
  step), Hermez **k=21**, Blake2b. Public inputs = **15 × Fr** = 12 KZG
  accumulator limbs + `[current_commit, next_commit, period]`. `current_commit`
  is **snark-bound** to the step proof's committee commitment (`inst[0..12]` are
  the accumulator). The VkBlob header sets `accumulator_limbs = 12` (byte 11).
  Regenerate: `EMIT_VKBLOB=1 examples/rotate_tree_n8.rs`.

  > ⚠ **Opcode-sound only with the decider extension.** The stock
  > `ZKHALO2VERIFYWITHVK` runs a plain SHPLONK `verify_proof` and does **not**
  > pair the 12 accumulator limbs. The proposed extension (see
  > `docs/zkhalo2verifywithvk_decider_extension.md`) reads `accumulator_limbs`
  > from the header and, when 12, additionally checks `e(lhs, g2) == e(rhs, s_g2)`
  > over `instances[0..12]` against the embedded Hermez `[s]·G2` — validated by
  > `examples/rotate_decider_check.rs` (id `opcode-ext`).
