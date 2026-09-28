# P2 — Michael's multi-thread split test with hold flags

Runs `tests/mt/cli.py test-multithread-cross-thread` in "keep threads alive" mode so
USDCBridge events fired by P3 land on multi-thread blocks with populated
`proof_block_refs`.

Reference: `acki_nacki_mt_test_hold_recipe.md` in `MEMORY.md`.

## Preconditions

- P1 (local node) is up and healthy — see [`run_local_node.md`](run_local_node.md).
- `tvm-cli`, `tvm-debugger`, `sold` are all on `$PATH` (the env-var recipe below
  substitutes explicit paths).

## Command

Run from the acki-nacki repo — **not** from this directory:

```bash
cd /Users/alinat/HALO2_TVM_EXPERIMENTS/acki-nacki

CLI_NAME=tvm-cli \
TVM_CLI=$(which tvm-cli) \
SOLD=$(which sold) \
TVM_DEBUGGER=$(which tvm-debugger) \
ZEROSTATE_HELPER=$(pwd)/scripts/zerostate_helper.sh \
NODE_HELPER=$(pwd)/scripts/node_helper.sh \
python tests/mt/cli.py test-multithread-cross-thread \
  --hold-seconds 1800 \
  --hold-burst-total 5000 \
  --hold-quiet-seconds 10 \
  --timeout 3600
```

## Why each flag

| Flag                         | Reason |
|------------------------------|--------|
| `--hold-seconds 1800`        | Total wall-clock time to hold the split active. Match to the length of your P4 collector run (30 min minimum for meaningful sample). |
| `--hold-burst-total 5000`    | Sustained load-bomb size. Below this the child thread starves — no cross-thread traffic → nothing to measure. |
| `--hold-quiet-seconds 10`    | Idle window between bursts. Too short and bursts back up; too long and the split rebalances back to a single thread. |
| `--timeout 3600`             | Hard upper bound. Extend for longer sessions. |

`CLI_NAME` and the four `*_HELPER`/`*_CLI` env vars bypass the `cli.py:1050`
symlink-resolve bug that shows up when the script tries to auto-discover the tools.

## Verify

While the test runs, in a separate shell:

```bash
curl -s http://localhost:8700/graphql -H 'Content-Type: application/json' \
  -d '{"query":"{ blockchain { blocks(order_by:{seq_no:desc}, limit:10) { edges { node { seq_no thread_id proof_block_refs } } } } }"}' \
  | jq '.data.blockchain.blocks.edges[].node | {seq_no, thread_id, refs_len: (.proof_block_refs | length)}'
```

You want to see:
- At least two distinct `thread_id` values in the last 10 blocks.
- `refs_len > 1` on at least some rows (slot 0 is the same-thread parent — cross-thread
  refs start at slot 1).

If both hold for 60 s of steady state, the split is stable and you can start P3+P4.

## Common failure modes

| Symptom                          | Likely cause | Fix |
|----------------------------------|--------------|-----|
| All blocks stay on `thread_id=0` | Split didn't fire, load too low | Bump `--hold-burst-total`, verify node received the bursts (grep node logs for `MULTITHREAD_LOAD_THRESHOLD`). |
| Finalization stalls after 5 min  | Child thread starving | Increase `--hold-burst-total`, decrease `--hold-quiet-seconds`. |
| `cli.py` panics at line ~1050    | Missing env-var prefix | Copy the exact block above verbatim. |
| Aerospike stop-writes            | Docker VM too small | Bump Docker Desktop RAM to ≥ 13 GiB, restart. |
