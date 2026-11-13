# bk-update parity fixtures

Drop a captured shellnet fixture here as `bk_update_shellnet.json` to enable
[`../parity_bk_update.rs`]. Without a fixture the test is a no-op (prints a
skip hint with the capture command).

Capture with `bridge-prover-daemon/examples/capture_bk_update_fixture.rs`:

```sh
BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql \
  cargo run --release --example capture_bk_update_fixture -- \
    --height <bk-update block seqno> \
    --out crates/bridge-prover-libraries/bridge-prover-lib/tests/fixtures/bk_update_shellnet.json
```

`--height` must be the seq_no of a block that emits a `bkSetUpdates` event
(look at chain history). The endpoint must serve `blockchain.latestBkSet`
(gql-server >= 1.5.0) and still hold the `bkSetUpdates` rows between its
snapshot and `--height`.

The capture tool builds the BK set before the rotation from GraphQL (checked
against leaf 2 of the rotation block) as `old_pubkeys`, and the set after it
(checked against leaf 3) as `expected_new_pubkeys`. It writes all four fixture
pieces (`block_id`, 16 leaves, delta blob, old + expected-new pubkeys).

The parity test then recomputes the rotation from the fixture and asserts:

1. `BlockIdMerkleTree(leaves).root == block_id`
2. `Poseidon(old_pubkeys) == leaves[2]`
3. `apply_delta(old_pubkeys, blob) == expected_new_pubkeys`
4. `Poseidon(new_pubkeys) == leaves[3]`

Any drift in `bridge-poseidon`, `bk_set_replay::{parse_changes, apply_block_changes}`,
`normalize_bk_set_pubkeys`, or `BlockIdMerkleTree` fails the test at CI time
instead of at first live drain.
