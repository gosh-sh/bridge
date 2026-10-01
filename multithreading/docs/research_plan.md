# Cross-thread reachability — research plan

`Y` (a thread-0 block anchored on-chain via `layerWindows`) is *newer* than
`X` (the block that emits the withdrawal event on a non-default thread `t`).
The walk follows `proof_block_refs` newer-to-older, from `Y` down to `X`.

## The path shape we prove

Merkle convention throughout: `A ◀── C` means "C is a leaf inside A's L7"
(right is a leaf of left). Every path we characterise reads root-on-left,
leaf-on-right:

```
Y (thread 0, on-chain-anchored)  ◀──  … arbitrary DAG of refs[0]/refs[i≥1] …  ◀──  X (thread t, event block)
```

- `proof_block_refs[0]` is the same-thread parent id
  (`node/src/types/ackinacki_block/mod.rs:549-552`).
- `proof_block_refs[1..]` are cross-thread refs; producer emits one ref per
  live thread it's advancing, monotone in that thread's seq_no
  (`should_include`, `node/src/block/producer/process.rs:530-540`).
- Transitive reachability of ancestors of `B` in thread t is protocol-honored
  via `walk_back_into_history`
  (`node/libs/thread-reference-state/src/lib.rs:544-612`).
- Each hop is one `BridgeMultiHopProof` SNARK (H=1), ≈ 8 SHA-256 gadgets
  per hop. Same L7 SHA + Poseidon inner-path primitive regardless of which
  ref slot is taken — slot 0 (parent-chain) and slot ≥ 1 (cross-thread) hops
  cost identically, so the research target is **shortest** path in the full
  ref-DAG, not just the direct-hit heuristic.

## What the ≤50 lag bound gives us

`MAX_UNREFERENCED_THREAD_LAG = 50`
(`node/src/protocol/cross_thread_ref_enforcement/mod.rs:56`). Thread 0 can
lag its view of any live non-default thread by at most 50 finalized blocks
before keepers withhold attestation. "Any advance passes"
(`evaluate_thread_lag:150`), so a stalling producer can crawl +1 per lag
window, but cannot stall indefinitely.

Impact on same-thread parent hops `(b_seq − x_seq)`:

- **Typical:** 0–10 (checkpoint stride
  `CROSS_THREAD_REF_HEIGHT_STEP = 10` at `process.rs:541-629`).
- **Worst case:** ~50 under crawl-mode enforcement pressure.

**Skip modes** (X never anchored):
1. `X.thread` is retired (postmortem → `refs = []` on retirement blocks;
   thread removed via
   `node/src/multithreading/thread_synchrinization_service.rs:87-90`).
2. `X.thread` still live, but thread 0 has not yet emitted a ref with
   `b_seq ≥ x_seq` — bounded by the ≤50 lag rule.
3. Thread 0 references `X.thread` only *indirectly* via another thread's
   ref chain. Rare on the 2-thread devnet; needs BFS in the collector.
   Deferred (see below).

## Headline empirical metrics

The session (see `../runbooks/run_multipath_session.md`) yields one JSONL
record per event, which the analyzer digests into:

1. **Thread distribution of X.** With the current USDCBridge deployment at
   `account_id = 0x1a1a…1a1a` under `DEFAULT_DAPP_ID`, we *expect* every
   event to land on thread 0 in the current split-thread test. Cross-thread
   reachability only kicks in when this expectation is violated; the primary
   purpose of the first run is to confirm or refute it.
2. **Shortest-path length distribution.** Per event, the collector reports
   `shortest_length` — hops in the cheapest `Y ◀── … ◀── X` path found within
   the 5-minute observation window. Each hop ≈ 8 SHA-256 gadgets, so this is
   the direct circuit-cost signal. Target: p90 ≤ 10; concern zone: p90 ≥ 20.
3. **All-paths structural diversity.** `all_paths` (up to
   `--max-paths-per-event`, default 20) captures alternative anchors that
   appear during the window. Feeds §7 of the analyzer digest — direct vs
   indirect prefix breakdown (whether the shortest walk stayed in `x_thread`
   or went through another thread).
4. **Latency.** `T_first_path_wall_s` (event → any path), `T_best_path_wall_s`
   (event → shortest path seen), `improvement_wait_s` (how much extra delay
   the caller pays for a *good* path vs the first-any path).
5. **No-path-in-budget rate.** Fraction of non-thread-0 events that saw no
   `Y ◀── … ◀── X` path within the observation window. Any nonzero rate is
   an operational problem (retention policy, node-side guarantee, or the
   deferred skip-mode follow-ups below).

## Collector design (topology only)

Full description in
[`multipath_algorithm.md`](multipath_algorithm.md);
implementation at `../research/multipath_collector.py`.

Two loops:

1. **Global block-DAG maintenance.** Each poll (~2 s) pulls the latest N
   blocks across all threads; each block contributes edges
   `(block_id → proof_block_refs[i])` to a global reverse-graph index.
   Missing ref targets are fetch-on-demand within a per-poll budget so the
   DAG stays connected as new blocks arrive.
2. **Per-event BFS.** For each `WithdrawalInitiated` event X: if thread-0,
   close trivially; else run BFS on the reverse graph rooted at X, treating
   every incoming ref (slot 0 *and* slot ≥ 1) uniformly. Every time BFS
   reaches a thread-0 block, that block is a candidate anchor Y; record the
   shortest `Y ◀── … ◀── X` path found. Keep watching for the observation
   window (default 300 s) so alternative-Y paths that arrive during the
   window get enumerated too.

**No SHA-256 / Poseidon reconstruction** — topology only. A later step can
shell out to a Rust helper that wraps
`bridge-event-witness`'s primitives to verify "would Circuit 4 accept this
walk?"; not in this pass.

## Non-goals / deferred

- **3+-thread indirect prefix in practice.** BFS on the reverse graph
  already handles this in principle (every ref slot is traversed
  uniformly). Not seen on the 2-thread test; the interesting empirical
  data lives on 4-thread runs.
- **Retirement detection.** The collector doesn't distinguish "thread t
  still live but slow" from "thread t retired." Add later by polling t's
  own tip and looking for postmortem markers.
- **Aggregator hop-cost measurement.** How many hops the aggregator can
  chain within the proving budget is a separate benchmark; not needed for
  the topology characterization pass.
- **Circuit-side slot-0 opening.** The BFS already counts slot-0 (parent
  chain) edges as first-class hops. The circuit and witness builder now
  accept `ref_index ∈ [0, 2^refs_tree_depth)` and select the Poseidon leaf
  tag on `is_zero(ref_index)` — slot-0 and cross-thread edges are first-class
  everywhere in the stack.
