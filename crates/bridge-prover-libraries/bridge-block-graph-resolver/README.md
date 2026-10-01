# Bridge block graph resolver

## Overview

`bridge-block-graph-resolver` finds a verifiable path from a newer Acki Nacki
thread-0 block to an older target block. Every returned hop identifies the
exact `proof_block_refs` slot used by the newer block: slot `0` is its parent,
and slots `1+` are cross-thread references.

The resolver maintains a bounded local graph, tries its reverse index first,
and falls back to an incremental historical search when the target is older
than the local window. It can be used in three ways:

- as a Rust library with pluggable block providers and stores;
- as a CLI for one-shot resolution and explicit synchronization;
- as an HTTP server backed by SQLite and periodic synchronization.

## Getting started

All Cargo commands below run from `crates/bridge-prover-libraries`.

### Library

Add the workspace crate to the consumer's dependencies, then construct a
`GraphResolver` from a provider and a store. This example uses the built-in
GraphQL provider and persistent SQLite store:

```rust,no_run
use std::{str::FromStr, sync::Arc};

use bridge_block_graph_resolver::{
    BlockId, BlockProvider, EdgePolicy, GraphResolver, GraphqlBlockProvider,
    HistoricalSearchConfig, ResolutionPolicy, ResolutionRequest,
    ResolverLimits, SqliteStore,
};

async fn resolve_block() -> anyhow::Result<()> {
let provider = Arc::new(GraphqlBlockProvider::new(
    "http://127.0.0.1:8600/graphql",
)?);
let store = Arc::new(
    SqliteStore::open_with_edge_policy(
        "resolver.sqlite",
        provider.namespace(),
        EdgePolicy::AllReferences,
    ).await?,
);
let resolver = GraphResolver::new(provider, store, 1_000)
    .with_historical_search_config(HistoricalSearchConfig {
        max_anchor_candidates: 1_000,
    });

resolver.sync_latest(1_000).await?;
let path = resolver
    .resolve(ResolutionRequest {
        target: BlockId::from_str(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )?,
        policy: ResolutionPolicy::ShortestCurrent,
        limits: ResolverLimits {
            max_hops: 300,
            max_visited_blocks: 100_000,
        },
    })
    .await?;
println!("{} -> {} in {} hops", path.anchor, path.target, path.hops.len());
Ok(())
}
```

The library is split around two abstractions:

- `BlockProvider` supplies finalized blocks. It supports block-ID lookup,
  rolling-window synchronization, and optional timestamped lookups by thread
  and height for historical resolution. `GraphqlBlockProvider` is the built-in
  implementation; applications can provide their own backend.
- `ResolverStore` owns blocks, reverse edges, graph versions, and positive path
  cache entries. `MemoryStore` is process-local; `SqliteStore` persists state
  and binds the database to the provider namespace so it cannot be reused
  accidentally for another network. The store also owns its immutable
  `EdgePolicy`: `AllReferences` permits parent and cross-thread hops, while
  `CrossThreadOnly` permits only slots `1+`. Opening a SQLite store with a
  different policy atomically clears its graph and path cache before reuse.

`GraphResolver<P, S>` is generic over both traits. Provider I/O is performed
without holding store locks, and `StoreBatch` is the atomic ingestion boundary.
Applications control when `sync_latest()` runs; `resolve()` can also populate
the store lazily during a cold historical search.

The default features are `graphql`, `sqlite`, and `cli`. Disable default
features for a provider-neutral library build, or enable `test-utils` for the
deterministic graph generator and synthetic provider. The generator models
per-thread drifting block intervals, gradual load changes, split/collapse
lifecycle events, terminal tombstone blocks, and per-thread inboxes for
cross-references. A selected inbox contributes its newest block and discards
older pending blocks. Ordinary cross-references use a rotating per-block fanout
budget of six and retain the 60% inclusion probability; tombstones are included
mandatorily outside that budget. Load grows until the configured thread limit, then enters a
cooling period, reaches the configurable floor (55% by default), and resumes
growth after the minimum cooling duration. Generated
graph statistics include the total thread count and completed-thread lifetime
distribution (`min`, `median`, `p95`, `max`, and mean in milliseconds), plus
10-minute buckets with created-thread counts and active-thread `min`, mean, and
`max`. `GeneratedGraph::accessibility_map()` can additionally classify every
block under either edge policy and records its earliest reachable thread-0
anchor and anchoring delay.

### CLI

Resolve one block and print a JSON `ResolvedPath`:

```bash
cargo run -p bridge-block-graph-resolver --bin bridge-block-graph-resolver -- resolve \
  --gql-url http://127.0.0.1:8600/graphql \
  --block-id 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
  --database resolver.sqlite \
  --edge-policy all-references \
  --policy shortest-current \
  --pretty
```

`resolve` first synchronizes `--scan-window` recent blocks, then uses the same
store for resolution. Omit `--database` for a temporary in-memory store. Use
`--max-hops`, `--max-visited-blocks`, and `--max-anchor-candidates` to bound a
request.

Use `--edge-policy cross-thread-only` to enforce paths compatible with a
cross-reference-only witness. The option belongs to the store and must match
across `sync`, `resolve`, and `serve`; changing it for an existing SQLite
database clears the stored graph and positive path cache.

Synchronize and validate the rolling graph without resolving a target:

```bash
cargo run -p bridge-block-graph-resolver --bin bridge-block-graph-resolver -- sync \
  --gql-url http://127.0.0.1:8600/graphql \
  --scan-window 1000 \
  --edge-policy all-references \
  --database resolver.sqlite
```

Without `--database`, `sync` is diagnostic and its state disappears when the
process exits.

### HTTP server

Start a persistent resolver service:

```bash
cargo run -p bridge-block-graph-resolver --bin bridge-block-graph-resolver -- serve \
  --gql-url http://127.0.0.1:8600/graphql \
  --database resolver.sqlite \
  --edge-policy all-references \
  --listen 127.0.0.1:8787 \
  --scan-window 1000 \
  --sync-interval-secs 10
```

The server performs an initial sync before listening and refreshes the graph
periodically. Resolve a block with:

```bash
curl -sS http://127.0.0.1:8787/v1/resolve \
  -H 'content-type: application/json' \
  -d '{
    "target":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    "policy":"shortest-current",
    "limits":{"max_hops":300,"max_visited_blocks":100000}
  }'
```

Endpoints:

- `GET /healthz` returns health, store version, and last-sync state;
- `GET /v1/status` returns the same state, including `edge_policy`, without
  health-status mapping;
- `POST /v1/sync` triggers a serialized refresh;
- `POST /v1/resolve` accepts `ResolutionRequest` and returns `ResolvedPath` or
  a structured error.

## Algorithm

### Graph representation and ingestion

Acki Nacki references point from a newer block to an older block. For a block
`A` with `proof_block_refs = [B, C]`, the proof-direction edges are:

```text
A --ref[0]--> B    parent
A --ref[1]--> C    cross-thread reference
```

When a block is applied, the store records the block and indexes both edges by
destination:

```text
incoming[B] += (A, ref_index=0)
incoming[C] += (A, ref_index=1)
```

`sync_latest(limit)` fetches a finalized rolling window, validates it, applies
the batch atomically, and retains the highest configured number of blocks per
thread. Replacing a block removes its previous reverse edges before inserting
the new ones. Graph versions change only when stored graph data changes.

The store's edge policy is applied consistently to both search phases.
`all-references` indexes and traverses every slot. `cross-thread-only` ignores
slot `0` in the reverse index and in historical forward traversal, and it does
not use the target-thread parent-chain shortcut. In that mode a candidate must
reach the exact target using only slots `1+`.

### Cached and reverse-index resolution

Resolution starts with the positive path cache. A `FirstValid` path remains
valid across graph growth because finalized references are immutable.
`ShortestCurrent` is reused only when its graph and anchor versions still
match the store.

On a cache miss, the resolver loads the target and performs a level-order BFS
from the target over `incoming_edges`. This walks references backwards for
discovery—from older blocks to newer blocks—until it reaches thread 0. The
returned proof is reconstructed in the opposite direction, from the thread-0
anchor to the target, preserving `ref_index` for every hop.

The first distance containing a thread-0 block is the minimum-hop distance in
the indexed graph. Equal-length anchors are selected deterministically: higher
anchor height first, then lexicographically smaller block ID. The BFS uses a
visited set for cycle protection and is bounded by `max_hops` and
`max_visited_blocks`.

### Historical fallback

The reverse index is intentionally local and may not contain a path to an old
target. On a reverse-BFS miss, the resolver:

1. loads the target generation time in Unix milliseconds;
2. loads the latest available thread-0 block;
3. binary-searches thread 0 by `(thread_id, height)` for the first block whose
   generation time is not older than the target;
4. scans consecutive thread-0 anchor candidates to the right, bounded by
   `max_anchor_candidates`;
5. follows their ordinary references toward older blocks without querying the
   store's reverse index;
6. when it reaches the target thread at or above the target height, follows
   slot-0 parents to the exact target.

The fallback is incremental across anchor candidates. It remembers the largest
remaining hop budget with which each block was expanded, the discovered local
dependencies, and the best known suffix from each block to the target. A later
anchor therefore explores only new graph regions or regions reached with a
larger budget. Newly found suffixes propagate through already discovered
dependencies, avoiding repeated traversal and provider requests.

Fetched blocks are committed to the normal store, so subsequent resolutions
can use them through the reverse-index fast path. The visited-block budget is
shared by the entire historical resolution rather than reset for every anchor.
`FirstValid` returns as soon as an eligible path is known; `ShortestCurrent`
examines the bounded candidate range and applies the same deterministic anchor
tie-break.

### Finality and timestamps

`BlockProvider` must return finalized blocks only. The current GraphQL schema
does not expose a finality predicate on the `blocks` connection, so the
configured endpoint must provide a finalized block feed.

Provider timestamps use Unix milliseconds. The live GraphQL schema exposes
whole-second `gen_utime`; `GraphqlBlockProvider` multiplies it by `1000`.
Custom and synthetic providers can preserve true millisecond precision.
