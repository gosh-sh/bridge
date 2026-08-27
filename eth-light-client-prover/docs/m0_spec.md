# M0 — frozen spec: PI layout, gindex/fork table, WS-anchor policy

**Status:** frozen v1 · **Created:** 2026-08-20 · Sources: consensus-specs
(Altair/Capella/Electra `light-client/sync-protocol`), verified empirically against a
live mainnet `finality_update` (fork `fulu`, see `../fixtures/`).

This is the contract M1–M5 build against. Changes here are breaking (bump the
version and the VkBlob).

---

## 1. Protocol choice

Altair **sync-committee** light client (512 signers), forward-only from a
weak-subjectivity anchor. One accepted update proves: the *active* committee
(trusted transitively from the anchor) signed a beacon header; that header is
finalized; it carries the given execution `blockHash`; and, on a period boundary,
it attests the *next* committee.

## 2. Frozen public-input layout (v1)

BN254 `Fr`, 32 B little-endian each (matches `ZKHALO2VERIFYWITHVK` PI encoding).
256-bit roots/hashes are split **hi = high 16 bytes, lo = low 16 bytes**, byte-for-byte
identical to the deposit circuit's `blockHashHigh/Low` convention
(`deposit-prover/src/circuit_v2.rs`, `bytes_to_field(&h[0..16])` / `&h[16..32]`), so
`finalizeDeposit` can compare the two directly.

| idx | name | type | contract use |
|:--:|------|------|------|
| 0 | `genesis_validators_root_hi` | 128b | **==** stored anchor (domain binding) |
| 1 | `genesis_validators_root_lo` | 128b | " |
| 2 | `active_sync_committee_root_hi` | 128b | **==** `currentSyncCommitteeRoot` (signer-set trust) |
| 3 | `active_sync_committee_root_lo` | 128b | " |
| 4 | `finalized_slot` | u64 | **>** `finalizedSlot` (monotonic) |
| 5 | `finalized_beacon_root_hi` | 128b | new canonical finalized beacon head (stored) |
| 6 | `finalized_beacon_root_lo` | 128b | " |
| 7 | `finalized_exec_block_hash_hi` | 128b | L1 `blockHash` recorded canonical (deposit binding) |
| 8 | `finalized_exec_block_hash_lo` | 128b | " |
| 9 | `finalized_exec_block_number` | u64 | telemetry / ordering |
| 10 | `next_sync_committee_root_hi` | 128b | rotation output; **0** ⇒ no rotation this update |
| 11 | `next_sync_committee_root_lo` | 128b | " |
| 12 | `participation` | u16 | popcount(bits); policy/telemetry |

**`DEPOSIT_LC_NUM_PUBLIC_INPUTS = 13`.** (Re-bench opcode gas once M3 lands; VkBlob
is VK-driven so the count needs no opcode change.)

Domain binding lives in PI[0..1]; the signer set in PI[2..3]; the two values the AN
contract *acts on* are `finalized_slot` (monotonic gate), `finalized_exec_block_hash_*`
(deposit-binding write), and `next_sync_committee_root_*` (committee rotation).

## 3. Generalized-index / fork table

Branch length = `floor(log2(gindex))`. Gindices are **fork-dependent**; the circuit
selects the set from the fork at `signature_slot` (helper `*_gindex_at_slot`).

| Merkle branch | Altair…Deneb gindex | depth | Electra…Fulu gindex | depth |
|---|:--:|:--:|:--:|:--:|
| `finalized_checkpoint.root` ∈ `BeaconState` | **105** | 6 | **169** | 7 |
| `current_sync_committee` ∈ `BeaconState` | **54** | 5 | **86** | 6 |
| `next_sync_committee` ∈ `BeaconState` | **55** | 5 | **87** | 6 |
| `execution_payload` ∈ `BeaconBlockBody` | **25** | 4 | **25** | 4 |

Empirical check (live mainnet `finality_update`, fork `fulu`): `finality_branch`
length = **7** (⇒ 169) and each `execution_branch` length = **4** (⇒ 25). ✓

**Pre-Electra normalization:** consuming pre-Electra data in an Electra+ store requires
`normalize_merkle_branch` (prepend zero hashes to the deeper depth). M1–M3 target
**Electra/Fulu forward only** (anchor is recent), so normalization is out of scope for
v1; revisit only if we ever ingest pre-Electra updates.

## 4. Signing / domain

- `DOMAIN_SYNC_COMMITTEE = 0x07000000`.
- `fork_data_root = hash_tree_root(ForkData{ current_version = fork_version(signature_slot),
  genesis_validators_root })`.
- `domain = DOMAIN_SYNC_COMMITTEE ‖ fork_data_root[:28]`.
- `signing_root = hash_tree_root(SigningData{ object_root =
  hash_tree_root(attested_header.beacon), domain })`.
- BLS: `e(agg_pk_G1, hash_to_curve_G2(signing_root)) == e(G1_generator, signature_G2)`
  (Eth pubkeys are **G1** (48 B), signatures **G2** (96 B); message hashed to **G2**,
  RFC 9380). **G1 & G2 subgroup checks are mandatory** (closes audit BLS-1 / FORK-2 —
  must not carry the "unchecked" pattern here).

## 5. Constants (mainnet preset)

| Name | Value |
|---|---|
| `SYNC_COMMITTEE_SIZE` | 512 |
| `SLOTS_PER_EPOCH` | 32 |
| `EPOCHS_PER_SYNC_COMMITTEE_PERIOD` | 256 |
| period | 8192 slots ≈ 27.3 h |
| participation threshold (**our policy**) | `ceil(2·512/3) = 342` (spec min is 1; we require supermajority for a finalized head) |
| `period(slot)` | `slot // 8192` |

Fork versions (mainnet): `GENESIS 0x00000000`, `ALTAIR 0x01000000`,
`BELLATRIX 0x02000000`, `CAPELLA 0x03000000`, `DENEB 0x04000000`,
`ELECTRA 0x05000000`, `FULU 0x06000000`. Testnets (Hoodi/Holešky) use their own fork
versions + `genesis_validators_root` — the anchor is **network-specific**.

Mainnet `genesis_validators_root =
0x4b363db94e286120d76eb905340fdd4e54bfe9f06bf33ff6cf5ad27f511bfe95`.

## 6. Circuit-shape note (for M3 VkBlob)

SSZ uses fixed-size SHA-256 merkleization (no RLP/RLC), and BLS12-381 ops are
non-native over BN254 via halo2-ecc. So this circuit likely uses plain
`BaseCircuitBuilder` (VkBlob `circuit_shape = 0`), **unlike** the deposit circuit which
needs axiom-eth's RLC (`circuit_shape = 1`) for RLP. Confirm at M3.

## 7. Weak-subjectivity anchor policy

**Anchor (set once at AN-contract deploy = WS checkpoint):**
`genesis_validators_root`, initial `current_sync_committee_root`, `anchor_slot`
(→ period), and the fork schedule. Sourced from a recent finalized
`LightClientBootstrap` at a checkpoint obtained from ≥ 2 independent
checkpoint-sync providers (and/or our own node), human-verified.

**Liveness / WS invariant.** The committee chain stays trustless only while the client
receives ≥ 1 valid update per sync-committee period (~27 h). If it lapses beyond one
period with no covering update, it **cannot** trustlessly bridge the gap (long-range) →
requires a governed **re-anchor**.

**Re-anchor procedure (rare, governed):** governance sets a fresh
`(current_sync_committee_root, genesis_validators_root, anchor_slot)` from a new
bootstrap, verified against multiple independent sources; documented and logged.
After `disableOwnerRotation()` the contract has **no** `setCommitteeCommitment`
path — that call is the trust-reduction switch, and a re-anchor back-door would
put the owner key back on the committee-advance path. Catch-up via `submitRotate`
is safe for at most one missed period; past the WS bound the recovery is a
**contract redeploy** with a new checkpoint. Do not flip the switch until the
relayer SLA exists (see [`m5_eth_beacon_light_client.md`](m5_eth_beacon_light_client.md)
§Operational constraints).

**Relayer SLA:** submit ≥ 1 update per period **plus** on-demand updates so any pending
deposit's target block is finalized promptly; alert when lag exceeds ~20 h.

## 8. Private witness (informative, finalized at M1–M2)

One beacon `LightClientUpdate` (Electra/Fulu shape): `attested_header`
(beacon + execution + `execution_branch`), `finalized_header` (same),
`finality_branch`, `next_sync_committee` (+ `next_sync_committee_branch`, rotation
updates only), `sync_aggregate{ sync_committee_bits[512], signature }`,
`signature_slot`, plus the 512 active-committee G1 pubkeys (bound to
`active_sync_committee_root` via SSZ) and `aggregate_pubkey`.
