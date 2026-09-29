# Direction (b) multi-path collector — algorithm

Supersedes the single-anchor design in
[`walker_algorithm.md`](walker_algorithm.md) for
the research pipeline. The single-anchor collector answers "does *some*
walk exist for this event?"; the multi-path collector answers **"what
does the shortest walk cost, and how many alternatives are there?"**

## Why this matters

In the bridge SNARK, one `BridgeMultiHopProof` step ≈ **8 SHA-256
gadgets**. A 3-hop walk costs ~24 SHA-256; a 40-hop walk costs
~320 SHA-256. Aggregation cost scales linearly. So a single number
matters more than any other from this experiment:

> **`shortest_length`** — the fewest hops needed to prove event X via
> some thread-0 anchor Y.

Everything else (anchor thread, path diversity, wait time) is context
for interpreting the length distribution and for spotting when a
smarter walker could do better.

## What the collector builds

A **global block DAG** across every thread, incrementally, from polled
GraphQL data:

- **Nodes** = blocks, keyed by `hash` (falling back to `block_id`).
  Attributes: `thread_id`, `seq_no`, `refs = proof_block_refs`.
- **Edges** = `refs[i]` for every slot `i`. Each edge points to an
  *older* block (parent for slot 0, cross-thread reference for
  slot ≥ 1).
- **Reverse index** `parents_of[target] = {sources...}` — used for BFS
  from an older event newer-ward.

The DAG grows only. Blocks are never evicted during a session.

## What the collector runs

Every poll (`--poll-interval`, default 2 s):

1. **`fetch_latest_blocks(scan_window)`** — pull the newest N blocks
   ordered by `seq_no desc`. `scan_window = 50` catches ~15 s of
   history on a ~300 ms/block devnet.
2. **Ingest each new block.** Record refs, update `parents_of`,
   per-thread tip.
3. **On-demand fetch** for any referenced block we haven't seen.
   Bounded by `--fetch-budget-per-poll` (default 200). This lets the
   parent chain and cross-thread targets appear in the DAG even when
   they're outside the recent-blocks polling window.
4. **`fetch_bridge_extouts()`** — pull WithdrawalInitiated events from
   the USDCBridge account. Register each new event with two deadlines:
   a **hard orphan cap** (`--max-wait-anchor-s`, default 600 s) if
   nothing ever anchors; and a **soft observation window**
   (`--observation-window-s`, default 300 s) that starts counting the
   moment the *first* path appears.
5. **BFS re-evaluation** — for each pending event, re-run
   `enumerate_paths` against the current DAG (§ below).
6. **Close events past their deadline** — emit one JSONL record with
   the full path set.

## BFS on the reverse graph

For event X at block `x_key`:

```
distance[x_key] = 0
queue = [x_key]
while queue and |anchors| < max_paths:
    cur = queue.popleft()
    if cur is on thread 0 and distance[cur] > 0:
        record cur as an anchor at distance distance[cur]
        (don't extend past cur — Y IS the anchor)
        continue
    for src in parents_of[cur]:
        if src already visited: skip
        distance[src] = distance[cur] + 1
        predecessor[src] = (cur, ref-slot)
        queue.push(src)
```

Path reconstruction is a straight predecessor walk from Y down to X.
Because BFS visits blocks in increasing distance order, the *first
time* each Y appears is at its shortest distance to X — recording that
distance and skipping further expansion out of Y gives us the
"shortest path per anchor" set.

**Cap knobs:**
- `--max-hop-budget` (default 60) — BFS depth ceiling. 60 is
  intentionally above the protocol 50-hop lag ceiling: an indirect
  path via another thread could conceivably require more than 50 hops
  in some pathological configuration. Cheaply widening the ceiling
  lets us see those cases if they exist.
- `--max-paths-per-event` (default 20) — cap on distinct anchors
  recorded. On a healthy network the shortest few anchors are what
  matter; twenty is comfortable head-room.

**What BFS does NOT enumerate:** multiple *distinct* paths to the
*same* anchor Y. If two different chains lead from Y to X, we record
only the shortest. For the length-distribution question this is fine;
different anchors give path diversity, and multiple paths to the same
anchor would all pay ≥ shortest hops anyway.

## Why not the simpler direct-hit strategy?

The single-anchor collector maintained `thread0_view[t]`: for each
non-default thread `t`, the sorted list of thread-0 blocks with a ref
into `t`. Given event X on thread `t`, the anchor was the *first
entry* with `b_seq ≥ x_seq`, and the walk was `B →ref[0]→
parent(B) → …`. That algorithm is optimal on a **2-thread network**
because the only path shape is thread-0 → x_thread + parent chain.

On 4+ threads, an **indirect prefix** can be strictly shorter:

```
      direct  Y —→ B_t (thread t, ~50 back) —→ …parent walk… → X    (52 hops)
    indirect  Y —→ B_t' (thread t', slot 2) —→ B_t (thread t) —→ … → X  (4 hops)
```

when thread 0 is lagging thread `t` heavily but a sibling thread `t'`
tracks `t` tightly at a seq close to X. The direct-hit heuristic never
considers B_t' because it doesn't index other threads. BFS considers
every edge type uniformly.

**Empirically we may not see this happen on 2-thread runs.** The point
of running BFS anyway is to have the machinery in place when a 4-thread
run becomes available, and to detect degenerate 2-thread cases (e.g.,
producer stalls into crawl mode).

## Termination / soft deadlines

A pending event has two clocks:

- **Hard orphan cap** — `--max-wait-anchor-s`. If no path is found
  within this window, emit `orphan_timeout`.
- **Observation window** — `--observation-window-s`. Starts the
  moment the *first* path appears. During the window, every new
  block ingest triggers a re-BFS; if a shorter path is discovered,
  the record captures the new best.

Combined deadline: `first_seen + orphan_cap` initially; once a first
path appears, `min(current_deadline, first_path_wall + window)`. This
way orphaned events don't loiter forever, and short-anchor cases
don't idle waiting for a shorter path that isn't coming.

## Failure modes the collector emits

| outcome                    | meaning                                                   |
|----------------------------|-----------------------------------------------------------|
| `same_thread_trivial`      | X on thread 0 — trivial anchor, no path needed            |
| `ok`                       | ≥1 path found                                             |
| `orphan_timeout`           | no path found within `max_wait_anchor_s`                  |
| `event_block_unresolved`   | the event referenced a block ID we could never fetch      |

## What the analyzer surfaces

Seven digest sections in `multipath_analyzer.py`:

1. Total + outcome breakdown.
2. Thread distribution of X.
3. `shortest_length` stats + histogram (this is the headline number).
4. Number of alternative paths per event; first-vs-best gap.
5. Latency: `T_first_path_wall_s`, `T_best_path_wall_s`,
   `improvement_wait_s` (best − first).
6. Shortest-path thread signature census (top 20). Reveals which
   thread stacks show up in short walks.
7. **Direct vs indirect** classifier on the shortest path's first
   hop. A high indirect fraction means BFS was doing real work; a
   near-100 % direct fraction means the simple direct-hit walker
   would have sufficed.

## Deferred / non-goals (same as single-anchor plan)

- **Cryptographic verification.** The collector confirms only that the
  ref edges exist — it does not open Poseidon proofs or Leaf-7 SHA
  preimages. A follow-on step can shell out to
  `bridge-event-witness::resolve_cross_thread_chain` primitives to
  confirm "would Circuit 4 accept this walk?".
- **Multiple distinct paths to the same anchor.** Only shortest per
  anchor is recorded.
- **Retirement / postmortem detection.** Not distinguished from a
  slow-to-anchor live thread.
- **Aggregator hop-cost benchmark.** Circuit-side budget is a separate
  measurement.
- **Direction (a) walker rewrite.** The daemon walker in
  `bridge-event-witness` still walks Direction (a) and will need
  independent updating before Direction (b) semantics can go live
  end-to-end.
