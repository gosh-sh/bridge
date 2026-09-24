# Parallel proving: experiment results & extrapolation

Concise summary of the `primary_real_prover_parallel` research runs on the
local 8-core macOS host (M-series, dev/release Rust, `cargo build --release`).
See [`primary_real_prover_parallel.rs`](./primary_real_prover_parallel.rs)
for the harness and raw logs `../parallel_300_p1.log` / `../parallel_300_p2.log`
for full output.

The research question: **does proving N independent attestations
concurrently beat doing them back-to-back, and what hardware does it need
at larger `max_signers`?**

---

## Measured runs (max_signers = 300, K = 20)

`base_circuit_params`: `num_advice=24, num_lookup_advice=2, lookup_bits=19`,
all-sign witness, 8 logical cores, rayon = 8 threads (auto).

| phase                      | wall      | per-proof prove | peak RSS | avg CPU |
|----------------------------|----------:|----------------:|---------:|--------:|
| keygen (VK + PK)           |  72.8 s   |   —             | —        | —       |
| 1 proof, alone             |  91.0 s   |  88.1 s         | 6.98 GB  | 5.9 cores |
| 2 proofs, sequential       | 176.6 s   |  85.3 s avg     | 6.70 GB  | 6.3 cores |
| 2 proofs, parallel (N=2)   | 265.2 s   | 260.3 s avg     | 7.95 GB  | 5.2 cores |

Cache hits between phases: PK is loaded once and shared across workers via
`Arc<ProvingKey>`, so RAM scaling reflects only witness/scratch duplication.

### Headline numbers

- **Throughput speedup at N=2: 0.67×** (efficiency 33%) — running 2 proofs in
  parallel is **slower** than back-to-back on this host.
- **RAM amplification at N=2: 1.19×** — the second concurrent proof costs
  only ~1.25 GB extra on top of the first proof's 6.7 GB. PK sharing works.
- **CPU per proof: ~6 cores** when proving alone. Two outer workers split
  the 8-core rayon pool → contention dominates the supposed parallelism gain.

---

## Cost model (derived from the table above)

### RAM

```
RAM(N concurrent proofs) ≈ R_base + (N − 1) × R_marginal
```

- **R_base** ("RAM single") = peak RSS of **one** proof running alone.
  At `max_signers=300`: **6.7 GB**. Pays for PK + SRS + one proof's
  witness/FFT scratch/transcript.
- **R_marginal** ("RAM marginal") = **extra** RAM per **additional**
  concurrent proof. At `max_signers=300`: **~1.25 GB**. PK is shared, so
  each extra worker only pays for its own witness/scratch.

`Marginal` = "incremental cost of one more unit", same sense as in
economics. Nothing to do with "small" or "borderline".

### CPU

```
Time(N) ≈ T_solo × max(1, N × C_solo / H_cores)
```

- **T_solo** = single-proof prove time (88 s here).
- **C_solo** = cores a single proof saturates (≈ 6 here, via rayon).
- **H_cores** = logical cores on the host.
- **Optimal N*** ≈ `H_cores / C_solo`. On this 8-core box, N\* ≈ 1.3 →
  N=1 is the right choice, which matches what we measured.

---

## Extrapolation to larger `max_signers`

Advice-column counts grow roughly linearly with `max_signers`; RAM tracks
column count. Measured `num_advice / num_lookup_advice`:

| max_signers | num_advice | num_lookup_advice | source |
|---:|---:|---:|---|
|  300 | 24 |  2 | this benchmark (parallel run) |
|  500 | 32 |  3 | `tests/real_prover_primary.rs` |
| 1000 | 60 |  4 | `tests/real_prover_primary.rs` |
| 2000 | ~120 (est.) | ~6 (est.) | linear extrapolation |

Projected single-proof RAM (scaling 6.7 GB at 24 advice cols linearly with
column count):

| max_signers | est. R_base | est. R_marginal |
|---:|---:|---:|
|  300 |  6.7 GB |  1.25 GB |
|  500 | ~9 GB   | ~1.7 GB  |
| 1000 | ~17 GB  | ~3 GB    |
| 2000 | ~34 GB  | ~6 GB    |

This is consistent with the OOM observed locally at `max_signers=2000` on
16 GB: a single proof alone needs more RAM than the box has.

### Max concurrent proofs by host RAM (`RAM(N) = R_base + (N−1) × R_marginal`)

| host RAM | N at ms=300 | N at ms=500 | N at ms=1000 | N at ms=2000 |
|---:|---:|---:|---:|---:|
|  16 GB |  8 |  5 |  0 (single proof OOM) | 0 |
|  32 GB | 21 | 14 |  6 |  0 |
|  64 GB | 47 | 33 | 16 |  6 |
| 128 GB | 98 | 70 | 38 | 16 |
| 256 GB | 200 | 145 | 80 | 37 |

These are **RAM ceilings only**. The actual sustainable N is
`min(RAM_ceiling, H_cores / C_solo)`.

### CPU ceiling (assuming C_solo ≈ 6 cores at ms=300, scaling ~linearly with cols)

| max_signers | est. C_solo | N\* on 16-core | N\* on 32-core | N\* on 64-core | N\* on 128-core |
|---:|---:|---:|---:|---:|---:|
|  300 |  6 |  2 |  5 | 10 | 21 |
|  500 |  8 |  2 |  4 |  8 | 16 |
| 1000 | 15 |  1 |  2 |  4 |  8 |
| 2000 | 30 |  0\* |  1 |  2 |  4 |

\* 2000 needs a 32-core+ host to even prove one in reasonable wall time.

### Recommended hosts

The right host is the one where `RAM_ceiling ≥ N*_CPU`, so neither resource
is the binding constraint.

| target                         | suggested AWS / similar | RAM | cores |
|--------------------------------|-------------------------|----:|------:|
| ms=300, parallelism ≈ 5–10      | `c6i.16xlarge` / `m6i.16xlarge` | 128 GB | 64 |
| ms=1000, parallelism ≈ 2–5      | `r6i.4xlarge` to `r6i.8xlarge`  |  128–256 GB | 16–32 |
| ms=2000, parallelism ≥ 2        | `x2idn.16xlarge` / `r6i.16xlarge` | 512 GB / 256 GB | 64 |
| ms=2000, just to prove **once** | any 64 GB+ box                  | 64 GB | 8+ |

---

## Caveats

- C_solo ≈ 6 was measured on a single 8-core host; it may shift on hosts
  with very different memory bandwidth / NUMA topology.
- RAM numbers for ms=1000 / 2000 are **linear extrapolations** of the
  advice-column scaling, not measured. The actual constant for `R_marginal`
  may grow super-linearly if FFT scratch dominates at large K.
- `sysinfo` on darwin reports an unrealistically low post-keygen RSS
  baseline (22–84 MB) and short instantaneous CPU spikes >100 cores. The
  **peak RSS during proving** and the **average CPU%** are the load-bearing
  numbers; treat the rest as sampling artifacts.
- All measurements above are dev-profile workspace, release-profile binary.
  An LTO + `target-cpu=native` build would likely shave another 10–20% off
  prove time but won't change the parallelism conclusion.

---

## Bottom line

On a single host where one proof already uses ≥ 75% of the cores, **parallel
proving is a loss**. Parallelism only pays off when the host has enough
cores that `H_cores ≥ N × C_solo` — i.e. you go wide on a 32+ core server,
not on a laptop. RAM is the easier constraint to plan for: budget
`R_base + (N−1) × R_marginal` and you're done.
