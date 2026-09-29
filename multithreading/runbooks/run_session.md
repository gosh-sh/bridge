# Direction (b) reachability session

End-to-end recipe for the split-thread devnet run that measures where
`WithdrawalInitiated` events land and, for the ones that don't land on thread 0,
what path a Direction (b) prover would walk `Y (thread 0) → B (thread t) → …
→ X (event block)`. Model + math live in
[`../docs/walker_algorithm.md`](../docs/walker_algorithm.md).

## What we're measuring

For every event fired during the session, one JSONL row with:

- Where `X` landed: `x_thread`, `x_seq`, `x_block_id`.
- If `x_thread == thread 0`: outcome = `same_thread_trivial`, hop_count = 0.
- If `x_thread != thread 0`: first future thread-0 block `Y` whose `Y.refs[i≥1]`
  points at some `B` in `x_thread` with `b_seq ≥ x_seq`, plus the same-thread
  parent chain `B → parent(B) → … → X`. Outcome = `ok`, hop_count =
  `1 + (b_seq − x_seq)`.
- Anchor delay: `T_anchor_wall_s` (seconds) + `T_anchor_blocks_thread_t`
  (`b_seq − x_seq`). The latter is the protocol-soft-capped quantity
  (≤ 50 per `cross_thread_ref_enforcement/mod.rs:56`).
- Failure buckets: `orphan_timeout`, `parent_walk_failed`,
  `event_block_unresolved`.

**Expectation before running:** USDCBridge lives at account_id `1a1a…1a1a`
under `DEFAULT_DAPP_ID = 0x00…00`. In the current setup that address routes
to thread 0; so most (all?) events will land on thread 0 and produce
`same_thread_trivial`. This session verifies that empirically and, whenever
an event *does* land on a child thread, captures the full Direction (b) walk.

## Prerequisites

| # | Item |
|---|------|
| 1 | Docker Desktop VM ≥ 12 GiB (per `acki_nacki_mt_test_hold_recipe.md`). |
| 2 | acki-nacki checkout at `$ACKI_NACKI_ROOT`, branch **`feature/node-3953-add-test-slow-block-builder-with-300ms-per-block-build-on`**. |
| 3 | Node built and healthy per [`../../../run_acki_nacki_node.md`](../../../run_acki_nacki_node.md): `cargo clean && cargo update && make generate_zerostate && make run`, then verify all containers healthy. |
| 4 | State-v2-compatible tooling at `/Volumes/x5/v2_tools/` (`tvm-cli`, `sold`, `tvm-debugger`) and `/Volumes/x5/cargo-target/release/{zerostate-helper, node-helper}`. |
| 5 | `research/stats/` writable. |

## Pane layout (tmux, 5 panes)

Bring panes up **A → B → (wait 2 min for split to stabilize) → D → C**. D
before C so the collector captures the baseline of pre-existing events cleanly.

| Pane | Role | See |
|------|------|-----|
| A    | Local acki-nacki node | [`../../../run_acki_nacki_node.md`](../../../run_acki_nacki_node.md) |
| B    | Split-thread cli.py test (below) | This file |
| C    | WithdrawalInitiated trigger loop | Existing `trigger_loop.py` |
| D    | Direction (b) collector (`collector.py`) | This file |
| E    | Observability (`docker stats`, node logs) | — |

## Pane B — split-thread test

Run from the acki-nacki repo root, **not** from this directory. The env-var
prefix bypasses the auto-discovery bug at `tests/mt/cli.py:1050`.

```bash
cd $ACKI_NACKI_ROOT   # branch feature/node-3953-add-test-slow-block-builder-with-300ms-per-block-build-on

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
  -d '{"query":"{ blockchain { blocks(last: 10) { edges { node { seq_no thread_id proof_block_refs } } } } }"}' \
  | jq '.data.blockchain.blocks.edges[].node | {seq_no, thread_id: (.thread_id[0:12]+"…"), refs_len: (.proof_block_refs | length)}'
```

Want to see: at least two distinct `thread_id` values in the last 10 blocks
AND `refs_len ≥ 2` (slot 0 parent + at least one cross-thread ref) on
thread-0 rows.

## Pane D — collector

```bash
cd /Users/alinat/HALO2_TVM_EXPERIMENTS/bridge/multithreading
python3 research/collector.py \
  --graphql http://localhost/graphql \
  --out research/stats/dirb-$(date +%Y%m%d-%H%M).jsonl \
  --poll-interval 2.0 \
  --max-wait-anchor-s 600 \
  --max-parent-hops 128 \
  --scan-window 50 \
  --heartbeat-s 30
```

Notes:
- `--scan-window 50` pulls the last 50 blocks per poll and filters to thread 0.
  Bump if the devnet block rate outruns 25 blocks/sec.
- `--max-wait-anchor-s 600` = 10 min. Protocol ceiling is ≤50 finalized blocks
  of thread t after X, which at ~300 ms/block is ~15 s worst-case-normal;
  600 s leaves generous margin for stalls.
- `--max-parent-hops 128` caps the walk. Aggregator practical limit hasn't
  been sized yet; 128 is well above any expected walk under enforcement.

## Pane C — WithdrawalInitiated trigger

The existing `trigger_loop.py` fires one event per iteration by spawning a
fresh multisig, minting ECC[3] via `USDCBridge.mintAndSend`, and calling
`initiateWithdrawal`.

```bash
cd /Users/alinat/HALO2_TVM_EXPERIMENTS/bridge/multithreading
# Once every 60 s. Adjust to control event rate.
python3 research/trigger_loop.py --interval 60
```

Env pass-throughs (edit if the vendored helper needs them):

- `ACKI_NACKI_ROOT` — points at the acki-nacki checkout (needed by
  `helper/common.py` to locate `config/USDCBridge.keys.json`).
- `USDC_BRIDGE_KEY_PATH` — set to
  `research/vendored/contracts/USDCBridge.keys.json` if the acki-nacki
  `config/` copy has drifted (see `bridge_python_orchestrator_usdc_keys.md`
  in memory).
- `NETWORK=http://127.0.0.1:80` and `GRAPHQL_URL=http://localhost/graphql`
  are the collector defaults.

## Full-run timing

The `cli.py` command holds for `--hold-seconds 1800` (30 min). One
`trigger_loop.py` iteration is ~90–150 s on this devnet (msig deploy + mint +
initiateWithdrawal + event confirmation), so a single 30-min hold yields
roughly 15–20 events. Repeat the hold burst (or bump `--hold-seconds`) to
grow the sample.

## Post-run analysis

```bash
cd /Users/alinat/HALO2_TVM_EXPERIMENTS/bridge/multithreading
python3 research/analyzer.py research/stats/dirb-*.jsonl
# Machine-readable version:
python3 research/analyzer.py research/stats/dirb-*.jsonl --json > research/stats/summary-$(date +%Y%m%d-%H%M).json
```

The digest prints five sections; the two we care about most:

1. **§2 thread distribution.** If everything lands on thread 0, the current
   deployment routes USDCBridge exclusively to the parent thread and there
   is no Direction (b) walk to characterize. If ≥ 1 event lands elsewhere,
   sections 3–4 quantify how far we'd have to walk to anchor it.
2. **§4 anchor delay.** `T_anchor_blocks` histogram is the direct empirical
   check of the ~50 protocol ceiling. Anything above 10 in the p90 column
   suggests the split producer stalled and only advanced its thread-0 view
   under enforcement pressure, not through the ordinary checkpoint stride.

## Teardown

Stop panes in reverse: C → D → B → A. `make stop` in acki-nacki. Keep
`research/stats/*.jsonl` and the summary JSON; move interesting sessions
into a named subdir with a README describing the flags.

## If X never leaves thread 0

Expected outcome given the current USDCBridge deployment. Two follow-ups
worth trying, in order:

1. **Redeploy USDCBridge (or a proxy) under a dapp/account-id that the
   split routes to a child thread.** Requires knowing the split point
   `cli.py` picks; see `MULTITHREAD_TEST_SESSION.md` for how the harness
   partitions dapp ids across threads.
2. **Fire events from a cross-thread caller.** The `cli.py` test already
   funds sender accounts spread across threads for its own burst traffic;
   sending `initiateWithdrawal` from one of those senders (instead of a
   freshly deployed msig on thread 0) would let X land on the sender's
   thread.

Both are protocol changes to the test setup, not to the collector. Leave the
collector unchanged.
