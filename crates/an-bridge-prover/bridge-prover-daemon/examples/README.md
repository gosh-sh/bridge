# bridge-prover-daemon examples

Two tools that share the same core primitive (`bk_set_at_height`, folding
`bkSetUpdates` from a genesis snapshot) but produce different artifacts for
different consumers.

| | `seed_prover_bk_set.rs` | `capture_bk_update_fixture.rs` |
|---|---|---|
| Produces | Runtime state file for the daemon | Test fixture for `parity_bk_update` |
| Says | "BK set as of height N" | "BK set before N, delta at N, BK set after N" |
| Height flag | `--cap-height N` | `--height N` (rotation block) |
| Fold cutoff | `<= N` (state already includes update N) | `< N`, then delta at N applied separately |
| Extra fetches | none | target block's 16 Merkle leaves + `bkSetUpdates` event + block_id cross-check |
| Uses `bridge_prover_lib` | yes (`ProverBkSet::from_pubkeys`) | no |
| Default out path | `state/prover_bk_set.json` | `tests/fixtures/bk_update_shellnet.json` |
| Genesis source | `--genesis` or env `BRIDGE_BK_SET_CONFIG` | `--genesis-bk-set` (required) |

## Why the fold cutoff differs

Both answer different questions.

- **Seeder** folds `<= N` so the state file already includes update N; the
  daemon resumes at N+1.
- **Capture** folds `< N` to get `old_pubkeys` (the input to rotation N),
  then applies the delta at N separately to derive `expected_new_pubkeys`.
  If it folded `<= N` like the seeder, old and new would be identical and
  the fixture would be tautological.

## When to use which

- Standing up a daemon without walking the whole chain → `seed_prover_bk_set`.
- Adding / refreshing a real-chain regression test for rotation transitions
  (e.g. after touching `parse_bk_set_changes_pub`, `normalize_bk_set_pubkeys`,
  or the 16-leaf handling) → `capture_bk_update_fixture`.

## Usage

```sh
# Seed daemon state at height N
BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql \
BRIDGE_BK_SET_CONFIG=state/genesis_bk_set.json \
    cargo run --release --example seed_prover_bk_set -- \
    --cap-height 2608018 --out state/prover_bk_set.json

# Capture a rotation fixture at height N
BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql \
    cargo run --release --example capture_bk_update_fixture -- \
    --height 2584711 \
    --genesis-bk-set path/to/bk_set.shellnet.json \
    --out crates/an-bridge-prover/bridge-prover-lib/tests/fixtures/bk_update_shellnet.json
```

## Gotcha

`capture_bk_update_fixture` calls `query_bk_set_updates_paged(N, 500, None)`
and reads only page 1. If the target rotation isn't in the first 500 events
returned, it silently bails with "no bkSetUpdates event at height N" —
switch to full pagination if you hit this on a rotation-heavy chain.
