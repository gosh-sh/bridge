# `daemon-live` performance

The baseline table below was collected on a single M-series Mac. A bundle is
one relayer submission, not every key block: current L1 stride is 1,024
seq_nos, while L2 stride is 16,384 seq_nos.

## Per-bundle wall time

`no cache` = first bundle after cold start, or `--pk-cache-dir` unset.
`with cache` = Primary + Layer outer PKs served from
`<params_dir>/pk_cache/`.

| Phase                              | no cache    | with cache | Δ            |
|------------------------------------|------------:|-----------:|-------------:|
| Circuit 1a Poseidon proof          | ~83 s       | ~83 s      | —            |
| Circuit 2 Poseidon proof + chain   | ~150 s      | ~150 s     | —            |
| `wrap_poseidon_snark(primary)`     | 0.21 s      | 0.21 s     | —            |
| **Primary aggregate subprocess**   | **~213 s**  | **~124 s** | −89 s / −42% |
| `wrap_poseidon_snark(layer)`       | 0.23 s      | 0.23 s     | —            |
| **Layer aggregate subprocess**     | **~317 s**  | **~192 s** | −125 s / −39% |
| wrap+aggregate total               | 534 s       | **317 s**  | −218 s / −41% |
| Sepolia submit + confirm           | ~22 s       | ~22 s      | —            |
| **Total per bundle**               | **~13m45s** | **~9m57s** | −3m48s / −28% |

With-cache variance is tight: Primary 120–126 s, Layer 186–196 s across
three consecutive bundles. Time is dominated by `create_proof` + Yul
self-check regeneration, not keygen.

## Enabling the PK cache

`--pk-cache-dir <path>` on `relayer daemon-live` / `prove-withdraw-shplonk`,
or `BRIDGE_PK_CACHE_DIR=<path>` in the env. The daemon forwards it to the
`aggregate-proof` subprocess, which routes through
`aggregator_cache::keygen_or_load` (see
`bridge-evm-aggregator/src/aggregator_cache.rs`).

## Operational notes

- **Concurrency model:** the daemon runs one bundle pipeline. Attestation and
  layer proving/aggregation stages are sequential, so it does not process
  independent historical bundles concurrently. Individual Halo2 stages do
  use many cores, however; the ~10 minute warm time is not invariant to CPU
  count. More cores can shorten a stage but do not multiply the number of
  in-flight bundles.
- **n14-class live acceptance (2026-08-27):** individual Halo2 stages showed
  multi-core CPU use, peak memory was about 23 GiB, and five consecutive L2
  cycles confirmed successfully. L2's roughly 91-minute chain cadence left
  ample idle time after each proof on that host class.
- **Cache footprint at K=22:** the Primary + Layer outer cache occupies about
  17 GiB in `<params_dir>/pk_cache/`, on top of roughly 17 GiB of SRS + inner
  PKs. Keep at least 80 GiB free before a cold first cycle for generation,
  temporary files and recovery headroom.
- **First-bundle write is fragile**: needs ≥20 GB free at bundle start.
  ENOSPC mid-write leaves a truncated `.pk` without a `.meta.json`
  companion; every subsequent bundle then panics with
  `UnexpectedEof: failed to fill whole buffer` inside `snark-verifier-sdk`.
  Recovery: `rm <name>__v2__*.pk` for the affected slot; the next bundle
  re-keygens.
- **Slot key** is content-addressed by `(base_name, config, agg_params,
  inner_snark)`. Regenerating any verifier under a new `AggregatorConfig`
  produces a fresh slot; the old files linger until purged manually.

## Reproduction

```bash
cd bridge/crates/an-bridge-prover
# BRIDGE_CONFIG_DIR must already be exported (./L1_config or ./L2_config)
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
./target/release/relayer --state "$BRIDGE_CONFIG_DIR/relayer-state.json" daemon-live \
  2>&1 | tee logs/perf_baseline_${BRIDGE_CONFIG_DIR##*/}.log
```

Timing lines to grep:

```
aggregate-proof subprocess (PrimaryAggregatorVerifier) took … ms
aggregate-proof subprocess (LayerHashesAggregatorVerifier) took … ms
aggregated_source: bundle wrap+aggregate (attestation + layer) total … ms
verified seq_no=…
```
