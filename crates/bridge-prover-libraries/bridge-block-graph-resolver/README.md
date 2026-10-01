# Bridge block graph resolver

This crate maintains a rolling graph of finalized Acki Nacki blocks in memory
or SQLite and resolves a minimum-hop proof path from a newer thread-0 anchor to
a target event block. Slot `0` (parent) and slots `1+` (cross-thread references)
are all indexed and retained in JSON output as `ref_index`.

The GraphQL schema does not expose an explicit finality filter for the `blocks`
connection. Configure `--gql-url` to an endpoint whose block feed contains only
finalized blocks. Without `--database`, `sync` is diagnostic and its state ends
with the process. Pass a SQLite path to preserve the rolling graph and positive
path cache across runs. A database is permanently bound to the normalized
GraphQL endpoint used when it is created and refuses to open against another
endpoint.

If the target is older than the rolling window, the resolver reads its
generation time in milliseconds, binary-searches thread 0 by
`(thread_id, height)` for the first
not-older anchor candidate, and examines thread-0 blocks to the right. Referenced
blocks on other threads are loaded lazily by block ID and saved into the same
reverse index. This avoids scanning every block produced by every thread during
the intervening weeks. `--max-anchor-candidates` bounds the cold search (default
`1000`); `--max-hops` and `--max-visited-blocks` bound the complete resolution.
Cold searches reuse expanded branches and known path suffixes between adjacent
anchor candidates.

`--algorithm` selects the resolver strategy. `reverse-index` first runs the
store-first reverse BFS and is the default; on a miss, it uses the same
incremental forward historical search described below. `forward-thread` skips
the reverse-index attempt entirely: it binary-searches the last thread-0 block
at or before the target time, scans thread 0 to the right, follows ordinary
references until it enters the target thread at a sufficient height, and then
follows slot-0 parents to the exact target. Neither forward historical search
reads incoming edges, and its visited-block budget is shared by the complete
resolve rather than reset for every anchor candidate.

For repeatable resolver tests, the optional `test-utils` feature exports
`TestGraphGenerator` and `SyntheticBlockProvider`. By default the generator runs
65,455 ticks—just over six logical hours—and advances time by 330 ms per tick.
Each thread has a seeded random millisecond offset inside the tick, retained for
its lifetime. The generator produces one block in every active thread and keeps
slot 0 as the parent. For each adjacent routing thread, a cross-reference is
included with a seeded 60% probability and points to a seeded random block from
that neighbor's latest 10 blocks rather than always to its tip. Split
inheritance and retirement handoff references remain mandatory. A seeded,
smoothed load process drives thread split and
retirement with cooldown and hysteresis; thread 0 remains alive, split children
inherit the old tip, retiring tips are handed to a surviving neighbor, and the
active set is capped at 200 threads. No wall-clock waiting is involved.

The resolver's provider API uses Unix milliseconds. The current live GraphQL
schema exposes only whole-second `gen_utime`, so the GraphQL adapter multiplies
it by 1000; synthetic and custom providers can retain true millisecond
precision.

```text
bridge-block-graph-resolver resolve --gql-url http://127.0.0.1:8600/graphql \
  --block-id <64-hex-character-id> --algorithm forward-thread
bridge-block-graph-resolver sync --gql-url http://127.0.0.1:8600/graphql \
  --scan-window 1000 --database resolver.sqlite
bridge-block-graph-resolver serve --gql-url http://127.0.0.1:8600/graphql \
  --database resolver.sqlite --listen 127.0.0.1:8787
```

The long-running server performs an initial sync before accepting requests and
then refreshes the same graph periodically. Its endpoints are:

- `GET /healthz` — health, store version and last sync state;
- `GET /v1/status` — the same status payload without health-status mapping;
- `POST /v1/sync` — trigger a serialized refresh immediately;
- `POST /v1/resolve` — accept a JSON `ResolutionRequest` and return a
  `ResolvedPath` or a structured JSON error.
