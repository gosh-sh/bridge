# Full reachability research session (multi-hour)

End-to-end recipe: bring up P1..P4, run for hours, collect JSONL, tear down.

## Prereqs checklist

- [ ] `run_local_node.md` preconditions met (Docker VM ≥ 13 GiB, ≥ 20 GB disk free).
- [ ] `tvm-cli`, `tvm-debugger`, `sold` on `$PATH`.
- [ ] `contracts/USDCBridge.keys.json` in acki-nacki matches the vendored copy under
      `research/vendored/contracts/USDCBridge.keys.json`. If not, either overlay the
      vendored copy into `acki-nacki/config/` OR set
      `USDC_BRIDGE_KEY_PATH=$PWD/research/vendored/contracts/USDCBridge.keys.json`
      when invoking P3.
- [ ] `research/stats/` exists and is writable.

## Terminal layout (tmux, 5 panes)

| Pane | Purpose | Command |
|------|---------|---------|
| A    | P1 — node | See [`run_local_node.md`](run_local_node.md) |
| B    | P2 — mt hold | See [`run_mt_test.md`](run_mt_test.md) |
| C    | P3 — event triggers | `python research/trigger_loop.py --interval 60` |
| D    | P4 — collector | `python research/reachability_collector.py --out research/stats/session-$(date +%Y%m%d-%H%M).jsonl` |
| E    | observability | `docker stats` + `tail -f /path/to/node0.log` |

Bring them up in order **A → B → (wait 2 min) → D → C**. Bring D up before C so the
collector snapshots the pre-triggered baseline of existing WithdrawalInitiated events
before P3 starts adding new ones — first-run diff is cleaner.

## Preflight pilot (10 min, first time only)

Before committing hours to a run, verify events actually land on non-zero threads:

```bash
# In pane C:
python research/trigger_loop.py --interval 30 --count 5
# In pane D:
python research/reachability_collector.py --out research/stats/pilot.jsonl
```

Wait for 5 events to be recorded, then:

```bash
jq -r '.event_thread_id' research/stats/pilot.jsonl | sort | uniq -c
```

**Interpretation:**
- All events on `event_thread_id=0` → USDCBridge stayed on parent thread. Stop and
  address per "Preflight pilot" in
  [`../docs/reachability_research_plan.md`](../docs/reachability_research_plan.md).
- ≥ 1 event on a non-zero thread → proceed to full run.

## Full run

Suggested first run: **4 hours, `--interval 60`** → ~240 events, enough sample for
both metrics.

Watch during the run:
- Pane E `docker stats` for RAM headroom (< 90% of Docker VM limit).
- Pane B for `--hold-seconds` remaining — if it expires before your P4 target, restart
  P2 with a bigger `--hold-seconds`.
- Pane D log lines for `outcome=gql_error` rate — spikes indicate node hiccups worth
  investigating.

## Post-run analysis

```bash
# Quick outcome breakdown
jq -r '.outcome' research/stats/session-*.jsonl | sort | uniq -c

# Non-anchorable rate
jq -r '.outcome' research/stats/session-*.jsonl | \
  awk 'BEGIN{ok=0;bad=0} /ok/{ok++} /empty_refs|stuck|too_deep/{bad++} END{printf "non-anchorable = %d / %d = %.3f\n", bad, ok+bad, bad/(ok+bad)}'

# Hop histogram for ok
jq -r 'select(.outcome=="ok") | .hop_count' research/stats/session-*.jsonl | sort | uniq -c
```

For a proper writeup: a small pandas notebook aggregating the four views listed in
[`../docs/reachability_research_plan.md#4-statistics-schema`](../docs/reachability_research_plan.md#4-statistics-schema).

## Teardown

Stop panes in reverse: C → D → B → A. Then `make stop` in acki-nacki.

Keep the JSONL files under `research/stats/` (gitignored). Copy the ones you want to
share into a named subdir with a short README explaining the session parameters
(P2 hold flags, P3 interval, wall-clock duration).
