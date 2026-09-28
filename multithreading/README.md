# Multithreading — cross-thread reachability research

This directory holds the plan, runbooks, vendored helpers, and research scripts used to
empirically measure how often a `WithdrawalInitiated` event fires on an acki-nacki block
that has **no reachable thread-0 ancestor via `proof_block_refs`** — the failure mode
Direction (a) walks worry about in
[`DRAFT_cross_thread_reachability_issue.md`](DRAFT_cross_thread_reachability_issue.md).

The goal is *data, not implementation*: a JSONL trace of per-event outcomes that answers
two headline questions:

1. **Non-anchorable rate** — how often does `X.refs` fail to reach thread 0?
2. **Hop distribution** — for the walks that succeed, how many hops did they take?

## Layout

```
multithreading/
├── DRAFT_cross_thread_reachability_issue.md     — the problem statement
├── README.md                                    — this file
├── docs/
│   └── reachability_research_plan.md            — full plan (P1..P4)
├── runbooks/
│   ├── run_local_node.md                        — P1: local acki-nacki devnet
│   ├── run_mt_test.md                           — P2: Michael's thread-split test
│   └── run_reachability_session.md              — the whole tmux/5-pane recipe
└── research/
    ├── vendored/                                — copies of upstream python/ (see SYNC_FROM.md)
    ├── trigger_loop.py                          — P3: periodic event firing
    ├── reachability_collector.py                — P4: refs-only ref-walk + JSONL stats
    └── stats/                                   — JSONL output (gitignored)
```

## Start here

1. Read [`docs/reachability_research_plan.md`](docs/reachability_research_plan.md).
2. Follow [`runbooks/run_reachability_session.md`](runbooks/run_reachability_session.md)
   for the end-to-end multi-hour session.
