# E2E shellnet run state — 2026-08-14

Snapshot for resuming the horizontal-chain E2E validation. Daemons stopped
cleanly after two consecutive bundle proofs verified OK; withdrawal event
proving was not yet attempted.

## What was being validated

Fix from prior session in `bridge-event-witness/src/bin/build.rs`: build.rs now
supports a horizontal chain of L1 openings when `key_block_seq !=
thinned_key_block_seq`. Circuit 2 dispatches Climb / Forward (pos 1) / Descend
via `verify_chain_of_dense_proofs` (MAX_CHAIN_LEN=11).

The rerun observed both dispatch arms live on shellnet:
- bundle 7986688: `chain_steps=4`, `chain_pos=1` → BOTH VERIFIED OK
- bundle 7987200: BOTH VERIFIED OK

## Fix applied this session

`bridge-verifier-daemon/src/main.rs` (~line 193 / ~225): removed the local
`bootstrapped` snapshot that captured `state.initialized` at boot and was
never resynced. Race was:
1. prover cold-boots first, proves bundle N, sends to verifier
2. verifier `append_bundle` flips `state.initialized = true`
3. prover writes `state/bootstrap_seed.json` (only happens after first bundle
   succeeds — see `persist_seed_if_needed`, `bridge-prover-daemon/src/main.rs:323`)
4. next verifier tick: local `bootstrapped` still `false`, so it re-reads seed
   and calls `initialize_bk_set_commitment` which panics (`ensure!(!self.initialized)`)

Guard is now `if !state.initialized` directly. See inline comment in the fixed
block.

## Known non-fatal bug (not fixed)

`bridge-gql-fetcher/src/gql_client.rs:311` — `query_bk_set_updates_paged`
passes `u64::MAX` as GraphQL `height_end`, but shellnet schema types it as
`Int` (i32). Every poll logs:
```
WARN bk-update drain: next_update_after failed (next_update_after page 0);
     returning None so caller retries
WARN pending_bk_update_below: next_update_after failed (...), assuming no pending update
```
Errors are swallowed in `live_driver::bk_update` and `pending_bk_update_below`.
Non-blocking on shellnet: 5-signer BK set is static, no rotations pending.
Fix: clamp height_end to `i32::MAX` (or drop the arg when unset).

## Files in state/ at stop time

```
bootstrap_seed.json    # written after bundle 7986688 succeeded
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
   and confirm `state/bootstrap_seed.json` exists — this is when withdrawal
   event proving can safely run.
4. Trigger a live withdrawal and prove it end-to-end:
   ```
   MODE=shellnet python3 python/generate_withdrawals_with_live_event_proving.py
   ```
5. Stop with `./scripts/stop-bridge-test.sh` (SIGINT → SIGKILL after 30s).

## Environment

- Deploy #6 (2026-08-13) bridge: `0x7769aaa09e5cB68EF3e0FE95D2E73A6644e41435`
- `BRIDGE_BOOTSTRAP_SEQNO=7618560` (unchanged; auto-pin took over at 7986176)
- Shellnet 5-signer BK set commitment: `1ca73c29be5f36c998399071c33704bf952f3269ff8c5c8b5af7e492180aeb08`

## Verifier summary at stop

```
total time:             834.9s
total proofs received:  2
both verified OK:       2
event proofs received:  0
```
