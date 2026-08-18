> **⚠️ ARCHIVED 2026-08-18 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/EVM-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

# `daemon-live` performance — shellnet Deploy #5

Measured 2026-08-04/05 against Sepolia + AckiNacki shellnet, single M-series
Mac, dev laptop-class hardware. Numbers are per bundle (one key-block every
512 seq_nos, ~5 min chain cadence).

## Baseline (pre-`4a38abe`, cold-keygen every bundle)

Bundles 4–7 immediately after `daemon-live` cold start:

| Phase                              | Wall time  |
|------------------------------------|-----------:|
| Circuit 1a Poseidon proof          | ~83 s      |
| Circuit 2 Poseidon proof + chain   | ~150 s     |
| `wrap_poseidon_snark(primary)`     | 0.21 s     |
| **Primary aggregate subprocess**   | **~213 s** |
| `wrap_poseidon_snark(layer)`       | 0.23 s     |
| **Layer aggregate subprocess**     | **~317 s** |
| Sepolia submit + confirm           | ~22 s      |
| **Total per bundle**               | **~13m45s**|

Root cause of the ~530 s wrap+aggregate: `aggregate-proof` re-ran the full
K=21 outer keygen every invocation because the daemon was not forwarding
`--pk-cache-dir` — the disk-cache path in `aggregator_cache::keygen_or_load`
was never exercised.

## After `4a38abe` (`--pk-cache-dir` wired, cache HIT)

Bundles 10–12, both Primary + Layer PKs served from `<params_dir>/pk_cache`:

| Phase                              | Cold      | Warm      | Δ          |
|------------------------------------|----------:|----------:|-----------:|
| Primary aggregate subprocess       | 213 s     | **124 s** | −89 s / −42% |
| Layer aggregate subprocess         | 317 s     | **192 s** | −125 s / −39% |
| wrap+aggregate total               | 534 s     | **317 s** | −218 s / −41% |
| **Total per bundle wall time**     | ~13m45s   | **~9m57s**| −3m48s / −28% |

Warm variance is tight: Primary 120–126 s, Layer 186–196 s across three
consecutive bundles. Subprocess time is now dominated by `create_proof` +
Yul self-check regeneration, not keygen.

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
  inner_snark)` (see `bridge-evm-aggregator/src/aggregator_cache.rs`).
  Regenerating any verifier under a new `AggregatorConfig` produces a
  fresh slot; the old files linger until purged manually.
- **Override**: `BRIDGE_PK_CACHE_DIR` env / `--pk-cache-dir` CLI flag.
  Both `daemon-live` and `prove-withdraw-shplonk` accept it.

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
