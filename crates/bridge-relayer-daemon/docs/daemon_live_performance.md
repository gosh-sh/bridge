# `daemon-live` performance

Per-bundle numbers on a single M-series Mac, dev laptop-class hardware.
One bundle = one key-block, 512 seq_nos, ~5 min chain cadence.

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

- **Cache footprint at K=21**: Primary PK 7.65 GB + Layer PK 9.55 GB =
  ~17 GB in `<params_dir>/pk_cache/`, on top of `params/`'s ~17 GB of SRS +
  inner PKs. Steady-state disk requirement ~34 GB.
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
set -a && source .env.shellnet && set +a
./target/release/relayer daemon-live 2>&1 | tee logs/perf_baseline.log
```

Timing lines to grep:

```
aggregate-proof subprocess (PrimaryAggregatorVerifier) took … ms
aggregate-proof subprocess (LayerHashesAggregatorVerifier) took … ms
aggregated_source: bundle wrap+aggregate (attestation + layer) total … ms
verified seq_no=…
```
