# bridge-prover-daemon examples

## `capture_bk_update_fixture.rs`

Captures a live-chain rotation as the fixture `parity_bk_update` checks
(`bridge-prover-lib/tests/fixtures/bk_update_shellnet.json`): the BK set
before the rotation at `--height`, the rotation's delta blob, the rotation
block's 16 Merkle leaves and block id, and the BK set after the rotation.

Both sets are built from GraphQL with `bk_set_history::build_bk_set_with_retry`:
the set before is checked against leaf 2 of the rotation block, the set after
against leaf 3. The delta is then applied to the set before in node order
(`bk_set_replay::{parse_changes, apply_block_changes}`) and must give the set
after, so a broken capture fails here instead of shipping a bad fixture.

Use it to add or refresh a real-chain regression test for rotation
transitions, e.g. after touching `parse_changes`, `normalize_bk_set_pubkeys`
or the 16-leaf handling.

The endpoint must serve `blockchain.latestBkSet` (gql-server >= 1.5.0) and
still hold the `bkSetUpdates` rows between its snapshot and `--height`.

## Usage

```sh
BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql \
    cargo run --release --example capture_bk_update_fixture -- \
    --height 2584711 \
    --out crates/bridge-prover-libraries/bridge-prover-lib/tests/fixtures/bk_update_shellnet.json
```

`--height` is the seq_no of the block that emits the rotation, not the block
before it.
