# E2E shellnet run state — 2026-08-15

Snapshot for resuming the live event-anchoring E2E validation. First
end-to-end run that produced **and daemon-verified** a Circuit 4 event
proof against shellnet.

## What was validated

Phase 1 + Phase 2 of the L1/L2 event-anchoring plan:

* `bridge-prover-lib::real_chain_builder::build_event_anchor_chain` — new
  composed entry point that dispatches on `target_layer`:
  * L1 → horizontal walk `H_e → K`, up to `W·P − 1` hops
  * L2 → single vertical L1→L2 rung to `T_2`
  * Internals (`build_layer_n_leaves`, `build_layer_n_tree`) stay private.
* `bridge-event-witness-builder` — new `--anchor-layer <1|2>` flag,
  default 1. Legacy `--layer-idx` (0-indexed) still accepted for the
  Python driver; conflicts are rejected.

Deferred to follow-ups: Phase 3 (wait-time warning + opt-in), Phase 4
(auto-escalation for old events), Phase 5 (unit tests for
`l_n_covering_boundary` + integration test for L2 anchoring).

## Run summary

Wall-clock 12:34 end-to-end.

* Withdrawal fired at block seq_no=**8251308**, message hash
  `fadbee42e2cc586e6eaf1bbb000b0da9857b0750c3845beff5e5cc3607cc9080`.
* Boundary math: `H_e = K = 8251392`, `hops = 0` (event landed on a
  W·P-aligned tree — trivial L1 anchor).
* Verifier reached anchor at T+09:37 after 3 bundles from the fresh seed:
  8249344 → 8249856 → 8250368 → 8250880 → 8251392.
* Circuit 4 proof produced (Step 7) and daemon-verified at height 8251392.
* Layer hash committed in public instances:
  `ca345b8e3aa11ed4b130c5fd640ac215447e2f04fa6c3d9851ecc28cb373fb1f`.

L2 dispatch path is code-path-audited but **not yet exercised on live
data** — this run took L1 (num_active_chain_steps=0).

## Config change carried forward

`.env.shellnet` (gitignored) — `BRIDGE_BOOTSTRAP_SEQNO` commented out:

```
# 2026-08-15: unset → daemon auto-picks a fresh W·P boundary past current head.
# Prior pin (7618560) went stale after 8 days; verifier could not catch up.
# BRIDGE_BOOTSTRAP_SEQNO=7618560
```

Rationale: on shellnet, a static pin decays quickly. Once the chain has
moved more than a few `W·P` boundaries past the pin, `bridge-prover-daemon`
must catch up bundle-by-bundle at ~3–5 min each — infeasible past a day or
two. Auto-seed pins at the next `W·P` boundary past current head, so
first-bundle wall-clock is ≤ `W·P × block_time` ≈ 2 min.

If the on-chain Sepolia bridge is later configured with a specific
`genesisLastSeenBlockSeqNo`, restore the pin so daemon state matches the
contract anchor.

## Known non-fatal bug (unchanged from 2026-08-14)

`bridge-gql-fetcher/src/gql_client.rs:311` — `query_bk_set_updates_paged`
passes `u64::MAX` as GraphQL `height_end`, but shellnet schema types it as
`Int` (i32). Every poll logs WARN spam; errors are swallowed in
`live_driver::bk_update` and `pending_bk_update_below`. Non-blocking on
shellnet (5-signer BK set is static). Fix: clamp `height_end` to
`i32::MAX` (or drop the arg when unset).

## Files in state/ after the run

```
bootstrap_seed.json    # auto-seeded at 8249344
prover_bk_set.json
prover_state.json
verifier_state.json
```

## How to resume

1. Rebuild binaries if any code changed since:
   ```
   cargo build --release -p bridge-prover-daemon -p bridge-verifier-daemon \
                          -p bridge-event-witness -p bridge-event-halo2-prover
   ```
2. Start both daemons (wipes proofs/state/logs, keeps params/):
   ```
   set -a && source .env.shellnet && set +a
   ./scripts/run-bridge-test.sh
   ```
3. Wait for at least one `BOTH VERIFIED OK` line in `logs/verifier_output.log`
   and confirm `state/bootstrap_seed.json` exists (~2 min from cold start
   with auto-seed).
4. Trigger a live withdrawal and prove it end-to-end:
   ```
   MODE=shellnet python3 python/generate_withdrawals_with_live_event_proving.py
   ```
5. Stop with `./scripts/stop-bridge-test.sh` (SIGINT → SIGKILL after 30s).

To exercise the L2 dispatch path, either wait for an event whose L1 slot
has rolled out of the window (`W·P` blocks ≈ 3.5 min at shellnet cadence),
or add `--anchor-layer 2` to the witness-builder invocation in the Python
driver.
