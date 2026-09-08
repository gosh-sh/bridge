# `probe_bk_updates` — quickstart

Read-only cadence probe for the AN `bkSetUpdates` stream.
Source: `src/bin/probe_bk_updates.rs`. Spec: `bridge-prover-libraries/docs/bk_set_update_no_circuit3_plan.md` §Phase 2.

## Build

```sh
cd crates/bridge-prover-libraries
cargo build --release --bin probe_bk_updates
```

## Run

```sh
# minimal — endpoint from env
BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql \
    ./target/release/probe_bk_updates

# full options
./target/release/probe_bk_updates \
    --gql https://shellnet.ackinacki.org/graphql \
    --last 500 \
    --gap-threshold 100 \
    --json > cadence.json
```

## Flags

| Flag | Default | Meaning |
|---|---|---|
| `--gql URL` | `$BRIDGE_GQL_ENDPOINT` | AN GraphQL endpoint |
| `--last N` | (all) | Truncate report to last N events (walk is always full) |
| `--gap-threshold N` | `100` | seq_no gap below which consecutive events count as a cluster |
| `--json` | off | Machine-readable output only (no per-page stderr chatter) |

## Output (human mode)

```
=== bkSetUpdates cadence probe ===
endpoint         : https://shellnet.ackinacki.org/graphql
total walked     : 1234
events reported  : 500
first height     : 2584711
last  height     : 2988000
missing height   : 0
--- gap seqno stats ---
min / median / p90 / max : 32 / 4096 / 32768 / 131072
mean             : 8123.4
bursts (gap<=100 seqno): 3 clusters, largest=5
--- events ---
  height=   2584711  delta=       n/a  block_id=abc...
  height=   2584777  delta=        66  block_id=def...
  ...
```

## What the numbers tell you

- **Median / p90 gap** → typical inter-rotation seq_no distance. Compared to `W·P` window (128×4 = 512 seq_no on the current test config), tells you whether one-at-a-time drain is fine (median ≫ W·P) or you need to plan for batching (median ≤ W·P).
- **Max gap** → longest quiet period. Sanity check that the fetcher's pagination reach is enough (with default 500-page pages, we're fine up to ~5M rotations).
- **Bursts** → how often multiple rotations arrive close together. If `largest_burst_len > 1` and clusters are common, drain loop needs to handle back-to-back events without falling behind.

## When to run

- **Before** enabling BK rotation on a chain — establishes a zero baseline.
- **First 24h after** rotation is enabled — captures real cadence with fresh data.
- **Weekly** on any rotating deploy — trend detection (accelerating rotation → sizing revisit).

The binary is read-only and touches no state files; safe to run against production endpoints.
