# M3 — light-client "step" circuit + G2 subgroup hardening

**Status: GREEN** (compiles + MockProver on n14 against real mainnet data).

M3 stitches M1 (BLS) and M2 (SSZ) into one circuit that proves a single
sync-committee update, and closes the G2-subgroup audit gap in-circuit.

## Deliverables

### 1. G2 subgroup check (`src/subgroup.rs`) — closes BLS-1 / FORK-2

`assert_g2_in_subgroup` enforces `P ∈ 𝔾₂` via the endomorphism identity
`ψ(P) == [x]P` (Scott, eprint 2021/1130). halo2curves' native `is_torsion_free`
is `psi(P) == mul_by_x(P)` with `mul_by_x(P) = [x]P = -[BLS_X]P`; halo2-ecc's
`mul_by_bls_x` gives `[BLS_X]P`, so in-circuit we check **`ψ(P) == -[BLS_X]P`**.

Cost is a single 64-bit scalar-mult + one `ψ` (2 Fp2 conjugations + 2 muls) —
far cheaper than the naive 255-bit `[r]P == O`.

`load_checked_g2` loads a signature once (on-curve **and** subgroup) and returns
the assigned point, so the *same* cells feed the pairing (sound binding). The M3
step inlines the equivalent so the subgroup-checked point is the pairing input.

MockProver (`tests/subgroup_mock_prover.rs`, n14): valid 𝔾₂ point **accepted**,
on-curve non-subgroup point **rejected** — **k=18, 1.6 s, ~0.5 GB**. The
non-subgroup point is built like halo2curves' own `G2::random` but *without*
`clear_cofactor` (B recovered from the generator, never hardcoded).

### 2. Step circuit (`src/step.rs`)

`verify_step(pool, range, sha, &StepWitness)` constrains, in one circuit:

1. **signing root** (M2): `htr(attested.beacon)` → `domain(fork_version, GVR)` → `signing_root`.
2. **hash-to-curve** (M1): RFC-9380 `signing_root` → G2 (`POP_` DST), in-circuit SHA-256.
3. **aggregate**: `agg = Σ bit_i·pk_i` via shifted-scalar MSM (`bit_i+1`, correct by
   `−all_pub_sum`), each committee pubkey on-curve; boolean bits; supermajority
   `3·Σbit ≥ 1024` (≥ 342/512).
4. **signature**: on-curve **+ subgroup** (same cell) → `e(-G1,sig)·e(agg,H(m)) == 1`.
5. **finality**: `htr(finalized.beacon)` + `finality_branch` @ gindex 169 → `attested.state_root`.

Outputs [`StepPublicInputs`]: `attested_slot`, `finalized_slot`,
`finalized_beacon_root`, `participation`.

MockProver (`tests/step_mock_prover.rs`, n14): **real** attested/finalized headers
+ **real** `finality_branch` from the fulu fixture, with a synthetic-but-valid
512-member committee signing the **real** `signing_root` → **k=22, 110 s, ~17.9 GB**.

### 3. Execution `block_hash` binding (`src/execution.rs`) — M3-exec ✅

`execution_payload_root(sha, ctx, &ExecutionPayloadVals)` computes
`htr(ExecutionPayloadHeader)` (Deneb..Fulu, 17 fields) and returns
`(root, block_hash_node)`:

- Bytes32 fields → node; `fee_recipient` (Bytes20) / `logs_bloom` (Bytes256) → `bytes_root`;
  uint64 fields → `uint64_root`.
- `extra_data` (List[byte,32]) → `mix_in_length(single-chunk, len)` = `SHA256(chunk32 ‖ uint256(len))`.
- `base_fee_per_gas` (uint256) → 32-byte little-endian node.
- `block_hash` (field 12) exposed for PI / deposit binding.
- 17 fields → padded to 32 → `container_root`.

`execution_branch` @ **`EXECUTION_PAYLOAD_GINDEX = 25`** (depth 4) proves this
root against `finalized.beacon.body_root` — which the finality branch already
ties to the finalized (signed) header. So: signed header → body_root →
execution root → **block_hash**, entirely in-circuit.

Off-circuit twin `native_execution_payload_root` cross-checks the gadget and,
critically, is validated against **live data**: exec-payload root + real
`execution_branch` reconstruct `body_root` byte-for-byte for **both** the
finalized *and* attested headers of the fulu fixture — proof the 17-field
layout, `mix_in_length`, uint256 LE encoding and gindex 25 are all correct.

MockProver (`tests/execution_mock_prover.rs`, n14): **k=20, 78 s, ~14.4 GB** —
exec root + branch reconstruct `body_root`, exposed `block_hash` bound to the
fixture value.

### 4. Committee ↔ anchor binding (`src/rotate.rs`) — M3-rotate ✅ (anchor + commitment)

The step trusts its 512 pubkeys as free witnesses. The rotate proof supplies the
missing anchor, following the Telepathy/Succinct split:

- **anchor**: `sync_committee_root = htr(SyncCommittee)` (the expensive
  ~1023-SHA256 SSZ merkleization from `committee.rs`) + `next_sync_committee_branch`
  @ **`NEXT_SYNC_COMMITTEE_GINDEX = 87`** (Electra/Fulu, depth 6) reconstruct a
  trusted beacon `state_root`.
- **commitment**: `commit_sync_committee(pubkeys, aggregate)` — a Poseidon
  commitment (T=3, RATE=2, R_F=8, R_P=57) over the same 48-byte pubkey witnesses
  (each pubkey → 2 field limbs `[0..31]`/`[31..48]`; 1026 elements → ~513 perms).

`verify_rotate` does both and returns the commitment. The rotate proof runs once
per period (~27 h), so its k≈26 cost is amortized; the frequent step proof only
recomputes the cheap Poseidon commitment.

The gindex + the whole SSZ merkleization are validated against **live mainnet
update data**: `htr(next_sync_committee)` + `next_sync_committee_branch` @ 87
reconstruct the attested `state_root` byte-for-byte. The in-circuit Poseidon
commitment matches the native `pse_poseidon` sponge, and is binding (any pubkey /
aggregate byte flip changes it).

MockProver (`tests/rotate_mock_prover.rs`, n14):
- `commit_sync_committee_matches_native` — **k=20, ~3 s** (Poseidon only).
- `full_rotate_in_circuit` (`#[ignore]`) — SSZ root + branch + commitment on live
  data — see the measurements table.

**Fusion closed in step (§6):** the commitment is now recomputed inside `step.rs`
over the same compressed pubkeys the step decode-binds to its aggregated points,
so the standalone-commitment caveat below no longer applies to the step circuit.

### 5. Compressed-G1 decode-bind (`src/decode.rs`) — M4-fusion brick #1 ✅

The soundness link between the rotate commitment (over 48-byte **compressed**
pubkeys) and the step aggregation (over `G1Affine` **points**).
`assert_pubkey_bytes_bind_point(range, ctx, bytes[48], point)` constrains
`bytes == compress(point)` for the ZCash/IETF encoding Ethereum uses:

- byte 0 top 3 bits `C‖I‖S` via `div_mod`+`num_to_bits`: **C=1** and **I=0**
  asserted; **S** = sign bit.
- **x**: rebuilt from the bytes as five little-endian 104-bit limbs (104 bits =
  exactly 13 bytes, so `limb0←BE[35..47]`, `limb1←BE[22..34]`, `limb2←BE[9..21]`,
  `limb3←BE[0..8]`, `limb4=0`) and `constrain_equal`'d to `point.x`'s limbs.
- **sign**: `S == (y > (p−1)/2)` — the **lexicographic** BLS sort flag (not the
  RFC-9380 parity `sgn0`), via a limb-wise magnitude compare against `(p−1)/2`.

Together x + sign pin the full point, so a prover cannot commit to `pk_i`'s bytes
while aggregating `−pk_i` (or any other point).

The **lexicographic convention is validated on live data**: for all 512 real
mainnet pubkeys, `y > (p−1)/2` equals the compressed byte's bit-5 (and C=1, I=0).
MockProver (`tests/decode_mock_prover.rs`, n14, **k=18, ~0.5 s, 0.3 GB**): binds
4 real pubkeys; **rejects** a flipped x byte and a negated `y`.

### 6. Step fusion (`src/step.rs`) — M4-fusion ✅ (one proof, all bricks)

The step circuit now folds bricks §3–§5 into a single proof, so one
`verify_step` call yields the full deposit-path vertical:

- **committee decode-bind ×512**: `verify_step` takes the 512 **compressed**
  pubkeys (`StepWitness.pubkeys_compressed`) and `assert_pubkey_bytes_bind_point`s
  each against the corresponding aggregated `G1Affine` point (§5). The aggregated
  committee is therefore exactly the committed bytes — no free-witness gap.
- **Poseidon commitment**: `commit_sync_committee` is recomputed over those same
  bytes + `aggregate_pubkey` and exposed as `StepPublicInputs.committee_commitment`
  — the value the **rotate** proof anchors to the chain `state_root` (§4). Equality
  of the two commitments (across the two proofs) is the M5 relayer's join.
- **execution block_hash**: `execution_payload_root` + `execution_branch` @ 25 (§3)
  reconstruct the **same** `f_body` cells the finality path already tied to the
  finalized (signed) header, and expose `StepPublicInputs.execution_block_hash`.

The exec branch reuses the Phase-A `f_body` node (not a fresh witness), so the
execution root provably hangs off the *finalized* body_root — closing the last
soundness seam between the signed header and the deposit block hash.

MockProver (`tests/step_mock_prover.rs::step_verifies_on_real_fixture`, n14):
real headers + real finality/execution branches + synthetic-valid 512 committee,
**with the 8 public inputs wired to a real instance column** →
**k=23, ~183 s, ~35.5 GB, ok**.

### 7. Public-input instance wiring (`src/step.rs`) — M4 ✅

`verify_step` packs its outputs into the canonical **8-element** instance column
(`STEP_INSTANCE_LEN`, via `pack_step_instances`) — the public-input shape the
`ZKHALO2VERIFYWITHVK` VkBlob pins:

```text
[0] attested_slot                 [1] finalized_slot
[2] finalized_beacon_root_hi      [3] finalized_beacon_root_lo
[4] participation                 [5] committee_commitment
[6] execution_block_hash_hi       [7] execution_block_hash_lo
```

Each 32-byte SSZ root is split `hi = LE(root[0..16])`, `lo = LE(root[16..32])`
(each < 2^128, so injective in the BN254 scalar field and trivially reassembled
by the AN-side consumer). Slots / participation are single field elements; the
committee commitment is the Poseidon output.

`StepPublicInputs.instances` holds the ordered cells; the caller assigns them to
the circuit's single instance column (`use_instance_columns(1)` +
`assigned_instances[0] = pi.instances`), so MockProver enforces the advice↔instance
copy constraints — the proof's public statement is exactly these 8 values.

Two MockProver tests (`tests/step_mock_prover.rs`):
- `step_instances_layout_and_binding` — **fast (k=12)**: a native twin pins the
  order + hi/lo endianness, and **each** of the 8 positions is shown genuinely
  bound (tampering any single instance value is rejected).
- `step_verifies_on_real_fixture` — the full fused circuit exposes the 8 instances
  on a real instance column and stays satisfiable (k=23, see table).

### 8. Real proof + SRS fit — M4 ✅

The fused step now has a **real SHPLONK proof** (not just MockProver) and
configures at **k = 19** (132 advice cols, lookup_bits=18), so
`ZKHALO2VERIFYWITHVK` consumes it directly (no Groth16 wrapper / aggregation).
The KZG **verifier** needs only `[s]·G2` (size-independent); the **prover** needs
`2^k` powers of `τ`.

`step_circuit_shape` (n14): ~69 M advice cells → **132 advice cols** at k=19
(lookup_bits=18). `step_real_proof_k19` (n14): keygen_vk 176 s, keygen_pk 127 s,
prove 158 s, verify 20 ms, **VK 17 KB / proof 40 KB**, 8 PIs, ~41.5 GB peak.
Full analysis + the width/k trade-off table: `docs/m4_real_proof_and_srs.md`.

> NB: M4 originally read `k=19` as the AN **chain** ceremony ceiling. M5 verified
> the opcode is actually keyed on **Hermez** (K≤28), so there is no ceiling; the
> production VkBlob is keyed on Hermez. See `docs/m5_vkblob.md`.

### 9. Production VkBlob emit — M5 ✅

The step VK is serialized as a real Base-v1 `ZKHALO2VERIFYWITHVK` `VkBlob`
(17 573 B), keyed on the **Hermez** ceremony (tau-preserving downsize of the
on-box Hermez k=20 SRS → k=19; `s_g2` head `92 8f af b3`), then reparsed and
verified through the **exact** opcode Base read path
(`VerifyingKey::read::<_, BaseCircuitBuilder<Fr>>` + `verify_proof` with
`VerifierSHPLONK`/Blake2b against Hermez `verifier_params`) — PASS. Artifacts +
SHA-256 in `fixtures/step_vkblob/`; emitter `examples/export_step_vk_blob.rs`;
full writeup `docs/m5_vkblob.md`.

## Seams carried forward

| Seam | Target | Why deferred |
|------|--------|--------------|
| **production VkBlob emit** | M5 ✅ | DONE — keyed on **Hermez** SRS (the ceremony the opcode embeds since 2026-07-23, K≤28 — NOT the chain ceremony), Base v1 `circuit_shape=0`, self-verified through the opcode read+SHPLONK path. Artifacts `fixtures/step_vkblob/`, see `docs/m5_vkblob.md`. Remaining: tvm-sdk opcode fixture + AN contract embed. Shape (8 PI, k=19, 132 cols) is fixed. |
| **rotate ↔ step join** | M5 | Two proofs share the committee Poseidon commitment (`instances[5]`); the relayer must feed the current period's rotate-anchored commitment as the step's expected value (equality enforced at the instance/aggregation layer). |
| **Weak-subjectivity anchor** | M4/M5 | `genesis_validators_root` + initial committee checkpoint policy (m0_spec §4). |

## n14 measurements

```bash
cd /mnt/data/gosh/sergey-bridge/eth-light-client-prover
cargo test --test subgroup_mock_prover -- --ignored --nocapture   # 2 tests, k18, ~1.6 s
cargo test --test step_mock_prover step_verifies_on_real_fixture -- --ignored --nocapture  # fused, k23, ~231 s
cargo test --test execution_mock_prover -- --ignored --nocapture  # k20, ~78 s
scripts/fetch_lc_fixtures.sh mainnet   # committee fixture for rotate/decode (gitignored)
cargo test --test rotate_mock_prover   # off-circuit anchor + k20 poseidon commitment
cargo test --test decode_mock_prover   # native sign-convention (512 live pubkeys)
cargo test --test decode_mock_prover -- --ignored  # in-circuit decode-bind accept/reject
```

| Test | k | Wall | Peak RSS | Result |
|------|---|------|----------|--------|
| `subgroup_check_accepts_valid_point` | 18 | \~1 s | 0.5 GB | ok |
| `subgroup_check_rejects_non_subgroup_point` | 18 | \~1 s | 0.5 GB | ok (rejected) |
| `step_verifies_on_real_fixture` (fused: BLS + finality + decode-bind×512 + commitment + exec + 8-PI instance column) | 23 | ~183 s | ~35.5 GB | ok |
| `step_instances_layout_and_binding` (layout order + hi/lo + per-position binding) | 12 | ~1.4 s | <0.5 GB | ok (+8 tamper-rejected) |
| `step_circuit_shape` (advice/lookup cells + per-k columns, witness-gen only) | — | ~66 s | 9 GB | ok (132 cols @ k19) |
| `step_real_proof_k19` (real SHPLONK keygen+prove+verify, 8 PI) | 19 | ~503 s | ~41.5 GB | ok (VK 17 KB, proof 40 KB, verify 20 ms) |
| `native_execution_branch_reconstructs_body_root` | — | \~0 s | — | ok (off-circuit) |
| `execution_payload_root_and_branch_in_circuit` | 20 | 78 s | 14.4 GB | ok |
| `native_next_committee_branch_reconstructs_state_root` | — | \~0 s | — | ok (off-circuit, live update) |
| `commit_sync_committee_matches_native` / `_is_binding` | 20 | \~3 s | <1 GB | ok |
| `full_rotate_in_circuit` (full 512 SSZ root) | 26 | — | >125 GB | **memory-bound** on n14 (thrashes; run on larger-RAM host) |
| `native_sign_flag_matches_compressed` (512 live pubkeys) | — | \~0 s | — | ok (off-circuit) |
| `decode_bind_accepts_real_pubkeys` | 18 | 0.5 s | 0.3 GB | ok |
| `decode_bind_rejects_tampered_x` / `_rejects_negated_y` | 18 | 0.5 s | 0.3 GB | ok (rejected) |

> The full-512 committee SSZ root (~1023 SHA-256) is dominated by the assignment
> size, not `2^k` padding, so it exceeds n14's 125 GB at both k25 and k26. This is
> exactly why rotate is a once-per-period proof and the frequent step uses the
> cheap Poseidon commitment. Its constituent primitives are each validated within
> RAM (M2 SSZ tests in-circuit; committee-root+branch natively on live data;
> Poseidon commitment in-circuit).

Off-circuit tests (`cargo test`) all pass.
