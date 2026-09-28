# Cross-thread reachability research plan

## Goal

Empirically characterize `X.refs → thread-0 Y` walks on a live multi-thread devnet so
we can decide whether Direction (a) — described in
[`../DRAFT_cross_thread_reachability_issue.md`](../DRAFT_cross_thread_reachability_issue.md)
— is safe to bake into Circuit 4 as-is.

Two headline metrics answer that question:

1. **Non-anchorable rate** = fraction of `WithdrawalInitiated` events whose block has no
   reachable thread-0 ancestor via `proof_block_refs`. Failure modes counted separately:
   `empty_refs`, `stuck`, `too_deep`, `gql_error`.
2. **Hop distribution** — histogram of walk depth for events that *did* reach thread 0.
   Tail informs `L_MAX` sizing in the Circuit 4 spec.

No SHA-256 / Poseidon opening verification — pure GQL ref topology.

## Four processes running in parallel

```
┌─────────────────────────────────────────────────────────────┐
│  P1  local acki-nacki node   (make run)                     │
│  P2  Michael's mt test       (cli.py test-multithread-...)  │  ← forces split + holds
│  P3  event trigger loop      (test_deploy_and_withdraw_...) │  ← fires event every N s
│  P4  reachability collector  (reachability_collector.py)    │  ← walks refs, writes JSONL
└─────────────────────────────────────────────────────────────┘
```

Runbooks:
- P1 → [`../runbooks/run_local_node.md`](../runbooks/run_local_node.md)
- P2 → [`../runbooks/run_mt_test.md`](../runbooks/run_mt_test.md)
- P1+P2+P3+P4 as a full session → [`../runbooks/run_reachability_session.md`](../runbooks/run_reachability_session.md)

## §1 P1+P2 — node and thread-split test

Covered in the two runbooks above. Preconditions before P3+P4:

- `docker stats` shows all node containers healthy, RAM comfortably under Docker VM limit.
- GQL responds at `http://localhost:8700/graphql` (via `nginx0`).
- A quick check like
  ```graphql
  { blocks(order_by:{seq_no:desc}, limit:5) { seq_no thread_id proof_block_refs } }
  ```
  returns at least two distinct `thread_id`s and non-empty `proof_block_refs` on some rows.

## §2 P3 — event trigger loop

`research/trigger_loop.py` wraps `vendored/test_deploy_and_withdraw_only.py` in a
sleep-loop:

```bash
python research/trigger_loop.py --interval 60
```

Behavior:
- Each iteration invokes the vendored script as a subprocess. `MODE=local` is set
  automatically; `NETWORK` and `GRAPHQL_URL` default to `http://127.0.0.1:80` and
  `http://localhost/graphql`.
- `deploy_multisig` inside the vendored script self-funds ECC[3] via
  `USDCBridge.mintAndSend` — no per-event pre-mint needed.
- Suggested cadence sweep: 30 s → 60 s → 120 s across the run to cover both
  same-thread and cross-thread event landings vs. thread activity phases.

## §3 P4 — reachability collector (topology-only)

`research/reachability_collector.py` polls the GQL endpoint for new
`WithdrawalInitiated` events; for each new event `E` it:

1. Resolves `E.src_block_id` to a full block record
   (`{block_id, thread_id, seq_no, proof_block_refs, prev_hash}`).
2. Walks refs Direction (a): while `thread_id != 0`, pick a ref from
   `proof_block_refs[1..]` (slot 0 is the same-thread parent — not useful for
   cross-thread descent), preferring a ref whose block's `thread_id` is
   `< current thread_id` (heuristic: descent toward thread 0). Cap at 32 hops.
3. Records outcome as one JSONL line per event.

Outcomes: `ok | empty_refs | stuck | too_deep | gql_error`.

No SHA-256, no Poseidon, no in-circuit primitive invocation. If we later need
"would Circuit 4 actually accept this walk?", layer that on top by shelling out to
a small Rust helper wrapping `bridge-event-witness::resolve_cross_thread_chain`.

### Why not the existing Rust `resolve_cross_thread_chain`

`bridge-event-witness/src/enrich.rs:753` implements exactly this walk but *also*
builds full L7 hop witnesses and validates SHA-256 + Poseidon openings. For pure
topology research that verification is dead weight and creates a false blocker
(any opening mismatch would fail the walk before we could observe its refs
topology). Keep the collector minimal.

## §4 Statistics schema

Per-event record (JSONL, one line per event):

```json
{
  "event_msg_id":     "0x…",
  "event_block_hash": "…",        // Block.hash returned by GQL (what ExtOut.block_id maps to)
  "event_block_id":   "…",        // Consensus block_id
  "event_seq_no":     1234567,
  "event_thread_id":  2,          // integer, 0 = default thread
  "event_wall_ts":    "2026-09-28T14:32:11Z",
  "trigger_run_id":   "…",        // matches P3 tag (uuid4 per trigger invocation)

  "outcome":            "ok",     // ok | empty_refs | stuck | too_deep | gql_error
  "hop_count":          4,
  "terminal_thread_id": 0,
  "terminal_block_id":  "…",

  "hops": [
    {
      "from_block_id": "…", "from_thread_id": 2, "from_seq_no": 1234567,
      "from_refs_len": 3,
      "ref_index_chosen": 1,
      "to_block_id":   "…", "to_thread_id":   2, "to_seq_no":   1234501
    }
  ]
}
```

Aggregate views (produced ad-hoc in a small pandas notebook after the run):

1. **Non-anchorable rate** — `count(outcome ∈ {empty_refs, stuck, too_deep}) / total`,
   broken down by `event_thread_id` and by whether the event fell inside/outside a
   hold-burst window.
2. **Hop distribution histogram** for `outcome == ok`, plus tail probability
   `P(hop_count > H_cap)` for `H_cap ∈ {1, 4, 8, 20}`. Directly feeds the `L_MAX`
   sizing decision in
   [`MULTITHREAD_BRIDGE_EVENT_CIRCUIT_SPECIFICATION.md`](../../crates/bridge-circuits/bridge-event-prove-circuit/docs/MULTITHREAD_BRIDGE_EVENT_CIRCUIT_SPECIFICATION.md).
3. **Dapp-vs-thread correlation** — does the mt test actually push USDCBridge events
   onto a non-zero thread? See "Preflight pilot" below.
4. **Empty-refs subset** — cross-reference with block finality status via a separate
   GQL query; postmortem/orphaned blocks should dominate this bucket.

## Preflight pilot (10 min)

Before committing to a multi-hour run, confirm the mt test's split actually routes
USDCBridge events off thread 0. USDCBridge is deployed under `DEFAULT_DAPP_ID = 0x00…00`
at address `0:1a1a…1a1a` (see `generate_zerostate.py:1430,1502`). Whether that
account-id falls inside the parent thread-0 range or the split-off child range depends
on the split point Michael's test picks.

Procedure:

1. Start P1 (node) and P2 (mt test with hold flags).
2. Wait 2 min for the split to stabilize.
3. Start P3 with `--interval 30 --count 5` and P4.
4. Inspect `research/stats/*.jsonl` for the first ~5 events.

**Two outcomes:**

- Some `event_thread_id != 0` → proceed to the multi-hour run. Nothing else to do.
- All `event_thread_id == 0` → USDCBridge stayed on the parent thread. Options:
  - Redeploy the multisig at an address whose account-id falls inside the child-thread
    range that `cli.py`'s split logic carves off.
  - Or accept that in this test scenario the walk is trivially 0 hops and switch the
    thread-split trigger mechanism.

## §5 Operational recipe

See [`../runbooks/run_reachability_session.md`](../runbooks/run_reachability_session.md).

## Non-goals / deferred

- L7 opening reconstruction (SHA-256 + Poseidon) — deferred; not needed for topology-only
  measurement. Layer on later via a Rust helper if we want to verify that
  `outcome == ok` implies "Circuit 4 witness would build."
- Direction (b) walks (Y → X) — separate research effort; requires a different collector
  design that seeds from a thread-0 anchor forward.
- Postmortem-block deep dive — an extra GQL query per `empty_refs` event to classify by
  block-finality status. Not in the first-pass collector; add later if the bucket
  turns out to be large.
