# Direction (b) walker — algorithm sketch

**Scope.** Given a withdrawal event in block `X` on non-default thread `t`, build the private witness that proves `X` is reachable from some on-chain-anchored thread-0 block `Y` via `Y.refs → ... → X`.

**Status.** Design sketch — not yet implemented. Feeds the eventual replacement of `bridge-event-witness::resolve_cross_thread_chain` (currently Direction (a)).

---

## Path shape

```
Y (thread 0, anchored)  --refs[i≥1]-->  B (thread t, newest referenced)
B                        --refs[0]---->  A                    (same-thread parent)
A                        --refs[0]---->  ...
                         --refs[0]---->  X (event block)
```

Each arrow is one `BridgeMultiHopProof` SNARK (H=1). Total hops = **1 cross-thread + (B_seq − X_seq) same-thread parent**.

Backing evidence from acki-nacki:
- `proof_block_refs[0]` = same-thread parent id (`node/src/types/ackinacki_block/mod.rs:549-552`).
- Transitive reachability via parent chain is protocol-honored (`thread-reference-state/src/lib.rs:544-612`, `walk_back_into_history`).
- Producer emits only the newest block per referenced thread; older blocks in that thread are implied via parent chain (`node/src/block/producer/process.rs:530-540`).

---

## The "≤50 lag" bound — short version

`MAX_UNREFERENCED_THREAD_LAG = 50` at `node/src/protocol/cross_thread_ref_enforcement/mod.rs:56`.

Meaning: thread 0 can lag its view of thread t by at most 50 finalized blocks of thread t before keepers withhold attestation (`evaluate_thread_lag`, `mod.rs:137-187`).

Impact on same-thread parent hops (`B_seq − N_x` after X finalizes at seq `N_x`):
- **Typical:** 0–10, because producers advance in ~10-block checkpoint stride (`CROSS_THREAD_REF_HEIGHT_STEP = 10`, `process.rs:541-629`).
- **Worst case:** ~50, because enforcement only requires "any advance" — a stalling producer can crawl +1 per lag-window.

So total path is typically 1–11 hops, ceiling ~51 hops. Needs empirical measurement.

Aggregator side-effects: SHPLONK aggregator handles chained SNARKs off-chain; on-chain verifier size does not grow with hop count (fixed recursive-verify pair). Proving time is the linear cost.

---

## Data structures (in-memory, persisted between runs)

```text
# One entry per non-thread-0 thread we care about:
thread0_view[t] : sorted-by-y_seq list of
    (y_seq, y_block_id, ref_slot_i, b_seq, b_block_id)
  # "at thread-0 seq y_seq, thread 0's ref-slot i pointed at thread-t block b"

# Parent chain within any thread, populated lazily as we fetch:
thread_parents[t] : map block_id -> parent_block_id

# Targets in flight:
pending_targets : set of (x_block_id, x_seq, x_thread, deadline_wall_time)
```

Persist both `thread0_view` and `thread_parents` to disk so a daemon restart does not refetch history.

---

## Poll loop (period ~30 s)

### 1. Ingest new thread-0 blocks

```text
head = query_latest_blocks(1) on thread 0
for each new Y in (last_y_seq .. head]:
    gql = query_block_by_height(THREAD_0_ID, Y.seq)
    thread_parents[THREAD_0][Y.block_id] = gql.proof_block_refs[0]
    for i in 1 .. gql.proof_block_refs.len():          # slot 0 is parent, skip
        ref_id     = gql.proof_block_refs[i]
        ref_block  = query_proof_block_by_id(ref_id)
        t          = ref_block.thread_id
        insert (Y.seq, Y.block_id, i, ref_block.seq, ref_id) into thread0_view[t]
```

### 2. Try to close each pending target

```text
for each X in pending_targets:
    candidates = [ e in thread0_view[X.thread] where e.b_seq >= X.seq ]
    if candidates is empty:
        continue                     # not yet anchored, keep waiting

    # First candidate = smallest y_seq. Proven optimal below.
    (y_seq, y_id, slot_i, b_seq, b_id) = candidates[0]

    # Materialize same-thread parent chain B -> ... -> X:
    chain = [b_id]
    cur   = b_id
    while cur != X.block_id:
        cb     = query_proof_block_by_id(cur)
        parent = cb.proof_block_refs[0]
        thread_parents[X.thread][cur] = parent
        chain.append(parent)
        cur    = parent
        if len(chain) > MAX_HOP_BUDGET:      # e.g. 60
            abandon target, log "path too long"
            break

    # Emit hops:
    #   Hop 0: Y --refs[slot_i]--> B                 (cross-thread)
    #   Hop 1: B --refs[0]-------> chain[1]          (same-thread parent)
    #   ...
    #   Hop N: chain[N-1] --refs[0]--> X
    yield witness (y_id, slot_i, chain)
    remove X from pending_targets
```

### 3. Persist `thread0_view` and `thread_parents`.

---

## Why greedy-first-candidate is optimal (no Dijkstra)

`should_include` (`process.rs:530-540`) makes thread 0's view of each thread strictly monotone: for a fixed t, `b_seq` is non-decreasing in `y_seq`. Therefore:

- Let `(Y_first, B_first)` be the first candidate. Path length = `1 + (B_first.seq − N_x)`.
- Any later candidate `(Y', B')` has `B'.seq ≥ B_first.seq`, so its path via parent chain has length `≥` that of `(Y_first, B_first)`.
- Detours through a third thread never help: after one cross-thread hop we are already inside thread t, and further cross-thread refs would land outside t. So the greedy pick is provably shortest.

No graph search library needed for the 2-thread case.

---

## Termination and skip modes

- **Non-terminated thread t:** protocol enforces `MAX_UNREFERENCED_THREAD_LAG = 50`, so `candidates` becomes non-empty within ≤ 50 finalized blocks of thread t after X. Bounded.
- **Retired thread t (postmortem):** thread t's tail blocks emit `refs = []` (`process.rs:384-385`), the thread is removed via `thread_synchrinization_service.rs:87-90`, and no future thread-0 block will reference `b_seq ≥ N_x` if the retirement happened before that. X is permanently orphaned. Bridge must detect and report.
- **Deadline expiry:** wall-clock timeout on each target; on expiry, drop and log.

---

## Deferred / open

1. **3+-thread indirect prefix.** If thread 0 never references thread t directly but references thread t' whose refs include thread t: needs BFS over the thread-graph. Not seen in the 2-thread test; defer until 4-thread experiments show it.
2. **Retirement detection.** Poll thread-t tip; on postmortem-marker or extended stall combined with no new thread-0 refs → mark orphaned.
3. **Persistence schema.** Choose format for `thread0_view` / `thread_parents` snapshots (SQLite? JSONL? sled?).
4. **Backpressure.** In production with many concurrent targets, the index is shared; only parent-chain materialization is per-target. Cache parent-chain segments across nearby targets.

---

## Empirical measurements needed on 5-node devnet

Before committing to `MAX_HOP_BUDGET`, measure on 2–4 thread configurations:

| Metric | How measured |
|---|---|
| `T_anchor(X)` in finalized-blocks of thread t | poll thread-0 refs, note first `b_seq ≥ x_seq` |
| `T_anchor(X)` in wall seconds | wall-clock delta from X finalization to first anchoring Y |
| `same_thread_hops = B_seq − X_seq` | direct subtraction |
| orphan rate | fraction of X never anchored before session end / thread retired |

Test recipe: `acki-nacki/tests/mt/cli.py test-multithread-cross-thread --threads {2,3,4}` with cyclic hold flags per `MULTITHREAD_TEST_SESSION.md:54-77`.
