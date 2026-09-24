# M6 — recursive rotate: fitting the committee SHA root under k on a 125 GB host

**Problem.** The rotate proof needs `hash_tree_root(SyncCommittee)` — ~1023
in-circuit SHA-256 (512 pubkey leaf hashes + 511 tree nodes + aggregate +
container). Its *assignment* (cell count, independent of `2^k` padding) exceeds a
125 GB host at the k≈26 it needs; n14 (125 GB RAM + 128 GB swap) thrashes. This is
why `tests/rotate_mock_prover.rs::full_rotate_in_circuit` is `#[ignore]` and why
`EthBeaconLightClient.ROTATE_VK_BLOB` ships empty. Even *without* BLS the bare SSZ
root does not fit, so the SHA root itself must be split.

**Approach.** Prove the committee SHA root in **shards** (each a small, comfortably
fitting proof) and combine them in one cheap **aggregation** proof via
snark-verifier.

**Which snark-verifier fork (settled 2026-08-21).** The rotate aggregated proof is
consumed on the AN side by the `ZKHALO2VERIFYWITHVK` opcode — a native **Halo2
SHPLONK** check, *not* an EVM Yul verifier. So the aggregation must run in the
**`gosh-sh/snark-verifier` (rev `164bd42`)** stack that reads a **gosh** halo2-base
shard VK natively. `deposit-prover` already pins exactly that stack, so the
wrap+aggregate lives there as `deposit-prover/examples/aggregate_rotate_shards.rs`
with **no cross-fork bridge**. The earlier `crates/bridge-evm-aggregator` /
`bridge-snark-utils` (axiom `snark-verifier` v0.1.7) path was only ever a mechanics
*probe* — it targets EVM Yul output and needs a gosh→axiom transcript bridge that is
both unnecessary here and blocked by sibling-repo version drift. It is retained for
the k-measurement probe only; the production rotate path is gosh-sh.

**Ceremony = Hermez, no k ceiling.** step/rotate (and the `ZKHALO2VERIFYWITHVK`
opcode since 2026-07-23) are keyed on the **Hermez** Perpetual Powers of Tau ceremony
(`s_g2` head `92 8f af b3`), which publishes powers for any k. The k≤19 cap belongs
to the *chain*-ceremony deposit/DarkDEX path only and does **not** constrain rotate.
Recursion is not about a k ceiling — it is about keeping every piece at small k so its
PK stays in the tens-of-GB range instead of the ~250 GB a monolithic k≈26 rotate PK
would need on n14 (125 GB RAM).

## Why sharding is exact (not an approximation)

`pubkeys_vector_root = merkleize([htr(pk_i) for i in 0..512])` is a **balanced**
binary tree over 512 leaves. A balanced tree's root equals the merkleization of
its top-level subtree roots, for any power-of-two split. So for
`COMMITTEE_SHARDS = N` (a power of two) and `PUBKEYS_PER_SHARD = 512 / N` (also a
power of two):

```
pubkeys_vector_root
  == merkleize([ subtree_root_j for j in 0..N ])
  where subtree_root_j == merkleize([ htr(pk_i) for i in j*512/N .. (j+1)*512/N ])
sync_committee_root
  == container_root([ pubkeys_vector_root, htr(aggregate_pubkey) ])
```

This is proven byte-for-byte — **including on live mainnet committee data** — by
`tests/rotate_shard_mock_prover.rs::shards_reduce_to_full_committee_root`, against
the monolithic `native_sync_committee_root`. The circuit primitives
(`committee_subtree_root`, `sync_committee_root_from_subtrees` in
`src/committee.rs`) and their native twins share the exact `merkleize` /
`bytes_root` / `container_root` used by the monolithic path, so there is no
reimplementation drift.

## Layers

Default `N = 8` (64 pubkeys/shard). Retune `N` to trade shard count vs per-shard k.

| Layer | Count | What it proves | Cost | Public IO |
|-------|-------|----------------|------|-----------|
| **Shard** `committee_subtree_root` | N (=8) | `subtree_root_j = merkleize(htr(pk) for pk in slice_j)` — binds 64 pubkey byte-witnesses to one 32-byte root | ~127 SHA-256; **fits at k≈20** (validated in-circuit, `committee_subtree_root_in_circuit_matches_native`) | out: `subtree_root_j` (hi/lo); + `poseidon_shard_digest_j` (see commitment) |
| **Aggregation** | 1 | snark-verify the N shard proofs; `sync_committee_root_from_subtrees(subtree_roots, aggregate)`; `verify_merkle_branch(committee_root, next_sync_committee_branch, gindex 87, state_root)`; recombine shard Poseidon digests → committee commitment | dominated by N proof-verifications (snark-verifier); ~k21–22 like the R15 aggregator; the SHA top is ~10 hashes (cheap, validated by `recompose_from_subtrees_in_circuit_matches_native` @ k18) | in: N proofs + `aggregate_pubkey` + branch + `state_root`; out: rotate PIs |

## The Poseidon commitment under sharding

The committee commitment (`_currentCommittee` on-chain, recomputed by both step and
rotate) is cheap — a Poseidon sponge, no SHA — but the aggregation circuit does
**not** hold the 512 pubkey bytes (they live in the shards), so it cannot recompute
the single-sponge `commit_sync_committee` over all of them.

**Decision — 2-level Poseidon commitment.** Each shard computes a Poseidon sponge
digest over its 64 pubkeys (its byte-witnesses are already loaded for the SHA
subtree, so the digest is bound to the *same* bytes); the aggregation sponges the N
shard digests + the aggregate-pubkey limbs into the committee commitment. This is a
well-defined commitment that is parallelizable and re-derivable from shard outputs.

**Coordinated change:** the **step** circuit must adopt the *same* 2-level scheme so
its recomputed commitment equals what rotate/aggregation produces (the contract
gates `submitUpdate` on `committee_commitment == _currentCommittee`). Step holds all
512 bytes, so it computes both levels in one (already-fitting) circuit — a small
change to `commit_sync_committee`. Because the commitment *value* changes, step and
rotate VkBlobs must be re-emitted **together**; ship them as one coordinated redeploy
(step VkBlob + `ROTATE_VK_BLOB` + contract). Until then the empty `ROTATE_VK_BLOB`
keeps `submitRotate` reverting and the owner bootstrap path advances the committee.

## R1 vs R2 boundary for the "current committee signed" check

The contract's `submitRotate` uses the **R2** PI layout `[current_commit,
next_commit, period]` (self-contained: the proof shows the current committee signed
the state that contains the next committee). But adding the 512-pairing BLS *into the
aggregation circuit* (on top of snark-verifying N shards) risks blowing the memory
budget again. Two ways to keep R2's contract ABI without an over-large aggregation:

1. **BLS as its own recursion input.** Prove the BLS aggregate (M1 already fits n14
   @ k=22) as a separate proof and snark-verify it in the aggregation alongside the
   N shard proofs. Aggregation then binds: signed `state_root` (from the BLS/finality
   proof) == the `state_root` the branch reconstructs. Clean, keeps each leaf small.
2. **Fold BLS into step, bind by `state_root` (R1 internally, R2 externally).** Step
   already proves the current committee signed a header; if step exposes the attested
   `state_root`, the aggregation only needs shards + branch + commitment, and the
   contract maps R1 outputs onto the R2 ABI. Requires step to expose `state_root`.

Option 1 is preferred (no step-ABI change beyond the commitment scheme, and it keeps
the contract's R2 semantics literally). It makes the recursion a **3-input
aggregation**: N committee-shard proofs + 1 BLS/finality proof, combined once.

## Memory budget — why N=8 must be a TREE, not one circuit (n14 = 125 GB RAM)

**The hard constraint:** aggregating all 8 shards in ONE `AggregationCircuit` needs
`k=23`. With REAL shard inners (many commitments) that outer PK / prover working set is
**~250 GB** — larger than n14's **125 GB RAM** → OOM. This is exactly the monolith wall
recursion is meant to avoid; stuffing 8 inners into one circuit re-creates it.

> **Caveat on the old N=8 @ k=23 "comfortable" probe (2026-08-21).**
> `crates/bridge-evm-aggregator/tests/shard_aggregation_probe.rs` reported 8-way at
> k=23 in ~8 min "well within 125 GB" — but its inners were the aggregator's own
> **trivial multiply** snarks, so the synthesized outer was only **~2–3 advice columns**
> at 2^23. That measures the **mechanics + k-ceiling**, NOT the RAM of a real
> aggregation (a real 2-to-1 node already needs **33 advice** — see below — so 8-in-one
> at k=23 is ~4× the rows × ~11× the columns ⇒ the ~250 GB figure). Do not read that
> probe as "k=23 is affordable on n14."

**The fix — recursion TREE, fixed fan-in 2, every node ≤ k21:**

| Level | Nodes | Aggregates | k (budget) |
|-------|-------|-----------|------------|
| L0 (leaves) | 8 | committee shard SHA proofs | 20 |
| L1 | 4 | 2 shard snarks each (no prev accumulator) | 21 |
| L2 | 2 | 2×L1 each (recursive: fold inner accumulator) | ~21 |
| root | 1 | 2×L2 + **committee glue** + emit `[acc12, current, next, period]` | ~21–22 |

Nodes prove **sequentially**, so peak RAM = one small node, independent of N. Recursion
is supported by the stack: `snark-verifier` `Config::with_accumulator_indices(...)`
marks a wrapped intermediate node as carrying an accumulator, and the SDK's
`expose_previous_instances(has_prev_accumulator = true)` folds that inner accumulator
while passing the inner app-PIs up (skipping the 12 limbs).

### 2-to-1 node cost probe (measured 2026-08-22, n14) — the decisive number

`examples/measure_agg_node.rs` (feature `aggregation`) aggregates **fan-in = 2** copies
of the REAL committee-shard snark (`out/shard_snark`, Hermez k=20) in a stock
`AggregationCircuit` with `VerifierUniversality::None` (bakes the inner VK as constants
— all same-level nodes share one VK; the lever that keeps `k` down) and
`expose_previous_instances(false)`.

Result (`/usr/bin/time -v`, Hermez inner k=20 + outer k=21):

| Metric | Value |
|--------|-------|
| fits at | **k = 21** (`num_advice=33, num_lookup_advice=4, num_fixed=1, lookup_bits=20`) |
| gen_pk | 321.9 s |
| prove + self-verify | 251.8 s (PASS) |
| wall | 10:51 |
| **peak RSS** | **≈ 40.0 GB** (40 036 464 kB) |
| outer proof | 11 680 B, 18 instances (12 accumulator + 6 passthrough shard PIs) |

**≈ 40 GB peak vs ~250 GB for the k=23 monolith** — the 2-to-1 tree comfortably fits
n14, with headroom for the recursive L2 fold + the root's committee glue. Conclusion:
build the tree at fan-in 2; only if a node tips over k21 tighten `VerifierUniversality`
or shrink the passthrough.

### Production proving cost (warm PK vs the published n14 walls)

The published n14 walls **include keygen**. `rotate_tree_n8.rs` now caches one PK
per aggregation *shape* (all four L1 nodes share one PK; both L2 nodes share
another) — the second and later nodes of a level are prove-only. Production
figures:

| Lane | Published wall (cold, includes keygen) | Warm PK (prove only) | Peak RSS |
|------|----------------------------------------|----------------------|----------|
| Step (k=19) | ~8.4 min (`keygen_vk` 176 s + `keygen_pk` 127 s + prove 158 s) | **~2.6 min** (158 s) | ~41.5 GB |
| 8 shards (k=20) | ~35 min (keygen once: vk 209 s + pk 149 s, then 8 proves) | **~25–30 min** after the one-time shard PK | ~tens of GB |
| Rotate tree (k=21) | ~55–57 min historically (per-node `gen_pk`); driver now keygens once per level | **~25–35 min** with L1/L2/root PKs cached | ~44 GB |

Margin vs the 27 h period window: a warm rotate is well under 1 h, so >20×
headroom even if a step is in flight. Keygen of the three rotate PKs is a
one-time (or rare, on VK rotation) cost, not per period.

**One host, two lanes.** Step ~41 GB + rotate ~44 GB ≈ 85 GB — both fit on a
125 GB box concurrently. Anchoring does **not** have to pause for a rotation.
If the operator prefers isolation, a step can wait; a missed checkpoint of the
current committee can be late-registered (see
[`m5_eth_beacon_light_client.md`](m5_eth_beacon_light_client.md) §Operational
constraints). That is not a WS failure as long as ≥ 1 update lands per period.

### Full N=8 tree — end to end (measured 2026-08-22, n14) ✅

`examples/rotate_tree_n8.rs` assembles the **whole** tree over the REAL Hermez-k20
shard snark: `8 shards → 4×L1 → 2×L2 → 1 root`. L1/L2 use the stock
`AggregationCircuit`; the **root** is `RotateAggregationCircuit` (generalised to read
each L2's passthrough as `prev[12..]` chunked into shard triples), folding 2
accumulator-carrying L2 snarks **and** running the committee glue over all 8 subtree
roots. **Every node at Hermez k=21; peak RSS 40 GB across the entire run; wall 20:11.**

| Node | k | num_advice | gen_pk + prove | out inst |
|------|---|-----------|----------------|----------|
| L1 (2 shards, `expose(false)`) | 21 | 33 | 592 s (combined) | 18 = 12 acc + 6 |
| L2 (fold 2×L1, `expose(true)`) | 21 | 16 | 276 s (combined) | 24 = 12 acc + 12 |
| **root** (fold 2×L2 + glue) | 21 | **14** | 114 + 126 s | **15 = 12 acc + 3 rotate PIs** |

The recursion gets **cheaper** going up (33 → 16 → 14 advice) — verifying an inner
aggregation snark costs less than the SHA-heavy shard, so the tree never blows up. The
root's `[current_commit, next_commit, period]` sit after the 12 accumulator limbs, and
its self-verify (`gen_snark_shplonk`) **PASSED**. This is the full N=8 committee rotate
aggregation, on-box, at k=21 — the k=23 monolith path is retired.

Remaining to a shippable rotate proof: ~~(i) distinct per-shard pubkey slices + a **real**
`next_sync_committee_branch`~~ **✅ DONE (see "Faithful committee" below)**; (ii) snark-verify
the BLS/finality proof so `current_commit` is bound (R2 opt-1), (iii) the emit blocker —
a **verifier-side decider** (opcode extension; a BN254 in-circuit decider is impossible)
validated in "Decider" below, plus emitting the outer on **Blake2b + Hermez**.

### Faithful committee — real mainnet data, distinct shards + real branch ✅ (2026-08-23)

The tree is no longer glued over 8 *identical* copies of one synthetic shard. Both the
emitter and the driver now share one committee source ([`src/mainnet.rs`]
`committee_source()`): they prefer the **real mainnet `next_sync_committee`**
(`fixtures/mainnet/update_period_1834.json`, 512 pubkeys + `aggregate_pubkey`) split
into `COMMITTEE_SHARDS` balanced 64-pubkey slices, and fall back to 8 *distinct*
synthetic slices when no fixture is present.

- `examples/export_shard_snark.rs` — keygen ONCE (shape is slice-independent), then
  proves **8 distinct shards** in a loop, writing `shard_{0..7}_{proof,instances}.bin`
  (+ shared `shard_vk.bin`/`shard_config.json`; shard 0 also under the legacy singular
  names). Each shard's exposed `[subtree_root_hi, subtree_root_lo, digest]` is
  native-checked against its real slice.
- `examples/rotate_tree_n8.rs` — loads the 8 distinct leaves, builds the tree **in
  order** (`L1[p]=agg(2p,2p+1)`, `L2[p]=agg(L1[2p],L1[2p+1])`, root folds `2×L2`), so
  the root's `previous_instances` flatten to shards `0..7`. The `RotateGlueWitness` is
  built from the **real** fixture: 8 distinct `subtree_roots`, the real `aggregate`, the
  real 6-node `next_sync_committee_branch`, the real beacon `state_root`, gindex 87, and
  the real `period` (`signature_slot / 8192`). Shape/k/RAM are unchanged from the N=8
  run above (root still `15 inst`), so this is a data-only upgrade.

Preconditions verified **locally on the live fixture** (no n14 needed):
- `shards_reduce_to_full_committee_root` — the 8 real subtree roots recompose
  byte-for-byte to the monolithic `native_sync_committee_root` (sharding is exact on
  real data).
- `native_next_committee_branch_reconstructs_state_root` — monolithic committee root +
  real branch @ gindex 87 == real `state_root`.
- **`next_committee_branch_binds_in_circuit_over_real_shards`** (new, k20) — runs the
  root's *anchor* in-circuit over the 8 distinct real subtree roots:
  `sync_committee_root_from_subtrees` → `verify_merkle_branch` against the real branch +
  real `state_root`. Asserts the roots are distinct (not degenerate) and the branch
  binds. This is exactly the tree root's anchor logic minus the 40 GB snark-verification,
  so it proves the faithful data path is satisfiable without an aggregation node.

**Heavy end-to-end run — MEASURED on n14 (2026-08-23), full real Hermez SRS ✅**

Real Hermez SRS rebuilt on-box from the Polygon zkEVM ceremony ptau
(`powersOfTau28_hez_final_21.ptau` → `convert-from-snarkjs` → k20 `sha256
80394564…` byte-identical to `scripts/bootstrap_hermez_srs.sh`; k21 `sha256
871ae7e7…`). Then the whole pipeline over the **real mainnet committee**:

- `export_shard_snark` — committee source `mainnet update_period_1834.json`; keygen
  once (vk 209 s + pk 149 s), then **8 DISTINCT shard proofs** (Hermez k20, each
  25 920 B, all **distinct sha256**, self-verify PASS). ~35 min.
- `rotate_tree_n8` — `8 distinct shards → 4 L1 (18 inst, ~560 s ea) → 2 L2 (24 inst,
  285 s ea) → 1 root (15 inst, prove 126 s)`, every node **Hermez k=21**. Root witness
  = `mainnet update_period_1834`: 8 distinct subtree roots, gindex 87, real branch
  depth 6, period 1834. `gen_snark_shplonk` self-verify **PASS**. **Peak RSS 44 468 852
  kB ≈ 42 GB**, exit 0, wall ~55 min.
- `rotate_decider_check` on the **faithful** `root.snark` — honest **PASS ✅**,
  tampered limb + swapped lhs/rhs **REJECTED ✅**. Root rotate PIs `[current, next,
  period]` = `[0x…12345678 (R2 witness placeholder), 0x2dc83154…8e4b (the real 2-level
  Poseidon commitment of the mainnet next_sync_committee), 0x72a = 1834]`.

So the full faithful rotate — 8 distinct real committee shards snark-verified,
recomposed to the real committee SSZ root, bound to the live beacon `state_root` via
the real `next_sync_committee_branch` @ gindex 87, committed, and decided — is proven
on-box at k≤21 within 42 GB. Only the R2 `current_commit` binding (BLS/finality) and
the verifier-side decider (opcode extension / Blake2b+Hermez emit) remain.

### Real cross-stack wrap+aggregate (measured 2026-08-21, n14)

The probe above stands in the aggregator's own snarks for shard proofs. The **first
end-to-end run with a REAL gosh-fork shard proof** — `deposit-prover/examples/aggregate_rotate_shards.rs`:

1. Reads the real committee-shard artifacts from `examples/export_shard_snark.rs`
   (`shard_{vk,config,proof,instances}`, Hermez k=20, Poseidon transcript).
2. `compile(vk) → Snark::new` in the **gosh-sh** stack (`snark-verifier` `164bd42`) —
   the gosh VK reads natively, no bridge.
3. Aggregates it in a gosh-sh `AggregationCircuit` at **Hermez k_outer=21**.

Result (N=1): inner SRS Hermez k=20 + outer SRS Hermez k=21 both ceremony-asserted;
`compile`+`Snark::new` OK; aggregation `gen_pk` **158 s** + `gen_snark_shplonk`
**127 s**; **outer proof 5888 B, 12 instances (KZG accumulator)**, self-verify **PASS**.

A passing `gen_snark_shplonk` means snark-verifier's **in-circuit Poseidon verifier
accepted a proof produced by the gosh halo2-base prover** — the transcript bridge is
confirmed end to end (not just the native byte round-trip the unit tests cover). N=1
fits Hermez k=21; the full 8-shard committee needs Hermez **k=23** (per the probe) —
the only remaining input is a Hermez k=23 SRS on box (n14 currently has Hermez k=21;
downsized k=20 sits next to the shard).

### In-circuit committee glue (measured 2026-08-22, n14)

The wrap+aggregate above exposes only the accumulator. `RotateAggregationCircuit`
(`src/rotate_aggregation.rs`, feature `aggregation`) adds the **real committee glue**
on top of `aggregate_snarks`, driven by `examples/aggregate_rotate_glue.rs`:

1. snark-verify the N shard proofs (`aggregate_snarks::<SHPLONK>` → verified inner
   instances as private cells in `previous_instances`);
2. `node_from_hilo` — reconstruct each shard's 32-byte subtree `Node` from its
   exposed `(hi, lo)` limbs: 32 range-checked byte witnesses constrained so
   `LE(bytes[0..16]) == hi`, `LE(bytes[16..32]) == lo` (binds the bytes fed to the
   SHA top to the verified instances);
3. `sync_committee_root_from_subtrees` over the reconstructed roots + aggregate;
4. `verify_merkle_branch(committee_root, branch, gindex 87, state_root)`;
5. `combine_committee_commitment(shard_digests, aggregate)` → `next_commit`;
6. expose rotate PIs `[current_commit, next_commit, period]` after the 12 accumulator
   limbs (`CircuitExt::accumulator_indices() = 0..12`).

Result (N=1, Hermez k_outer=21): `gen_pk` **175 s** + prove **155 s**; **outer proof
6816 B, 15 instances = 12 accumulator + 3 rotate PIs**; `gen_snark_shplonk`
self-verify **PASS**. For N=1 the committee is a single 64-pubkey shard
(`merkleize` of one leaf = the leaf) with a self-consistent synthetic
branch/state_root — a faithful mechanics proof of the whole wiring over a REAL
gosh-fork shard proof. Scaling to the full committee is `--n 8` + a real
`next_sync_committee_branch`, needing a Hermez k=23 SRS.

> **⚠ RESOLVED (2026-08-22) — the opcode does NOT run the accumulator decider.**
> Read straight from the source: `execute_zkhalo2_verify_with_vk`
> (`tvm-sdk/tvm_vm/src/executor/zk_halo2_with_vk.rs`, branch
> `pruvendo/deposit-chainid-12pi-hermez-srs`) is a **plain** Halo2 SHPLONK verify —
> `verify_proof::<KZGCommitmentScheme, VerifierSHPLONK, Challenge255, Blake2bRead,
> SingleStrategy>(…)`. It never reads accumulator limbs and never runs the second
> pairing `e(lhs,[1]G2) == e(rhs,[s]G2)`. So a raw snark-verifier accumulation proof
> would be **accepted but unsound** (the 12 accumulator limbs are treated as ordinary
> public inputs; the in-circuit verifier only *defers* the inner-proof pairing into
> those limbs). Consequences for the rotate emit:
> 1. **A BN254-only "decider-in-circuit" is impossible** — the decider is itself a BN254
>    pairing, uncomputable inside a BN254 circuit, and any extra in-circuit KZG-verify
>    just produces a *new* accumulator. So the decider must be **verifier-side**: extend
>    the opcode to pair the 12 limbs against the already-embedded `[s]G2` (exactly the
>    tail of an EVM Yul verifier). See the reference + spec in "Decider" below.
> 2. the outer proof must use the **Blake2b** transcript (the opcode's, not the Poseidon
>    used for the inner/self-verify) and the **Hermez** ceremony (this branch embeds
>    Hermez `928fafb3…`).
> Tracked as the M6 emit blocker; it is orthogonal to the tree mechanics below.

### Decider — validated reference + opcode-extension spec (2026-08-22, n14) ✅

`examples/rotate_decider_check.rs` runs the accumulation decider **natively over the
REAL tree-root snark** (`out/rotate_tree/root.snark`, 15 instances = 12 accumulator +
3 rotate PIs). It decodes `(lhs, rhs) ∈ G1²` from `instances[0..12]` and checks the
pairing, matching snark-verifier's `pcs/kzg/decider.rs` exactly:

- **layout**: `lhs.x ‖ lhs.y ‖ rhs.x ‖ rhs.y`, each coordinate = `LIMBS=3`
  little-endian limbs of `BITS=88` (`snark-verifier-sdk/src/lib.rs`);
  `coord = Σ limbᵢ·(2⁸⁸)ⁱ` in `Fq`.
- **check**: `e(lhs, g2) == e(rhs, s_g2)` (snark-verifier `terms=[(lhs,g2),(rhs,−s_g2)]`,
  product = 𝟙). `g2` is the fixed BN254 `G2::generator()`, `s_g2` is the ceremony
  `[s]G2` — **already embedded in the opcode as `KZG_S_G2_BYTES`**.

Result: honest proof **PASS**; a tampered limb **REJECTED**; swapped `lhs/rhs` (both
on-curve, wrong pairing) **REJECTED** — i.e. the pairing is the genuine soundness gate.
`decode_accumulator` + `decide` in that example are the drop-in reference.

**Opcode extension (partner-side, ~20 lines in `execute_zkhalo2_verify_with_vk`):**
mark aggregation VkBlobs (a new shape/flag carrying `accumulator_indices = 0..12`);
after the existing `verify_proof(...).is_ok()`, if the blob is an aggregation, also
decode `instances[0..12]` → `(lhs, rhs)` and return `plonk_ok && decide(lhs, rhs,
G2::generator(), s_g2_from_KZG_S_G2_BYTES)`. No new SRS data is needed (halo2curves is
already a tvm-sdk dep). Until then the raw rotate aggregation must **not** be emitted as
a `ROTATE_VK_BLOB`.

## Status

| Piece | State |
|-------|-------|
| Sharding is exact (native + live mainnet) | ✅ `shards_reduce_to_full_committee_root` |
| Shard subtree root in-circuit @ k20 | ✅ `committee_subtree_root_in_circuit_matches_native` |
| Aggregation cheap top in-circuit @ k18 | ✅ `recompose_from_subtrees_in_circuit_matches_native` |
| `committee_subtree_root` / `sync_committee_root_from_subtrees` primitives | ✅ `src/committee.rs` |
| 2-level Poseidon commitment (shard digest + combine) primitives + in-circuit test | ✅ `src/rotate.rs` (`commit_committee_shard` / `combine_committee_commitment` / `commit_sync_committee_2level`); `commit_2level_in_circuit_matches_native` proves step-path == aggregation-path, binding + distinct from v1 single-sponge |
| Shard **inner circuit** (real instance column) | ✅ `src/rotate.rs::verify_shard` → `SHARD_INSTANCE_LEN=3` instances `[subtree_root_hi, subtree_root_lo, shard_digest]`; `shard_circuit_instances_match_native` MockProver @ k20 (this is the snark the aggregation verifies) |
| N-way aggregation **mechanics + k-ceiling** (trivial inners) | ✅ `crates/bridge-evm-aggregator/tests/shard_aggregation_probe.rs` — N=8 at k=23; ⚠ inners are trivial multiply snarks (~2–3 advice) so this measures mechanics/k **only, NOT RAM** — a real 8-in-one is ~250 GB → OOM on n14 (see Memory budget §) |
| **2-to-1 node cost with REAL shard (the tree unit)** | ✅ `examples/measure_agg_node.rs` — fan-in 2, `VerifierUniversality::None`, Hermez k=21: fits at **k=21** (`num_advice=33`), gen_pk 322 s + prove 252 s, **peak RSS ≈ 40 GB**, outer 11 680 B / 18 inst, self-verify PASS. Confirms the recursion tree fits n14. |
| Transcript bridge (gosh-fork shard proof → snark-verifier-loadable) | ✅ `src/poseidon_transcript.rs` (vendored `PoseidonWrite`/`PoseidonRead`, byte-identical to snark-verifier-sdk's `PoseidonTranscript<NativeLoader,_>`); fast validation `tests/poseidon_transcript_prove.rs` (real `create_proof`+`verify_proof`, tamper-rejected @ k8); real shard emit `examples/export_shard_snark.rs` — **n14: Hermez k=20, keygen_vk 203 s + keygen_pk 151 s + Poseidon prove 212 s, 25 920 B proof, self-verify PASS**, writes `shard_{vk,config,proof,instances}` in the exact `export_poseidon_snark` file shape (VK asserted to read back via `VerifyingKey::read::<BaseCircuitBuilder<Fr>>(RawBytesUnchecked, config)`) |
| Step alignment to 2-level (`step.rs` swap `commit_sync_committee` → `_2level`) | ⏳ at the coordinated re-emit (changes the committee value, so ships with the rotate VkBlob) |
| **Wrap + aggregate a REAL shard proof (cross-stack)** | ✅ `deposit-prover/examples/aggregate_rotate_shards.rs` — gosh-sh `compile → Snark::new → AggregationCircuit`; **n14: N=1 @ Hermez k=21, gen_pk 158 s + prove 127 s, outer 5888 B / 12 inst, self-verify PASS** (transcript bridge confirmed end to end). No axiom `bridge-snark-utils` needed. |
| **snark-verifier aggregation circuit + committee glue** | ✅ `src/rotate_aggregation.rs` `RotateAggregationCircuit` (feat `aggregation`) — `aggregate_snarks` + `node_from_hilo` (range-checked) + `sync_committee_root_from_subtrees` + `verify_merkle_branch` (gindex 87) + `combine_committee_commitment`, exposes `[current_commit, next_commit, period]` after 12 accumulator limbs. **n14: N=1 @ Hermez k=21, gen_pk 175 s + prove 155 s, outer 6816 B / 15 inst, self-verify PASS** (`examples/aggregate_rotate_glue.rs`). Generalised to read tree-node passthrough (`prev[12..]` → shard triples). |
| **Full N=8 committee via 2-to-1 recursion TREE** | ✅ `examples/rotate_tree_n8.rs` + `examples/rotate_tree_fold.rs` — `8 shards → 4 L1 → 2 L2 → 1 root`, **every node Hermez k=21, peak RSS 40 GB, wall 20:11**, root folds 2×L2 + glue over 8 subtree roots, self-verify PASS (root 15 inst = 12 acc + 3 rotate PIs). Recursion cheapens upward (33→16→14 advice). The k=23 monolith (~250 GB → OOM) is retired. |
| **Faithful committee — real data (distinct shards + real branch)** | ✅ `src/mainnet.rs::committee_source` (shared by emitter+driver) splits the REAL mainnet `next_sync_committee` (`fixtures/mainnet/update_period_1834.json`) into 8 distinct 64-pubkey slices; `export_shard_snark` keygens once + proves **8 distinct** `shard_{0..7}_*`; `rotate_tree_n8` builds the tree in order + a **real** `RotateGlueWitness` (real branch @ gindex 87, real `state_root`, real period). Data-only (shape/k/RAM unchanged). Anchor validated in-circuit locally: `next_committee_branch_binds_in_circuit_over_real_shards` (k20) + native `shards_reduce_to_full_committee_root` / `native_next_committee_branch_reconstructs_state_root` on the live fixture. |
| Accumulator (KZG) decider consumed by `ZKHALO2VERIFYWITHVK` | ✅ **answered: NO** (opcode = plain SHPLONK verify) **+ reference validated** — `examples/rotate_decider_check.rs` decides the REAL root snark (`e(lhs,g2)==e(rhs,s_g2)` over `instances[0..12]`): honest PASS, tampered/swapped REJECTED. ⏳ needs the **opcode extension** (partner-side, ~20 lines, spec in "Decider" §); a BN254 in-circuit decider is impossible |
| Real proofs + `ROTATE_VK_BLOB` emit (+ coordinated step re-emit) | ⏳ after aggregation builds |
| tvm-sdk `rotate_light_client` opcode fixture | ⏳ after emit |

## Next brick

The aggregation *shell* is de-risked (N-way probe: 8-input SHPLONK at k=23, ~11 min,
RAM comfortable) **and** the transcript bridge is landed — `examples/export_shard_snark.rs`
emits a real Hermez-k=20 shard SHPLONK proof under the vendored Poseidon transcript,
self-verifies it, and writes the `shard_{vk,config,proof,instances}` files in the exact
shape `bridge-snark-utils::export_poseidon_snark` consumes (VK asserted to read back via
the same `VerifyingKey::read::<BaseCircuitBuilder<Fr>>` reader). Remaining to reach a
real rotate aggregation:

1. **Wrap + aggregate (DONE for N=1, mechanical to scale, on n14).**
   ```bash
   # a) emit the shard snark (vary the pubkey slice per shard for real N):
   cd eth-light-client-prover && cargo run --release --example export_shard_snark
   # b) wrap (compile → Snark::new) + aggregate in the gosh-sh stack — no axiom bridge:
   cd ../deposit-prover
   SHARD_DIR=../eth-light-client-prover/out/shard_snark \
     OUTER_SRS=../params/kzg_bn254_21.srs PROBE_N=1 PROBE_K_OUTER=21 \
     cargo run --release --example aggregate_rotate_shards
   # scale to the full committee once a Hermez k=23 SRS is on box:
   #   PROBE_N=8 PROBE_K_OUTER=23 OUTER_SRS=../params/kzg_bn254_23.srs ...
   ```
2. **In-circuit glue on aggregated shard outputs. ✅ DONE (N=1).**
   `RotateAggregationCircuit` runs `node_from_hilo` (range-checked) +
   `sync_committee_root_from_subtrees` + `verify_merkle_branch(gindex 87)` +
   `combine_committee_commitment`, exposing `[current_commit, next_commit, period]`
   after the accumulator (n14 N=1 @ Hermez k=21 PASS; see § above).
   ```bash
   cd eth-light-client-prover
   SHARD_DIR=out/shard_snark OUTER_SRS=../params/kzg_bn254_21.srs PROBE_N=1 \
     cargo run --release --features aggregation --example aggregate_rotate_glue
   ```

Remaining bricks:

3. **Scale to N=8 via the recursion TREE — ✅ DONE** (`examples/rotate_tree_n8.rs`;
   see § "Full N=8 tree"). `8 shards → 4 L1 → 2 L2 → 1 root`, every node Hermez **k=21**,
   peak RSS **40 GB**, root folds 2×L2 + committee glue over 8 subtree roots, self-verify
   PASS. The stock `AggregationCircuit` handles L1/L2 (`expose_previous_instances(false)`
   then `(true)` — `gen_snark_shplonk` stamps `accumulator_indices=0..12` on each
   intermediate snark so the parent folds it); `RotateAggregationCircuit` is the root.
   **Faithful committee — ✅ DONE (2026-08-23, data-only):** distinct per-shard pubkey
   slices from the **real** mainnet `next_sync_committee` + the **real**
   `next_sync_committee_branch` (`src/mainnet.rs::committee_source`, shared by emitter +
   driver; anchor validated in-circuit by `next_committee_branch_binds_in_circuit_over_
   real_shards`).
   **`current_commit` binding (R2 opt-1, `bls-bind`) — ✅ DONE + IRON-CLAD CONFIRMED
   (2026-08-23, n14, real Hermez SRS).** Instead of running BLS inside the rotate
   circuit (would blow k/RAM), the light-client **step** proof is folded as an
   ADDITIONAL inner input to the ROOT (a **3-input** aggregation: 2×L2 + 1 step) and
   the rotate PI `current_commit` is **constrained to the step's verified
   `committee_commitment`** (step public input index 5, `STEP_COMMITTEE_COMMITMENT_
   INDEX`) rather than `load_witness`ed. New pieces:
   - `examples/export_step_snark.rs` — emits the step proof under the **Poseidon**
     transcript (so it is snark-verifiable in-aggregation), keyed on **Hermez**.
     Emitted at **k=21 deliberately**: the root's in-circuit verifier cost to fold a
     snark ≈ its commitment count ≈ `num_advice`, which scales `1/2^k`; at k=19 the
     step has ~132 advice columns, at k=21 only **33** — comparable to an L2 node, so
     the 3-input root still fits k=21. (keygen_vk 182s + keygen_pk 117s + prove 201s.)
   - `RotateAggregationCircuit::new` gained an optional `step_snark`; when present it
     is appended to the folded snarks and `current_commit = prev_instances[step][5]`.
   Measured root (fold 2×L2 + step + glue): **k=21, num_advice=21** (only +5 over the
   step-less root), gen_pk 201s + prove 151s, root = 15 inst (12 acc + 3 rotate PIs),
   full-tree wall **56:57**, peak RSS **~44 GB**. Result:
   `current_commit = 0x0953e092…f56128` == the step's `committee_commitment` (bound,
   not witnessed); `next_commit = 0x2dc83154…` (real 2-level over 8 real subtree
   roots); `period = 0x72a = 1834`. `rotate_decider_check` on this root: honest PASS,
   tampered/swapped REJECTED — the 3-input accumulator is sound.
   **`state_root` binding (R2 soundness, `bls-state-root`) — ✅ DONE + IRON-CLAD
   CONFIRMED (2026-08-23, n14).** The step now also exposes the **attested beacon
   `state_root`** (public-input indices 8/9, `STEP_INSTANCE_LEN` 8→10) — the state
   its BLS signature commits to (it is hashed into the signing root and is the
   finality-branch anchor). When a step snark is folded, the rotate binds its
   `next_sync_committee_branch` anchor to that verified `state_root`
   (`STEP_ATTESTED_STATE_ROOT_{HI,LO}_INDEX`, via `node_from_hilo`) instead of
   witnessing it, so the successor committee is proven to sit in the *signed* state.
   For this to be satisfiable the step and the rotate must describe the SAME update:
   `export_step_snark` now builds its witness from the same `update_period_*.json`
   the rotate driver consumes (`mainnet::find_update_fixture`), so both share
   `attested state_root = 0x7dda4497…340db8a2`. Confirmed: step exposes that root at
   inst[8|9]; the ROOT proved (state_root `constrain_equal` satisfiable) at the same
   **k=21, num_advice=21** (binding is free — reuses `node_from_hilo`), full-tree wall
   57:14, peak RSS ~43 GB; `current_commit` bound to the step's `committee_commitment`,
   `next_commit = 0x2dc83154…`, period 1834; decider honest PASS / tampered REJECTED.
   R2 soundness is now complete on our side: the rotate proves the committee that
   **signed** attested header H (commitment = `current_commit`) attests to
   `H.state_root`, in which `next_sync_committee` (commitment = `next_commit`) sits at
   gindex 87 — nothing witnessed freely.
   **Committee-commitment scheme unified (`step-2level`) — ✅ DONE (2026-08-23).**
   `step.rs` now computes `committee_commitment` via `commit_sync_committee_2level`
   (shard digests over `PUBKEYS_PER_SHARD`-pubkey slices → combine with the aggregate),
   the **identical** value the recursive rotate emits as `next_commit`
   (`combine_committee_commitment` over snark-verified shard digests). Proven in-circuit
   by `tests/rotate_shard_mock_prover.rs::commit_2level_in_circuit_matches_native`
   (step path == aggregation path == native) and shown distinct from the retired v1
   single-sponge by `commit_2level_is_binding_and_distinct_from_single`. So the
   on-chain `_currentCommittee` a rotate advances into (`next_commit`) matches what the
   next step's `submitUpdate` recomputes (`committee_commitment`). This changes the
   step VK → step + rotate VkBlobs must ship together at `emit-real` (which naturally
   re-exercises the 2-level step on n14).
4. **Decider — ANSWERED + reference validated** (opcode does a plain verify only; a
   BN254 in-circuit decider is impossible — see ⚠ + "Decider" §). ✅
   `examples/rotate_decider_check.rs` decides the real root snark (honest PASS,
   tampered/swapped REJECTED). ⏳ Remaining: the **partner opcode extension** (~20 lines,
   pair `instances[0..12]` against the embedded `[s]G2`), and emit the outer on the
   **Blake2b** transcript + **Hermez** ceremony.
5. **Emit** real proofs + `ROTATE_VK_BLOB` — ✅ **DONE (2026-08-23, n14, Hermez SRS).**
   `EMIT_VKBLOB=1 examples/rotate_tree_n8.rs` re-proves the root under **Blake2b**
   (the outer transcript is independent of the inner Poseidon snark verification) and
   writes the Base v1 blob wire. One full run produced both artifact sets:
   - `fixtures/rotate_vkblob/` — root at Hermez **k=21**, Blake2b. `rotate_vk_blob.bin`
     (3232 B) reads back as `BaseCircuitBuilder<Fr>` and the **exact opcode path**
     (`VerifyingKey::read(RawBytes)` → `verify_proof<…,VerifierSHPLONK,…,SingleStrategy>`
     over `Blake2bRead`) **ACCEPTS** the proof (7392 B). PI (15 × Fr): 12 accumulator
     limbs + `current_commit = 0x1256912d…` (**bound** to the step's 2-level
     `committee_commitment`), `next_commit = 0x2dc83154…`, `period = 0x72a` (1834).
   - `fixtures/step_vkblob/` — 2-level step re-emitted at Hermez **k=19**, Blake2b;
     PI now **10 × Fr** (was 8) with the 2-level commitment at `inst[5]`.
   Full run: build + step snark (k=21) + 2-to-1 tree + Blake2b re-prove (149.6 s) +
   step VkBlob, **wall 1:00:24 for the tree stage, peak RSS 44.5 GB** (`/usr/bin/time -v`).
   ⚠ opcode acceptance is **necessary but not sufficient** — the plain SHPLONK verify
   does not pair `instances[0..12]`; sound emission still awaits the `opcode-ext` decider.
   **`accumulator_limbs = 12` is now a hard gate**, not a comment: emit
   (`examples/rotate_tree_n8.rs`) asserts byte 11 == 12 before writing;
   `scripts/check_rotate_vkblob_accumulator.sh` checks the committed fixture, its
   sha256 sidecar, the patch-embedded hex, and a byte-11-cleared negative probe;
   `embed_rotate_vk_blob.py` and `sync_rotate_opcode_fixtures_to_tvm_sdk.sh`
   refuse to ship a 0. CI job `test:light-client:vkblob-header` runs that script.
   **tvm-sdk fixture — ✅ DONE.** `scripts/sync_rotate_opcode_fixtures_to_tvm_sdk.sh`
   installs the three operands into `tvm-sdk/tvm_vm/halo2_test_data/rotate_light_client`;
   `tvm_vm/src/tests/test_halo2_with_vk.rs` gained 5 `rotate_light_client_*` tests
   (round-trip ACCEPTS; flipped proof byte, tweaked accumulator limb, tweaked rotate PI
   all REJECTED; cache reuse) — all green alongside the re-synced 2-level step (10 PI).
   ⏳ Remaining: only the partner `opcode-ext` decider before the blob is emission-sound.

> **Hermez k≥23 note.** The tree keeps every node at k≤21 (Hermez k=21 is on n14), so a
> Hermez k=23 SRS is **no longer needed** for N=8 — the monolithic k=23 path is retired.
> (n14's k=22/k=23 SRS are the CHAIN ceremony `c6028acf…`, not Hermez, and unusable with
> the Hermez shard inners anyway.)
