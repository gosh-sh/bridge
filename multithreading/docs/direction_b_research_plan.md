# Direction (b) reachability — research plan

Supersedes `reachability_research_plan.md` (Direction (a)). The Direction (a)
files under `research/` and `runbooks/` are kept only as history; they were
built around the walk `X → thread-0 Y` that we abandoned when the
`withdrawByProofBundle` contract semantics flipped (see the `feature/multithreading`
branch commit `51ccf24`).

## The pivot in one sentence

Under Direction (b), `Y` (a thread-0 block anchored on-chain via
`layerWindows`) is *newer* than `X` (the block that emits the withdrawal
event on a non-default thread `t`). The walk goes `Y → … → X`, following
`proof_block_refs` newer-to-older.

## The path shape we prove

```
Y (thread 0, on-chain-anchored)
  └─ refs[i≥1] ─→ B (thread t, seq ≥ X.seq)
                    └─ refs[0] ─→ parent(B)
                                    └─ refs[0] ─→ …
                                                    └─ refs[0] ─→ X
```

- `proof_block_refs[0]` is the same-thread parent id
  (`node/src/types/ackinacki_block/mod.rs:549-552`).
- `proof_block_refs[1..]` are cross-thread refs; producer emits one ref per
  live thread it's advancing, monotone in that thread's seq_no
  (`should_include`, `node/src/block/producer/process.rs:530-540`).
- Transitive reachability of ancestors of `B` in thread t is protocol-honored
  via `walk_back_into_history`
  (`node/libs/thread-reference-state/src/lib.rs:544-612`).
- Each hop is one `BridgeMultiHopProof` SNARK (H=1). Same L7 SHA + Poseidon
  inner-path primitive regardless of which ref slot is taken.

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

## Two headline empirical metrics

The session (see `../runbooks/run_direction_b_session.md`) yields JSONL
records the analyzer digests into:

1. **Thread distribution of X.** With the current USDCBridge deployment at
   `account_id = 0x1a1a…1a1a` under `DEFAULT_DAPP_ID`, we *expect* every
   event to land on thread 0 in the current split-thread test. Direction (b)
   only kicks in when this expectation is violated; the primary purpose of
   the first run is to confirm or refute it.
2. **When events *do* land off thread 0:**
   - `T_anchor_blocks_thread_t = b_seq − x_seq` — hop count for the
     same-thread parent walk. Expected p90 ≤ 10, ceiling ~50.
   - `T_anchor_wall_s` — wall-clock delay from event first seen to first
     anchoring Y observed. At ~300 ms/block (the branch's slow-block-builder
     tuning), the ≤50 lag translates to ~15 s worst-case-normal.
   - Orphan / failure rate.

## Collector design (topology only)

Full description in
[`direction_b_walker_algorithm.md`](direction_b_walker_algorithm.md);
implementation at `../research/direction_b_collector.py`.

Two loops:

1. **Thread-0 index maintenance.** Each poll pulls the latest N blocks,
   filters to thread-0, and for each cross-thread ref (slots ≥ 1) fetches
   the referenced block and appends
   `(y_seq, y_id, slot_i, b_thread, b_seq, b_id)` to
   `thread0_view[b_thread]`. That list is append-only and monotone-non-
   decreasing in `b_seq` by protocol.
2. **Event correlation.** For each `WithdrawalInitiated` event, resolve the
   block; if thread-0, close trivially; else scan
   `thread0_view[x_thread]` for the first entry with `b_seq ≥ x_seq`, walk
   B's parent chain to X, emit outcome and hop counts.

**No SHA-256 / Poseidon reconstruction** — topology only. A later step can
shell out to a Rust helper that wraps
`bridge-event-witness::resolve_cross_thread_chain`'s primitives to verify
"would Circuit 4 accept this walk?"; not in this pass.

## Non-goals / deferred

- **3+-thread indirect prefix.** If thread 0 references t only via a
  third thread t' (2-hop cross-thread prefix), we need BFS over the
  thread-graph. Not seen on the 2-thread test; deferred to a 4-thread run.
- **Retirement detection.** The collector doesn't distinguish "thread t
  still live but slow" from "thread t retired." Add later by polling t's
  own tip and looking for postmortem markers.
- **Aggregator hop-cost measurement.** How many hops the aggregator can
  chain within the proving budget is a separate benchmark; not needed for
  the topology characterization pass.
- **Direction (a) walker.** The daemon walker in
  `bridge-event-witness::resolve_cross_thread_chain` still walks
  Direction (a); it will need a rewrite mirroring this plan before the
  Direction (b) contract semantics can go live end-to-end. Out of scope
  for this document.
