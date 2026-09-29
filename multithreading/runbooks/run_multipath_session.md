# Multi-path reachability session

End-to-end recipe for the split-thread devnet run that measures, per
`WithdrawalInitiated` event, **every path** `Y (thread 0) ◀── … ◀── X
(event block)` observed within a 5-minute observation window — with the
shortest one ranked first. Model + math live in
[`../docs/multipath_algorithm.md`](../docs/multipath_algorithm.md).

## One-command entry point (recommended)

```bash
cd bridge/multithreading
./runbooks/run_multipath.sh
```

The script orchestrates the whole session in one blocking foreground
process (see the header comment in `run_multipath.sh` for the exact
step-by-step). All outputs land under `research/stats/` with a shared
`$TS` suffix — session log, monitor JSONL, cli.py log, collector
JSONL, events JSONL, analyzer summary JSON.

### Setup on a fresh machine (n14 / Linux)

1. **Clone the two repos as siblings.** The wrapper expects
   `acki-nacki` next to `bridge/` (or set `ACKI_NACKI_DIR` to override):

   ```
   ~/work/
     ├── bridge/                ← this repo
     └── acki-nacki/            ← branch feature/node-3953-add-test-slow-block-builder-with-300ms-per-block-build-on
   ```

2. **Populate `bridge/multithreading/bins_<OS>/`** — the wrapper
   auto-detects: `bins_macOS/` on Darwin, `bins_linux/` on Linux. See
   [`../bins_macOS/README.md`](../bins_macOS/README.md) for the five
   binaries (`tvm-cli`, `sold`, `tvm-debugger`, `zerostate-helper`,
   `node-helper`) and how to populate them from a local `acki-nacki`
   checkout. For a Linux box, populate `bins_linux/` the same way from
   Linux release assets or a Linux `cargo build --release`.

3. **System deps:** `python3` (≥ 3.10), `docker` (Compose v2), `jq`,
   `curl`, `make`, `git-lfs`. Docker VM ≥ 13 GiB RAM, ≥ 20 GB disk.

4. **Run:**

   ```bash
   cd bridge/multithreading
   ./runbooks/run_multipath.sh
   ```

   Or with overrides:

   ```bash
   ACKI_NACKI_DIR=/opt/acki-nacki \
   TOOLS_DIR=/opt/bin \
   NODE_CONTAINER=my-node0 \
       ./runbooks/run_multipath.sh
   ```

The wrapper is idempotent on node bringup — if `docker ps` already
shows a healthy node0 (GQL answering), it skips `make run` and moves
straight to the test.

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
  walk went direct (Y ◀── … ◀── X, all hops staying in `x_thread`)
  or indirect (through another thread).
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
`Y ◀── t' ◀── t` indirect prefix may strictly beat the direct 50-hop
parent walk. Run this recipe the same way when the 4-thread test becomes
available; no collector changes needed.

## Prerequisites

| # | Item |
|---|------|
| 1 | Docker Desktop VM ≥ 12 GiB. |
| 2 | acki-nacki checkout at `$ACKI_NACKI_ROOT`, branch **`feature/node-3953-add-test-slow-block-builder-with-300ms-per-block-build-on`**. |
| 3 | Node built and healthy per [`./run_local_node.md`](./run_local_node.md). |
| 4 | State-v2-compatible tool binaries populated under `bridge/multithreading/bins_<OS>/` (see [`../bins_macOS/README.md`](../bins_macOS/README.md)). |
| 5 | `research/stats/` writable. |

## Pane layout (tmux, 5 panes)

Order **A → B → (wait 2 min for split to stabilize) → E → D → C**. E
(thread-liveness monitor) before D so a wedged child thread is caught
inside 30 s — the moment the extra thread dies, every subsequent event
falls into `same_thread_trivial` and the collector produces nothing
interesting; there is no point running the trigger loop until E shows
both threads `LIVE`. D before C so the collector has already primed
the graph before the first event fires.

| Pane | Role                                                       | See |
|------|------------------------------------------------------------|-----|
| A    | Local acki-nacki node                                      | [`./run_local_node.md`](./run_local_node.md) |
| B    | Split-thread `cli.py` test                                 | This file |
| C    | WithdrawalInitiated trigger (`smart_trigger.py`)           | This file |
| D    | **Multi-path collector** (`multipath_collector.py`) | This file |
| E    | **Thread-liveness monitor** (`thread_liveness_monitor.py`) | This file |

## Pane B — split-thread test

Source `bins_<OS>/env.sh` first — it exports the six tool paths
(`CLI_NAME`, `TVM_CLI`, `SOLD`, `TVM_DEBUGGER`, `ZEROSTATE_HELPER`,
`NODE_HELPER`) plus `DISABLE_MV=true` from the binaries you populated
in that directory (per Prereq #4). Exporting these before invoking
`cli.py` also bypasses the auto-discovery bug at
`tests/mt/cli.py:1050`, which otherwise walks symlinks into
`tvm-sdk/target/release/` where `zerostate-helper` doesn't exist.

Then run the test from the acki-nacki repo root:

```bash
# From bridge/ root:
source multithreading/bins_macOS/env.sh    # or bins_linux/env.sh on Linux
export MESSAGE_ARCHIVE_OTEL_RUN_ID="local-2-thread-$(date +%Y%m%d-%H%M)"

cd "$ACKI_NACKI_ROOT"   # branch feature/node-3953-...
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

If binaries live outside the repo (e.g. an external volume, a shared
tools shelf, or a colleague's setup), either symlink them into
`bins_<OS>/`, or export the six env vars by hand pointing wherever
they live — `cli.py` only cares about the resolved absolute paths.

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

## Pane E — thread-liveness monitor

`cli.py` reports "fan didn't advance" only after the fan timeout expires
(tens of minutes). By then the child thread has already been dead for a
long time and every event we fired since is in thread 0 → useless. The
monitor scrapes `docker logs` for two signatures the node emits every
few seconds:

- `Incoming block candidate: … thread=ThreadIdentifier<HEX>` — a
  candidate arrived from thread `HEX` → thread is producing.
- `pulse_stall … thread=<T:HEX>` — that thread's producer missed its
  chain-pulse deadline. Persistent stalls with zero candidates = wedged.

Run *inside* the acki-nacki checkout (the monitor exec's `docker logs`
against the compose project running there):

```bash
cd "${ACKI_NACKI_DIR:-../../../acki-nacki}"
python3 "$OLDPWD/../multithreading/research/thread_liveness_monitor.py" \
  --interval 5 \
  --out "$OLDPWD/../multithreading/research/stats/thread-mon-$(date +%Y%m%d-%H%M).jsonl"
```

Each sample line looks like:

```
[2026-…] t0=LIVE cand=12 stall=0 seq=9421 | t1a2b=LIVE cand=8 stall=0 seq=1102
```

**Decision rule.**

- Both threads `LIVE` with `cand > 0` → proceed to Pane D.
- Any child thread `STALLED` (`cand=0, stall>0`) or `IDLE` after 90 s of
  hold-mode → **abort the run**. Kill Pane B, tear down A, restart. The
  session is not going to produce cross-thread events.
- Child thread flips `LIVE → STALLED` mid-run → stop firing triggers
  (kill C) and note the wall-clock in the run log; events fired after
  that point will be `same_thread_trivial`.

Keep E running for the full session — sampling is cheap (one `docker
logs --since` per interval).

## Pane D — multi-path collector

```bash
cd ../multithreading
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

`smart_trigger.py` fires one event per iteration by spawning a fresh
multisig, minting ECC[3] via `USDCBridge.mintAndSend`, and calling
`initiateWithdrawal` — then paces the *next* fire based on where X
landed:

- X on thread 0 → sleep `--same-thread-pause-s` (default 15 s). Nothing
  to walk, no reason to burn the collector's 5-min observation window.
- X on any other thread → sleep the full `--observation-window-s` so D
  can enumerate every alternate path before we perturb the ref-DAG with
  a fresh event.

Match `--observation-window-s` to the same value Pane D uses (default
300 s) so the two are in phase.

```bash
cd ../multithreading    # or wherever this repo lives
python3 research/smart_trigger.py \
  --count 12 \
  --graphql http://localhost/graphql \
  --observation-window-s 300 \
  --same-thread-pause-s 15 \
  --collector-out research/stats/dirb-mp-YYYYMMDD-HHMM.jsonl \
  --events-out    research/stats/events-$(date +%Y%m%d-%H%M).jsonl
```

Point `--collector-out` at the *same* JSONL file Pane D writes to;
`smart_trigger.py` reads it at the end of the run to join per-event
observations with the collector's shortest-path records. `--events-out`
gets its own per-event digest (fire wall-clock, X thread, `dst_msg_id`,
carrier block).

Env pass-throughs (edit if the vendored helper needs them):

- `ACKI_NACKI_ROOT` — points at the acki-nacki checkout (needed by
  `helper/common.py` to locate `config/USDCBridge.keys.json`).
- `USDC_BRIDGE_KEY_PATH` — set to
  `research/vendored/contracts/USDCBridge.keys.json` if the acki-nacki
  `config/USDCBridge.keys.json` has drifted from the vendored copy the
  helpers ship with. Symptom of drift: `USDCBridge.mintAndSend` fails
  with TVM exit code 209 on a fresh local devnet. Diagnose by
  diffing the two JSON files byte-for-byte; the file the orchestrator
  actually uses is the one at this env var (or the acki-nacki `config/`
  copy if unset).
- `NETWORK=http://127.0.0.1:80` and `GRAPHQL_URL=http://localhost/graphql`
  are the collector defaults.

## Full-run timing

One `--hold-seconds 1800` block yields roughly 15–20 events; each event stays open for
`observation-window-s` (5 min) after its first path, so records get
emitted with ~5 min lag from event fire. Plan for the session to run
a hold burst *and then* 5+ min of extra collector time before closing
D.

## Post-run analysis

```bash
cd ../multithreading
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

Stop panes in reverse: **C → wait ≥ observation-window-s → D → E → B →
A**. Cutting D before the observation window elapses discards open
events; give the collector time to close in-flight rows first
(SIGINT triggers a best-effort emit but the shorter path may not
have appeared yet). E stays up until B is stopped so the monitor
still captures whatever the node emits during shutdown.

## If X never leaves thread 0

Expected outcome on the current 2-thread setup. All rows will be
`same_thread_trivial` and §3–7 of the analyzer will be empty. Two
follow-ups worth trying, in order:

1. **Redeploy USDCBridge under a dapp/account_id the split routes off
   thread 0.** The current bridge account uses `DEFAULT_DAPP_ID` and
   an account_id of `1a1a…1a1a`, and the 2-thread split routes both
   to thread 0. To land the contract on another thread, deploy it
   under a different `dapp_id`/`account_id` combination and verify
   with the GraphQL query in Pane B (the account's landing block
   should have a non-zero `thread_id`). Then re-point Pane C's
   `smart_trigger.py` at the new bridge address.
2. **Fire events from a cross-thread caller** — `cli.py` already
   funds senders across threads for burst traffic; sending
   `initiateWithdrawal` from one of those senders (instead of a
   freshly deployed msig on thread 0) lets X land on the sender's
   thread.

Both are protocol changes to the test setup, not to the collector.
Leave the collector unchanged.

## Why this recipe (params analysis)

**Why not default flags.** `cli.py test-multithread-cross-thread`'s
default `--hold-seconds 0` blocks on the initial cross-thread drain:
the harness forces a 1→2 thread split via a same-DApp warmup burst,
but with no sustaining load the child thread's last block never gets
a BK quorum of attestations, stays *prefinalized*, and
`authority_switch` refuses to open round 0 → dead thread →
cross-thread messages queue up but never get delivered → the test
times out. The cyclic-hold path (both `--hold-seconds` and
`--hold-burst-total` set — see `tests/mt/cli.py:3046` in the
acki-nacki checkout) keeps sustained traffic on both threads so the
split holds.

Chosen values:

- `--threads 2 --total 20000` — 20 k funded senders across 2 threads.
- `--hold-seconds 1800` — 30 min cyclic-hold window.
- `--hold-burst-total 5000 --hold-quiet-seconds 0 --batch-size 200` —
  keeps 5 000 tx in flight per cycle with no quiet period; cli.py's
  `_wait_for_active_thread_count` self-heals if the split retracts.
- `--deploy-value 12e12 --minimum-balance 8e12` — deploy budget + top-up
  threshold sized so senders don't run out mid-burst.
- `--timeout 2400` — 40 min hard ceiling, 10 min buffer beyond the hold.

Observed clean on a mac (Docker VM 12 GiB, 5 nodes) on 2026-09-18:
27 clean hold cycles, ~180 msg/s per direction, ~140 k msgs
delivered per receiver over ~24 min; then a client-side back-pressure
crash at cycle ~28 from the hard-coded 20 k `SINGLE_THREAD_LOAD_THRESHOLD`
refill inside `keep_split_load_active` (harness bug, not a network
failure — the split itself stayed healthy the whole time). n14 is a
Linux box with more headroom than the mac, so the same values should
ride even easier — `pulse_stall` overshoot is the usual failure
mode, and it's CPU-contention driven.

Even so, the wrapper runs `thread_liveness_monitor.py` in-band and a
watchdog polls it every 15 s: if any child thread flips to
STALLED/IDLE after a 90 s grace, the trigger is SIGTERM'd and the run
aborts with an explicit ABORTED banner. That is defence-in-depth over
the cli.py's own health check, whose fan-timeout only fires after
tens of minutes — well after events start falling into thread 0.

The `smart_trigger.py --count 12` value assumes the current
2-thread + `DEFAULT_DAPP_ID` USDCBridge setup routes ~100 % of events
to thread 0. Each same-thread event costs ~60 s (15 s pause +
resolve), so 12 events ≈ 12 min — well inside the 30 min hold. If a
future setup routes many events off thread 0 (each cross-thread event
consumes the full 300 s observation window), cut `--count` to 5; the
wrapper has a comment marking the exact spot.
