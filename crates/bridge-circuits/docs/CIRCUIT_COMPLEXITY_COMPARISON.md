# Inner + outer complexity comparison across `bridge-circuits/`

Full-stack cell / column / outer-bytecode budget for every AN→ETH circuit that
lands as a Yul verifier under `contracts/ethereum/verifiers/`. Complements
[`SHA256_INVOCATIONS.md`](SHA256_INVOCATIONS.md) — SHA compression counts alone
do not decide the outer aggregator's fit, but drive the inner column count that
does.

Use this doc when picking `k_outer`, `num_advice_per_phase`, or
`VerifierUniversality` for a new inner circuit, and to justify the choice
against EIP-170.

## 0. Constants used

- **K = log2(rows)** inside `BaseCircuitParams` — SRS domain size for the inner
  circuit; a snark's row budget is `2^K − NUM_UNUSABLE_ROWS`.
- **SHA cell cost** = **≈ 354 000 advice cells per compression block** on the
  gosh eDSL `Sha256Chip::digest_bytes` (measured in
  `gosh-dark-dex-halo2-new-circuit::test_sha256_cell_count`, see
  [`SHA256_INVOCATIONS.md §0`](SHA256_INVOCATIONS.md#0-padding-math-why-some-calls-cost-more-than-one-compression)
  for the padding math).
- **EIP-170 runtime bytecode limit** = **24 576 B** (`contracts/ethereum/verifiers/README.md`,
  hard-checked by `scripts/check_shplonk_artefacts.sh`; soft warning fires at 90 %
  = 22 118 B).
- **Bytecode driver** = **inner `num_advice_per_phase`**. Empirical slope
  ~450–500 B per inner advice column at fixed universality (see §4).

## 1. Per-circuit shape table

| Circuit | File | Inner K | `num_advice_per_phase` | `num_lookup_advice` | `lookup_bits` | Inner PIs | SHA compressions | Poseidon perms |
|---|---|---|---|---|---|---|---|---|
| 1A primary attestation | `attestation-bls-checker-circuit/src/primary_circuit.rs` | 20 | (calc) | — | — | 4 | ~5 (BLS `hash_to_curve`) | 0 |
| 1B fallback attestation | `attestation-bls-checker-circuit/src/fallback_circuit.rs` | 21 | 22 | — | — | 4 | ~10 (2× BLS `hash_to_curve`) | 0 |
| 2 historical-layer-hashes | `historical-layer-hashes-movement-checker-circuit/src/circuit.rs` | 17 | **25** | ~9 | 16 | 14 | 8 (4× 64-B walk at L0) | up to 10 (`verify_chain_of_dense_proofs`, `MAX_CHAIN_LEN=10`) |
| 4a `BridgeEventFinalProof` | `bridge-event-prove-circuit/src/bridge_event_final_proof.rs` | 19 | 16 | 2 | 18 | 11 | 15 (4 BOC + 4×2 at L8) | ~127 (128-leaf dense_merkle_root) + Poseidon96 × 4 + nullifier |
| **4b `BridgeMultiHopProof` H=2 (rejected)** | `bridge-event-prove-circuit/src/multi_hop_proof.rs` | 17 | **50** | 14 | 16 | 2 | 16 (2 hops × 8) | ~22 (2 × (1 ref-leaf + 8-deep walk)) |
| **4b `BridgeMultiHopProof` H=1 (current)** | same | 17 | **≈ 24–25** (predicted) | ~8–14 | 16 | 2 | 8 (1 × 8) | ~11 (1 + 8-deep walk) |

Empty `num_advice_per_phase` cells for 1A/1B are `calculate_params(...)`
outputs at real-prover time (their driver crates re-tune from a `<name>.t.config`
template rather than fixing the count in source).

## 2. Cell budget per snark (SHA-dominated tier only: 2 · 4a · 4b)

At the K-sizes above, `2^K − NUM_UNUSABLE_ROWS ≈ 131 072 − 109 = 130 963` rows
for K=17. Each cell in the constraint system consumes one advice-cell slot in
one column at one row.

| Circuit | SHA cells | Poseidon + gate/byte parsing cells | PI packing + copy | **Total** (approx) | Rows @ K | Col floor = ⌈total / rows⌉ | Measured / predicted cols |
|---|---|---|---|---|---|---|---|
| 2 (layer hashes) | 8 × 354 k = **2.83 M** | ~10 chain Poseidon × ~250 = 2.5 k + preimage byte parsing / 10-layer loop ~10 k | ~450 (14 PIs) | **~2.85 M** | 130 963 | 22 | **25 (calculate_params)** |
| 4a (withdrawal) | 15 × 354 k = **5.31 M** | 128-leaf dense_merkle_root ~127 × ~1 000 + Poseidon96 × 4 ~1 000 each + nullifier ~1 000 ≈ **~135 k** | ~350 (11 PIs) | **~5.45 M** | 524 179 (K=19) | 11 | **16 (fixed)** |
| 4b H=2 (rejected) | 16 × 354 k = **5.66 M** | 2 × (1 ref-leaf + 8-deep gated walk) ≈ ~5 k + byte-parsing ~10 k | ~150 (2 PIs) | **~5.68 M** | 130 963 | 44 | **50 (fits, ~14 % slack)** |
| **4b H=1 (current)** | 8 × 354 k = **2.83 M** | 1 ref-leaf + 8-deep gated walk ≈ ~2.8 k + byte-parsing ~5 k | ~150 (2 PIs) | **~2.84 M** | 130 963 | 22 | **24–25 (predicted)** |

Two things to notice:

- **SHA is ~99 %** of the cell budget for the SHA-heavy tier. Everything else
  is rounding noise relative to how `calculate_params` picks columns.
- **Circuit 4b H=1 ≈ Circuit 2** in every dimension that matters for outer
  aggregation: same K, same SHA count, similar Poseidon count, *fewer* PIs
  (2 vs 14). It should not require more columns than layer hashes.

## 3. Outer verifier bytecode — measured

`contracts/ethereum/verifiers/SIZES` (`wc -c` on the committed `.bin` files
2026-09-18, hard-checked by `scripts/check_shplonk_artefacts.sh`; drift refuses
CI):

| Verifier | Inner K / cols / PIs | `k_outer` / universality | Bytes | vs EIP-170 (24 576 B) |
|---|---|---|---|---|
| `BridgeWithdrawalAggregatorVerifier.bin` | 19 / 16 / 11 | 21 / Full | 21 152 | 86 % (OK) |
| `PrimaryAggregatorVerifier.bin` | 20 / calc / 4 | 21 / Full | 21 494 | 87 % (OK) |
| `FallbackAggregatorVerifier.bin` | 21 / 22 / 4 | 21 / PreprocessedAsWitness | 21 493 | 87 % (OK) |
| `LayerHashesAggregatorVerifier.bin` | 17 / 25 / 14 | 22 / Full | 23 111 | 94 % (soft warn) |
| **BridgeMultiHopAggregatorVerifier H=2 (regen abandoned)** | 17 / 50 / 2 | 21 / Full | **33 213** | **135 % (FAIL)** |
| **BridgeMultiHopAggregatorVerifier H=1 (landed 2026-09-27)** | 17 / 25 / 2 | 21 / Full | **23 722** | **96.5 % (OK, above 90 % soft-warn)** |

## 4. What drives outer bytecode

The SHPLONK aggregator template has a fixed skeleton (~15 KB) plus
per-inner-VK cost:

- **α ≈ 450–500 B per inner advice column** (Full universality, k_outer=21).
  Direct evidence: Circuit 1B at K=20 needed 44 advice cols and Yul went to
  ~28 KB; re-keygened at K=21 the inner dropped to 22 advice cols and Yul
  dropped to 21 493 B (~7 KB saved by halving cols).
  Cross-check on the H=2→H=1 delta: at 25 cols predicted Yul =
  `33 213 − (50 − 25) × 480 = 21 213 B` — matches Circuit 4's 21 152 B
  reference within noise.
- **k_outer bump ≈ +500 B to +1 500 B bytecode** at fixed inner shape (not
  the dominant lever). Bumping k_outer buys more rows for the AggregationCircuit
  itself, not smaller bytecode.
- **`PreprocessedAsWitness` vs `Full`**: moves the inner VK from EVM
  constants into calldata. Direct evidence: FallbackAggregator (PreproWit,
  21 493 B) vs PrimaryAggregator (Full, 21 494 B) — same shape, ~1 B apart.
  Savings show up only when the inner VK is large; **~3 KB is a reasonable
  ceiling** for the SHA-heavy tier.
- **k_outer ≠ inner K**. k_outer picks the *aggregator* SRS domain; inner K
  picks the *inner circuit* SRS domain. Both need Hermez PPoT SRS at that K
  for production security (we ship K=21 today; K=22 would need a fresh
  ~35–40 min ptau download + convert).

## 5. Multi-hop H=1 landing (2026-09-27)

**Inner shape (K=17):** `num_advice_per_phase = 25`. MockProver min-col sweep
2026-09-27: 20 FAIL, 21 FAIL, 22 PASS, 25 PASS. Chose 25 for parity with
`historical-layer-hashes-movement-checker-circuit` and a 3-col safety margin
above the empirical floor of 22. PK size 516 MiB (H=2 was 1002 MiB).

**Outer:** `k_outer = 21, lookup_bits_outer = 20, VerifierUniversality::Full`
— same as `BridgeWithdrawalAggregatorVerifier`. Auto-tuned aggregator shape:
8 advice cols, 1 lookup advice, `lookup_bits = 20`, 14 outer instances
(12 accumulator + 2 inner).

**Measured Yul: 23 722 B (96.5 % of EIP-170).** Above the 90 % soft-warning
line (22 118 B); regeneration only forced on further growth. Prediction was
~21 KB — the 2.5 KB overshoot vs the empirical 480 B/col slope is consistent
with the outer aggregator landing at 8 advice cols and 14 outer instances
(both slightly higher than the Circuit 4a reference case, which is ~7 cols /
23 instances at 21 152 B). No Hermez K=22 SRS bootstrap needed — the K=21
SRS we already ship covers the outer.

Fallbacks if the measured Yul drifts on future regeneration:

1. **> 22 118 B (soft warn) but ≤ 24 576 B (current state)**: landed, noted;
   regeneration only forced on further growth.
2. **> 24 576 B**: first switch to `PreprocessedAsWitness` at k_outer=21 —
   saves ~3 KB with no SRS change.
3. **Still > 24 576 B**: then bump to k_outer=22 and do the Hermez K=22
   ptau bootstrap.

## 6. Cross-references

- [`SHA256_INVOCATIONS.md`](SHA256_INVOCATIONS.md) — where SHA is called
  in each circuit and how the block-id tree is opened at three different leaf
  positions (L0 / L7 / L8).
- `contracts/ethereum/verifiers/README.md` — deploy-time invariants,
  regeneration commands, size gates.
- `crates/bridge-evm-aggregator/src/aggregator.rs` — the source of truth for
  every verifier's `k_outer` / `lookup_bits_outer` / universality choice.
- `crates/bridge-prover-libraries/bridge-prover-lib/src/keys/*.rs` — one key
  manager per circuit, each pinning `KEYGEN_SRS_K` and a `_CIRCUIT_REVISION`
  that invalidates cached PK/VK on shape change.
