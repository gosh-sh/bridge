# attestation-bls-checker-circuit

ZK circuit that verifies a BLS attestation produced by an Acki-Nacki node and
extracts the attested `block_id` / `block_seq_no` as public instances. Two
variants are provided:

- **Primary** (`primary_circuit.rs`) — single attestation, `target_type == Primary`,
  threshold `≥ ⌈2n/3⌉`.
- **Fallback** (`fallback_circuit.rs`) — two attestations on the same `block_id`
  with `target_type == {Primary, Fallback}`, threshold `> n/2` each.

Both circuits parse `attestation_bytes`, a `bincode`-serialized
`Envelope<AttestationData>` taken straight from the chain
(`acki-nacki/node/src/bls/envelope.rs`).

## What the primary circuit constrains

### Inputs

| Kind | Value |
|---|---|
| Private witness | `attestation_bytes` (full bincode envelope), `bk_set: HashMap<u16, Vec<u8>>` (compressed G1 pubkeys keyed by protocol signer index), `last_seen_block_seqno: u32` |
| Off-circuit derivations | `signature: G2Affine` (BLS aggregate), `signer_entries: Vec<(pos, count)>` (protocol indices remapped to padded-array positions), `bk_set_pubkeys: Vec<G1Affine>` padded to `max_signers` with `G1::generator()` and `sorted_bk_set_indices` padded with `PADDING_SIGNER_INDEX = 0xFFFF` |

### Public instances (column 0, in emission order)

| Idx | Value | Source |
|---|---|---|
| 0 | `block_id_fr` | 32 bytes at attestation-data offset 48, packed LE into a single `Fr` |
| 1 | `bk_set_commitment` | Poseidon (BN254, T=3 RATE=2) over the padded BK set's CRT limbs |
| 2 | `block_seq_no_fr` | u32 LE at attestation-data offset 80 |
| 3 | `last_seen_block_seqno_fr` | caller-supplied baseline |

### Constraint steps (`build_primary_constraints`)

1. **Load attestation data.** The 120-byte `AttestationData` slice is assigned as byte witnesses (one cell per byte).
2. **Block-id extraction (A).** Bytes `[48..80]` are folded into `block_id_fr` via `inner_product` against `256^i` constants — that single field element becomes public instance [0], so an EVM verifier sees the block hash directly.
3. **Target-type pin (A′).** Each of the four bytes at offset 116 is `constrain_equal`'d to 0, i.e. `target_type == Primary (0x00000000)`. Wrong discriminant ⇒ proof fails.
4. **Seq-no inequality (A″).** `constraint_block_seqno_gt_last_seen` extracts `block_seq_no` from offset 80, requires `block_seq_no - last_seen_block_seqno - 1 ∈ [0, 2^32)` (range-checked through the lookup table). Public instances [2] and [3] commit both values so an L1 verifier can chain monotonicity across proofs.
5. **BK set load (B).** Each padded G1 pubkey is decomposed into CRT limbs (5 × 104 bits per coordinate) and loaded as assigned cells (`load_bk_set_pubkeys`).
6. **`all_pub_sum` (B′).** A point-wise sum of every assigned pubkey is computed *in-circuit*. It's used by the BLS verifier as the "all-ones" reference for the post-MSM signer-mask correction.
7. **BK set commitment (C).** `compute_bk_set_poseidon_padded_to` Poseidon-hashes the padded `(sorted_index, limbs)` stream; the real-vs-padding boundary is detected from `PADDING_SIGNER_INDEX` and yields `n_real_pubkeys`, the threshold denominator. This becomes public instance [1] — i.e. the proof binds the attesting committee identity, not just the count.
8. **Hash-to-curve (D).** `HashToCurveChip` runs the BLS12-381 `ExpandMsgXmd` construction with SHA-256 (`gosh_sha256_chip`) and the canonical `DST` over the same 120-byte `AttestationData` witnesses, producing the assigned G2 msghash. **The BLS signature thus attests the inner `AttestationData` payload, not the envelope** — the outer bincode framing is parser bookkeeping.
9. **BLS verification (E).** `verify_bls_attestation_with_assigned_msghash` runs the BLS12-381 pairing check, applying `ThresholdMode::Primary` (≥ ⌈2·`n_real_pubkeys`/3⌉) against the assigned signer mask, msghash, signature, `all_pub_sum`, and the assigned pubkey array. This is the only constraint that observes the aggregate signature; everything earlier was setup.

### Cost profile

The pairing + scalar-mul / MSM work in step 9 dominates: BLS12-381 arithmetic over CRT limbs is by far the largest cell consumer, followed by the SHA-256-based hash-to-curve in step 8. BK-set loading and Poseidon commitment scale linearly in `max_signers` and become the next-tier contributor as the cap grows (this is what bends the scaling tables above between 1000 and 2000 signers). Field extraction, target-type pin, and seq-no range check are negligible.

## What the fallback circuit adds

`fallback_circuit.rs` runs the same shape on **two** attestations carried in `attestation_bytes` and `attestation_2_bytes`, sharing a single BK set:

- One must have `target_type == Primary (0x00000000)`, the other `target_type == Fallback (0x01000000)` — constrained byte-wise.
- The `block_id` byte ranges of both assigned messages are constrained equal, so both attestations name the same block.
- Each message gets its own hash-to-curve and its own `verify_bls_attestation_with_assigned_msghash` call, both using `ThresholdMode::Fallback` (> n/2).
- The BK-set commitment and public instances ([block_id, bk_set_commitment, block_seq_no, last_seen]) are emitted once, taken from the primary-typed attestation.

Cost is roughly **2× the primary** circuit: two BLS verifications and two hash-to-curves, one BK-set load.

## Attestation byte layout

The bytes are produced by `bincode::serialize(&envelope)` (bincode 1.x, default
config), where `Envelope` is serialized via `EnvelopeSerDe`:

```rust
struct EnvelopeSerDe<TSignature, TData> {
    aggregated_signature: TSignature,          // gosh_blst::min_pk::Signature wrapper
    signature_occurrences: Vec<(SignerIndex, u16)>,
    data: TData,                                // = AttestationData
}
```

Bincode default rules used below: `Vec<T>` and `serialize_bytes` get an 8-byte
u64 LE length prefix; fixed-size types are inlined; `#[repr(u8)]` enums encode
as a 4-byte LE discriminant.

### Outer envelope

| Offset | Size | Field | Notes |
|---|---|---|---|
| `0..8` | 8 | `aggregated_signature` length prefix | u64 LE = 192. `gosh_blst` Signature serializes via `serialize_bytes`, which bincode frames with a u64 length. |
| `8..200` | 192 | BLS signature | Compressed BLS12-381 G2 point. |
| `200..208` | 8 | `signature_occurrences` length prefix | u64 LE = `num_signers`. |
| `208..208 + 4·num_signers` | `4·num_signers` | signer entries | Each entry: `u16 LE signer_idx ‖ u16 LE count`, sorted by `signer_idx`. |
| `208 + 4·num_signers ..` | 120 | `AttestationData` | See next table. |

The dynamic offset where `AttestationData` begins is computed by
[`attestation_data_offset(num_signers)`](src/attestation_data_parser.rs).

### Inner `AttestationData`

Defined at `acki-nacki/node/src/node/associated_types.rs:319`. Field order
(matters for bincode) and serialized sizes:

| Rel. offset | Size | Field | Type / serialization |
|---|---|---|---|
| `0..8` | 8 | `parent_block_id` length prefix | u64 LE = 32 (`BlockIdentifier` uses `serde_with = bytes`). |
| `8..40` | 32 | `parent_block_id` bytes | `[u8; 32]`. |
| `40..48` | 8 | `block_id` length prefix | u64 LE = 32. |
| `48..80` | 32 | `block_id` bytes | `[u8; 32]`. |
| `80..84` | 4 | `block_seq_no` | `BlockSeqNo(u32)` → u32 LE. |
| `84..116` | 32 | `envelope_hash` | `AckiNackiEnvelopeHash([u8; 32])` (transparent). |
| `116..120` | 4 | `target_type` | `AttestationTargetType` enum → u32 LE discriminant. `Primary = 0x00000000`, `Fallback = 0x01000000`. |
| Total | **120** | | |

The relative offsets the circuit cares about are mirrored as constants in
[`src/lib.rs`](src/lib.rs):

- `BLOCK_ID_REL_OFFSET = 48`
- `BLOCK_SEQ_NO_REL_OFFSET = 80`
- `TARGET_TYPE_REL_OFFSET = 116`
- `ATTESTATION_DATA_LEN = 120`

### Source of truth in acki-nacki

| Type / call | Path |
|---|---|
| `Envelope` / `EnvelopeSerDe` | `node/src/bls/envelope.rs` |
| `Signature(gosh_blst::min_pk::Signature)` | `node/src/bls/gosh_bls.rs:41` |
| `AttestationData`, `AttestationTargetType` | `node/src/node/associated_types.rs:313`, `:319` |
| `BlockIdentifier` (`ser = bytes`) | `node/libs/node-types/src/types.rs` |
| `BlockSeqNo(u32)` | `node/src/types/block_seq_no.rs:13` |
| `AckiNackiEnvelopeHash([u8; 32])` (transparent) | `node/src/types/ackinacki_block/envelope_hash.rs:14` |
| Producing call site (in this repo) | `test-data-gen/src/generator.rs` — `bincode::serialize(&attestation_envelope)?` |

## Primary circuit scaling (MockProver, K=20)

Measured via `test_primary_attestation_bls_checker_mock_scaling` (`#[ignore]`,
all-sign case, dev profile).

| Signers | Advice cols | Lookup cols | MockProver::run | assert | Total |
|---:|---:|---:|---:|---:|---:|
|  500 |  32 | 3 |   66 s |   8 s |   78 s |
|  700 |  42 | 3 |  118 s |  11 s |  135 s |
| 1000 |  60 | 4 |  274 s |  20 s |  303 s |
| 2000 | 144 | 7 | 1434 s |  96 s | 1552 s |

All cases fit K=20; columns grow ~linearly to 1000 signers, then jump.

## Primary circuit real prover (KZG, K=20)

Measured via `tests/real_prover_primary.rs` (`#[ignore]` sizing tests,
all-sign, dev profile). One VK/PK per `max_signers`; single matching
`bk_set_size` per row.

| max_signers | Adv / Lkp cols | keygen_vk | keygen_pk | Proof gen | Proof    | Verify  |
|---:|---:|---:|---:|---:|---:|---:|
|  300 | 22 / 3 |  —      |  —      | 103.4 s |  8192 B | 3.45 ms |
|  500 | 32 / 3 |  74.1 s |  67.4 s | 128.6 s | 10976 B | 4.40 ms |
| 1000 | 60 / 4 | 338.3 s | 539.1 s | 684.7 s | 19392 B | 9.16 ms |
| 2000 | — / — |  —      |  —      |  —      |  —      |  —      |

(`max_signers = 300` row uses cached VK/PK from `test_real_prover_primary_multi_bk_set`,
so keygen timings are omitted there; its column counts are read from the cached
`base_circuit_params`.)

## Standalone scaling binary (`primary_real_prover`)

For benchmarking on a remote host where you'd rather ship a single executable
than the whole workspace, the crate exposes a `[[bin]]` target gated behind
the `bench-bin` feature. It iterates over a list of `max_signers` values with
`bk_set_size == max_signers`, runs SRS → keygen → prove → verify for each,
and prints `[timing] ...` lines for every phase.

```text
# Build (locally — produces target/release/primary_real_prover)
cargo build --release -p attestation-bls-checker-circuit \
    --features bench-bin --bin primary_real_prover
```

Upload `target/release/primary_real_prover` to the benchmark host and run it
there — no source tree or cargo needed on the remote box:

```text
# Defaults: max_signers ∈ {300, 500, 1000, 2000}, artefacts in ./params
./primary_real_prover

# Pick specific cases:
./primary_real_prover 300 500

# Direct artefacts elsewhere (e.g. a fast scratch disk):
ARTIFACT_DIR=/scratch/primary_bench ./primary_real_prover 1000 2000

# Override the SRS cache (halo2-base `gen_srs` convention):
PARAMS_DIR=/scratch/srs ./primary_real_prover 300
```

Each `max_signers` value gets its own VK/PK because `BaseCircuitParams` change
with the cap; artefact filenames are suffixed with `max_signers` so caches
don't collide across cases. Re-running a case loads cached VK/PK and skips
straight to prove+verify.

Capture all timings with `./primary_real_prover 2>&1 | tee bench.log`.

### Remote bench (n14: 128 GB RAM, 48 cores)

Real-prover scaling collected by running `./primary_real_prover {300,500,1000,2000}`
on `n14.srv.gosh.sh` (release binary, K=20, cache-miss VK/PK each row, single
matching `bk_set_size == max_signers` per row). `Case total` includes SRS load,
keygen, prove, and 5× verify; SRS itself is ≤ 1 s once `K=20` params are on disk.

| max_signers | Adv / Lkp cols | keygen_vk | keygen_pk | Proof gen | Proof   | Verify (avg of 5) | Case total |
|---:|---:|---:|---:|---:|---:|---:|---:|
|  300 |  24 / 2 |  60.8 s |  47.5 s | 142.4 s |  8 192 B |  9.35 ms |  297.9 s |
|  500 |  32 / 3 |  95.9 s |  68.0 s | 188.2 s | 10 976 B | 10.05 ms |  386.7 s |
| 1000 |  60 / 4 | 186.2 s | 149.1 s | 330.6 s | 19 392 B | 12.32 ms |  734.1 s |
| 2000 | 144 / 7 | 420.6 s | 335.0 s | 770.4 s | 44 896 B | 20.22 ms | 1680.4 s |

Scaling shape (doubling signers):

- **300 → 500 (×1.67):** keygen_vk ×1.58, keygen_pk ×1.43, proof gen ×1.32
- **500 → 1000 (×2.0):** keygen_vk ×1.94, keygen_pk ×2.19, proof gen ×1.76
- **1000 → 2000 (×2.0):** keygen_vk ×2.26, keygen_pk ×2.25, proof gen ×2.33

Proof size grows sub-linearly: ×5.5 (8 → 44 KB) across ×6.7 signers.
Verification scales 9.4 ms → 20.2 ms (×2.2 over ×6.7 signers) — still well
under the EVM gas-budget zone for any reasonable signer cap.

### n14 vs. local (dev profile, `tests/real_prover_primary.rs`)

Not strictly apples-to-apples — local rows come from in-repo `cargo test`
runs (dev profile, single-developer machine); n14 rows are the release
binary. Still useful as a sanity check:

| max_signers | keygen_vk (local → n14)         | keygen_pk (local → n14)         | Proof gen (local → n14)               |
|---:|---:|---:|---:|
|  300 | — / 60.8 s                       | — / 47.5 s                       | 103.4 → 142.4 s (n14 ×1.38 slower)    |
|  500 |  74.1 → 95.9 s (n14 ×1.29 slower) | 67.4 → 68.0 s (≈ tie)            | 128.6 → 188.2 s (n14 ×1.46 slower)    |
| 1000 | 338.3 → 186.2 s (n14 ×1.82 **faster**) | 539.1 → 149.1 s (n14 ×3.6 **faster**) | 684.7 → 330.6 s (n14 ×2.07 **faster**) |
| 2000 | — / 420.6 s                       | — / 335.0 s                       | — / 770.4 s                            |

Crossover sits around `max_signers ≈ 1000`: below that, fast single-thread
cores beat n14's 48-way parallelism; above it, n14's rayon scaling dominates
and the gap widens with size. At 2000 signers a single end-to-end run
(SRS → keygen → prove → 5× verify) finishes in **~28 minutes** on n14,
producing a **~44 KB proof verifiable in ~20 ms**.

## Parallel-throughput research binary (`primary_real_prover_parallel`)

Sister binary that asks "given one host, how many primary-attestation proofs
can I generate in parallel before RAM or CPU saturates?". For one
`max_signers` value it generates `N` independent attestations (via the same
synthetic generator), proves+verifies them concurrently, and reports
per-worker timings, peak RSS, peak/avg CPU%, and a projected per-proof RAM
footprint that can be used to size larger hosts.

Critically, the binary loads the PK **once** and shares it across worker
threads via `Arc<ProvingKey>` — without this, N concurrent provers would
each hold their own multi-GB PK copy and the experiment would measure RAM
thrashing rather than parallel compute.

```text
# Build
cargo build --release -p attestation-bls-checker-circuit \
    --features bench-bin --bin primary_real_prover_parallel

# Defaults: max_signers=300, parallelism=2, no sequential baseline
./primary_real_prover_parallel

# With sequential baseline (doubles total runtime but yields speedup ratio)
./primary_real_prover_parallel --baseline

# Test how single-proof rayon parallelism interacts with concurrent workers
./primary_real_prover_parallel --parallelism 4 --rayon-threads 2

# Heavy host
ARTIFACT_DIR=/scratch ./primary_real_prover_parallel \
    --max-signers 1000 --parallelism 4 --baseline
```

The final `=== Extrapolation ===` block estimates the maximum concurrent
proofs sustainable at 16 / 32 / 64 / 128 / 256 GB RAM and prints
"cores-per-proof" so the user can pair it with the target host's CPU count.
Note that on a host where a single proof already saturates all cores,
extra concurrency only buys throughput if RAM permits — the binary will
show this directly via the parallel-phase CPU% reading.

### n14 parallel sweep summary (full report: [`PARALLEL_BENCHMARK_N14_REPORT.md`](PARALLEL_BENCHMARK_N14_REPORT.md))

12-case sweep on n14 (2× Xeon E5-2687W v4 = 24 physical / 48 logical cores, 128 GB RAM) for `max_signers ∈ {300, 1000, 2000}`:

| max_signers | T_seq (s) | best-throughput N | proofs/hour @ N | RAM @ N (GB) | RAM-safe N (≤128 GB) |
|---:|---:|---:|---:|---:|---:|
|  300 | 110 |  8–16 | 64–70 |  65–118 | 16 |
| 1000 | 283 |   4–6 | 23–24 | 101–120 |  5–6 |
| 2000 | 662 |     2 |   6.4 |     99 |  2 |

Key takeaways:
- **CPU is the binding ceiling.** Average CPU never exceeds ~25 of 48 *logical* threads (~12–13 of 24 *physical* cores) even at N=16. Hyperthreading does not help — proving is FPU/AVX-bound, so the second SMT thread per core sees no extra throughput.
- **Speedup vs sequential plateaus by N≈4** (1.75× ms=300, 1.94× ms=1000) and barely improves further (2.15× / 2.04× at the largest tested N).
- **Marginal RAM per extra concurrent proof:** 6.9 GB (ms=300), 16.6 GB (ms=1000), 21.7 GB (ms=2000) — scales with circuit width (`num_advice_per_phase` 24 → 60 → 144).
- **Practical recommendation on a 128 GB / 48-core host:** N=4 for ms=300 (best efficiency/RAM trade), N=4 for ms=1000 (23 proofs/h at 101 GB), N=2 for ms=2000 (the only feasible step above solo).

### Off-circuit parser

[`src/attestation_data_parser.rs`](src/attestation_data_parser.rs) exposes:

- `parse_signature_bytes(att) -> &[u8]` — the 192-byte signature
- `parse_num_signers(att) -> usize`
- `parse_signer_entries(att) -> Vec<(u16, u16)>` — `(signer_idx, count)`
- `attestation_data_offset(num_signers) -> usize`
- `parse_attestation_data_bytes(att) -> &[u8]` — the 120-byte `AttestationData` payload

All offset literals (`8`, `192`, `200`, `208`, `4`) live as named constants at
the top of that file. If acki-nacki ever changes the envelope layout (e.g.
swaps the signature wrapper, reorders `AttestationData` fields, or adds a new
field), the constants there and in `src/lib.rs` are the only things that need
to move.
