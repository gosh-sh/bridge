# Direction (b) multi-path session

End-to-end recipe for the split-thread devnet run that measures the
**shortest walk** `Y (thread 0) → … → X (event block)` per
WithdrawalInitiated event, along with alternative paths.

Companion to
[`run_session.md`](run_session.md). The
single-anchor collector answers "does *some* walk exist"; this session
answers **"how short is the shortest walk, and how many alternatives
exist within a 5-minute observation window?"** Model + math live in
[`../docs/multipath_algorithm.md`](../docs/multipath_algorithm.md).

## What we're measuring

Per event, one JSONL row with:

- Where X landed: `x_thread`, `x_seq`, `x_block_id`.
- **`shortest_length`** — fewest hops from any thread-0 anchor Y to
  X. Each hop ≈ 8 SHA-256 gadgets in the circuit, so this is the
  cost knob we care about.
- **`all_paths`** — every distinct-anchor path enumerated (capped at
  `--max-paths-per-event`, default 20), ordered shortest-first.
- **`shortest_path.signature`** — thread stack of the winning path,
  e.g. `t0 -> t1a2b -> t2c3d -> X`. Tells you whether the shortest
  walk went direct (Y → x_thread → … → X) or indirect (through
  another thread).
- Latency: `T_first_path_wall_s`, `T_best_path_wall_s`,
  `improvement_wait_s`.
- Failure buckets: `orphan_timeout`, `event_block_unresolved`,
  `same_thread_trivial`.

## Expectation before running

With the current USDCBridge deployment (account_id `1a1a…1a1a` under
`DEFAULT_DAPP_ID`), the 2-thread test still routes every event to
thread 0 → most rows will be `same_thread_trivial`. This session's
practical output on 2 threads is:

1. Confirm the thread-0 routing empirically.
2. If any event *does* land off thread 0, produce the shortest-path
   record so we can check the direct-hit heuristic wasn't overpaying.

The multi-path machinery pays off on **4-thread runs**, where a
Y → t' → t indirect prefix may strictly beat the direct 50-hop parent
walk. Run this recipe the same way when the 4-thread test becomes
available; no collector changes needed.

## Prerequisites

Same as `run_session.md`:

| # | Item |
|---|------|
| 1 | Docker Desktop VM ≥ 12 GiB. |
| 2 | acki-nacki checkout at `$ACKI_NACKI_ROOT`, branch **`feature/node-3953-add-test-slow-block-builder-with-300ms-per-block-build-on`**. |
| 3 | Node built and healthy per [`../../../run_acki_nacki_node.md`](../../../run_acki_nacki_node.md). |
| 4 | State-v2-compatible tooling at `/Volumes/x5/v2_tools/` and `/Volumes/x5/cargo-target/release/`. |
| 5 | `research/stats/` writable. |

## Pane layout (tmux, 5 panes)

Order **A → B → (wait 2 min for split to stabilize) → D → C**. D
before C so the collector has already primed the graph before the
first event fires.

| Pane | Role                                                       | See |
|------|------------------------------------------------------------|-----|
| A    | Local acki-nacki node                                      | [`../../../run_acki_nacki_node.md`](../../../run_acki_nacki_node.md) |
| B    | Split-thread `cli.py` test                                 | This file / `run_session.md` |
| C    | WithdrawalInitiated trigger loop (`trigger_loop.py`)       | This file |
| D    | **Multi-path collector** (`multipath_collector.py`) | This file |
| E    | Observability (`docker stats`, node logs)                  | — |

## Pane B — split-thread test

Identical to `run_session.md`; run from the acki-nacki repo
root, not this directory:

```bash
cd $ACKI_NACKI_ROOT   # branch feature/node-3953-...

DISABLE_MV=true \
CLI_NAME=/Volumes/x5/v2_tools/tvm-cli \
TVM_CLI=/Volumes/x5/v2_tools/tvm-cli \
SOLD=/Volumes/x5/v2_tools/sold \
TVM_DEBUGGER=/Volumes/x5/v2_tools/tvm-debugger \
ZEROSTATE_HELPER=/Volumes/x5/cargo-target/release/zerostate-helper \
NODE_HELPER=/Volumes/x5/cargo-target/release/node-helper \
MESSAGE_ARCHIVE_OTEL_RUN_ID=local-2-thread \
python3 tests/mt/cli.py test-multithread-cross-thread \
  --threads 2 \
  --total 20000 \
  --hold-burst-total 5000 \
  --hold-quiet-seconds 0 \
  --batch-size 200 \
  --deploy-value 12000000000000 \
  --minimum-balance 8000000000000 \
  --hold-seconds 1800 \
  --timeout 2400
```

Verify the split before proceeding (in another shell):

```bash
curl -s http://localhost/graphql -H 'Content-Type: application/json' \
  -d '{"query":"{ blockchain { blocks(order_by:{seq_no:desc}, limit: 10) { edges { node { seq_no thread_id proof_block_refs } } } } }"}' \
  | jq '.data.blockchain.blocks.edges[].node
        | {seq_no, thread_id: (.thread_id[0:12]+"…"),
           refs_len: (.proof_block_refs | length)}'
```

Want to see: at least two distinct `thread_id` values in the last 10
blocks AND `refs_len ≥ 2` on thread-0 rows (slot 0 parent + at least
one cross-thread ref).

## Pane D — multi-path collector

```bash
cd /Users/alinat/HALO2_TVM_EXPERIMENTS/bridge/multithreading
python3 research/multipath_collector.py \
  --graphql http://localhost/graphql \
  --out research/stats/dirb-mp-$(date +%Y%m%d-%H%M).jsonl \
  --poll-interval 2.0 \
  --scan-window 50 \
  --fetch-budget-per-poll 200 \
  --observation-window-s 300 \
  --max-wait-anchor-s 600 \
  --max-hop-budget 60 \
  --max-paths-per-event 20 \
  --heartbeat-s 30
```

Knobs to consider tuning:

| Flag                        | Default | When to change                                     |
|-----------------------------|---------|----------------------------------------------------|
| `--scan-window`             | 50      | Bump if devnet block rate outruns 25 blocks/s.     |
| `--fetch-budget-per-poll`   | 200     | Rare need on 2 threads; may need +on 4 threads.    |
| `--observation-window-s`    | 300     | Keep at 5 min so an alternate short path has time. |
| `--max-wait-anchor-s`       | 600     | Protocol ceiling ~15 s worst-case at 300 ms/block. |
| `--max-hop-budget`          | 60      | Bump only when investigating pathological paths.   |
| `--max-paths-per-event`     | 20      | Enough for structural diversity.                   |

Heartbeat lines look like:

```
[2026-…] heartbeat: pending=3 closed=47 blocks=812 edges=3244 gql_calls=1930
```

Watching `blocks` and `edges` grow linearly with wall time is the
health signal for the polling loop.

## Pane C — WithdrawalInitiated trigger

Same as `run_session.md`:

```bash
cd /Users/alinat/HALO2_TVM_EXPERIMENTS/bridge/multithreading
python3 research/trigger_loop.py --interval 60
```

## Full-run timing

Same as the single-anchor session: one `--hold-seconds 1800` block
yields roughly 15–20 events; each event stays open for
`observation-window-s` (5 min) after its first path, so records get
emitted with ~5 min lag from event fire. Plan for the session to run
a hold burst *and then* 5+ min of extra collector time before closing
D.

## Post-run analysis

```bash
cd /Users/alinat/HALO2_TVM_EXPERIMENTS/bridge/multithreading
python3 research/multipath_analyzer.py \
  research/stats/dirb-mp-*.jsonl
# Machine-readable version:
python3 research/multipath_analyzer.py \
  research/stats/dirb-mp-*.jsonl --json \
  > research/stats/summary-mp-$(date +%Y%m%d-%H%M).json
```

The digest prints seven sections. The two most load-bearing:

1. **§3 shortest-path length histogram** — the direct empirical
   check of "how expensive is this in circuit?". `p90 ≤ 5` is cheap
   (~40 SHA-256), `p90 ~ 20+` is a concern, `p90 ~ 50+` means the
   parent-chain worst case is regular.
2. **§7 direct vs indirect prefix** — a high `indirect` fraction
   means BFS wins over the direct-hit heuristic. On 2-thread runs
   expect ~100 % direct. On 4-thread runs any non-trivial indirect
   percentage would be a signal that the walker (and eventually
   `bridge-event-witness`) needs the graph-BFS strategy.

## Teardown

Stop panes in reverse: **C → wait ≥ observation-window-s → D → B →
A**. Cutting D before the observation window elapses discards open
events; give the collector time to close in-flight rows first
(SIGINT triggers a best-effort emit but the shorter path may not
have appeared yet).

## If X never leaves thread 0

Expected outcome on the current 2-thread setup. All rows will be
`same_thread_trivial` and §3–7 of the analyzer will be empty. Two
follow-ups worth trying, in order:

1. **Redeploy USDCBridge under a dapp/account_id the split routes off
   thread 0** — see `MULTITHREAD_TEST_SESSION.md`.
2. **Fire events from a cross-thread caller** — `cli.py` already
   funds senders across threads for burst traffic; sending
   `initiateWithdrawal` from one of those senders (instead of a
   freshly deployed msig on thread 0) lets X land on the sender's
   thread.

Both are protocol changes to the test setup, not to the collector.
Leave the collector unchanged.

## Comparison to the single-anchor session

| Aspect                    | Single-anchor (`collector.py`)     | Multi-path (`multipath_collector.py`) |
|---------------------------|-------------------------------------------------|----------------------------------------------------|
| Data structure            | `thread0_view[t]` per-thread ref index          | Full block DAG + reverse-parent index              |
| Path selection            | First `b_seq ≥ x_seq` in `thread0_view[t]`      | BFS on reverse graph, all thread-0 anchors         |
| Handles indirect prefix?  | No                                              | Yes (BFS traverses every ref slot uniformly)       |
| Path enumeration          | One walk per event                              | Up to `--max-paths-per-event` per event            |
| Fetch-on-demand?          | Only for events + gap-fill via `blockByHeight`  | Yes (bounded per-poll for any missing ref target)  |
| Latency captured          | `T_anchor_wall_s`, `T_anchor_blocks_thread_t`   | `T_first_path_wall_s`, `T_best_path_wall_s`        |
| Circuit-cost signal       | `hop_count = 1 + (b_seq − x_seq)`               | `shortest_length` (all edge types counted)         |

Use the single-anchor session when you want the traditional
"same-thread parent walk" cost bound. Use the multi-path session
when you care about **shortest** walks and structural diversity.
