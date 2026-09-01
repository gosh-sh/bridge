# bk-update parity fixtures

Drop a captured shellnet fixture here as `bk_update_shellnet.json` to enable
[`../parity_bk_update.rs`]. Without a fixture the test is a no-op (prints a
skip hint with the capture command).

Capture with `bridge-prover-daemon/examples/capture_bk_update_fixture.rs`:

```sh
BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql \
  cargo run --release --example capture_bk_update_fixture -- \
    --height <bk-update block seqno> \
    --genesis-bk-set path/to/bk_set.shellnet.json \
    --out crates/bridge-prover-libraries/bridge-prover-lib/tests/fixtures/bk_update_shellnet.json
```

`--height` must be the seq_no of a block that emits a `bkSetUpdates` event
(look at chain history). `--genesis-bk-set` must be the JSON snapshot of the
BK set active at chain height 0 (or any known-good anchor prior to `--height`).

The capture tool folds every rotation strictly before `--height` onto the
genesis anchor to derive `old_pubkeys`, then applies the target rotation's
delta to derive `expected_new_pubkeys`. It writes all four fixture pieces
(`block_id`, 16 leaves, delta blob, old + expected-new pubkeys).

The parity test then recomputes the fold from scratch and asserts:

1. `BlockIdMerkleTree(leaves).root == block_id`
2. `Poseidon(old_pubkeys) == leaves[2]`
3. `apply_delta(old_pubkeys, blob) == expected_new_pubkeys`
4. `Poseidon(new_pubkeys) == leaves[3]`

Any drift in `bridge-poseidon`, `parse_bk_set_changes_pub`,
`normalize_bk_set_pubkeys`, or `BlockIdMerkleTree` fails the test at CI time
instead of at first live drain.
