# Multithreading — cross-thread reachability research

This directory holds the plan, runbooks, and research scripts used to empirically
measure the walk `Y (thread 0, on-chain-anchored) → … → X (event block on a
non-default thread)` for `WithdrawalInitiated` events — the shape a
`BridgeMultiHopProof` SNARK will have to prove.

The soundness argument for starting at Y (not at X) is in
[`DRAFT_cross_thread_reachability_issue.md`](DRAFT_cross_thread_reachability_issue.md) §2.1.

The goal is *data, not implementation*: JSONL traces the analyzer digests into
two headline metrics — thread distribution of X, and hop distribution of the
successful walks.

## Layout

```
multithreading/
├── DRAFT_cross_thread_reachability_issue.md     — the problem statement (retracts Direction (a))
├── README.md                                    — this file
├── docs/
│   ├── direction_b_research_plan.md             — full plan (supersedes reachability plan)
│   ├── direction_b_walker_algorithm.md          — single-anchor collector algorithm
│   └── direction_b_multipath_algorithm.md       — multi-path collector algorithm + math
├── runbooks/
│   ├── run_local_node.md                        — local acki-nacki devnet
│   ├── run_direction_b_session.md               — single-anchor session recipe
│   └── run_direction_b_multipath_session.md     — multi-path session recipe (preferred)
└── research/
    ├── vendored/                                — copies of upstream python/ (see SYNC_FROM.md)
    ├── trigger_loop.py                          — periodic WithdrawalInitiated firing
    ├── direction_b_smart_trigger.py             — smarter cross-thread trigger variant
    ├── direction_b_collector.py                 — single-anchor walker + JSONL stats
    ├── direction_b_multipath_collector.py       — BFS multi-path walker + JSONL stats
    ├── direction_b_analyzer.py                  — single-anchor digest
    ├── direction_b_multipath_analyzer.py        — multi-path digest
    ├── keepalive_load_driver.py                 — driver for keeping the split alive between fans
    ├── thread_liveness_monitor.py               — samples GraphQL for active thread set
    └── stats/                                   — JSONL output (gitignored)
```

## Start here

1. Read [`docs/direction_b_research_plan.md`](docs/direction_b_research_plan.md).
2. Follow [`runbooks/run_direction_b_multipath_session.md`](runbooks/run_direction_b_multipath_session.md)
   for the end-to-end session — its Pane B block has the correct `cli.py` incantation.
