# Status: `review_bk_set_merkle_binding_2026-06-08.md` — resolutions as of 2026-07-27

**Scope.** Per-item status of the 7 recommendations from the 2026-06-08 review of the AN-side BK-set update Merkle binding, checked against `crates/an-bridge-prover/` at HEAD (branch `feture/block_id_16_leafs_support_plus_exta_bridge_prover_refactoring_and_cleaning`).

**No prover is deployed at this time.** All fixes below can land without a coordinated rollout.

---

## 0. Structural drift since the review

Two things about the reviewed baseline are no longer true and need to be read into every item below:

- **8-leaf → 16-leaf tree.** The review's §2 8-leaf schema is superseded by the 16-leaf depth-4 SHA-256 block-id tree (`bridge-prover-lib/src/block_id_tree.rs`, `BLOCK_ID_TREE_LEAF_COUNT = 16`). L2 = Poseidon(old BK set), L3 = Poseidon(new BK set) are unchanged; L8 = `tracked_ext_out_messages_root` is new; L9..L15 zero-padding. The L2/L3-binding semantics the review analyzes still hold — only the fold depth and sibling count changed. Open siblings for L2/L3 opening are now three (`h01`, `h4_7`, `h8_15`) instead of two.
- **Crate split.** `bk_set_fetcher.rs` moved from `bridge-prover-lib/src/` to a new `bridge-gql-fetcher` crate. Drain loop split into `bridge-prover-daemon/src/main.rs::handle_bk_update` and `bridge-prover-lib/src/live_driver/bk_update.rs::drive_next_bk_update`. All references below are to the current paths.

---

## 1. Summary

| # | Recommendation | Status |
|---|---|---|
| 1 | Layer-path `verify_gql_block_merkle` off-circuit port | **Open** |
| 2 | Drain fail-fast `upd_block.block_id == tree.root` | **Partially resolved** — L2, L3, and Fr-fold checks landed; explicit `tree.root == upd_block.block_id` compare still missing |
| 3 | Misleading self-verify comment in drain loop | **Resolved** (refactor deleted it; current comment is truthful) |
| 4 | Layer-verifier cross-check `block_id` between C1a and C2 | **Open** |
| 5 | `NEXT_UPDATE_AFTER_LOOKBACK = 100` → pagination | **Resolved in this PR** (see §5 below) |
| 6a | Unit test open Merkle reconstruction | **Resolved** — `block_id_tree.rs` has three tests including `l2_l3_opening_reconstructs_root`; all rewritten for 16-leaf |
| 6b | Parity test: real shellnet block → `Poseidon(new_bk_set) == leaves[3]` | **Partially resolved** — the equality is enforced at runtime on every drain (see §6b); no dedicated fixture test |
| 7 | `probe_bk_updates` binary (plan §7 Phase 2) | **Open** |

---

## 2. Per-item detail

### #1 — Layer-path `verify_gql_block_merkle` **[Open]**

The review asked for a port of `acki-nacki/helpers/proof_helper/src/gql_proof.rs`'s `verify_gql_proof_block` / `verify_gql_block_merkle` into the AN prover, to be called before the Circuit 2 witness builder consumes GQL leaves.

Grep across `bridge/crates/` for `verify_gql_block_merkle | verify_gql_proof_block | verify_gql | gql_proof | verify_bk_replay` finds zero hits in code — only a reference in `crates/an-bridge-prover/docs/bk_set_update_no_circuit3_plan.md`.

Current layer path still relies on the `leaves[2] == bk_set_commitment` fail-fast (and Circuit 2 witness/on-chain verifier rejection) as the only defenses against malformed GQL leaves. The failure mode is loud, not silent, so this is a **defense-in-depth gap** rather than a correctness bug.

**Suggested next step.** Deferred until we see actual GQL-corruption incidents or the layer verifier grows enough surface that a shared helper pays for itself.

### #2 — Drain fail-fast `upd_block.block_id == tree.root` **[Partially resolved]**

Present in `bridge-prover-lib/src/live_driver/bk_update.rs`:

- **L2 vs commitment** (line 89): `bail!` if `l2 != cur_commitment`.
- **Poseidon(new_pubkeys) == L3** (line 133): `bail!` on mismatch.
- **Fr-fold vs circuit-committed `block_id_fr`** (line 222): `debug_assert_eq!(fold_hash_be_to_fr(&tree.root), upd_proof.block_id_fr, ...)`.

Missing: the explicit `tree.root == upd_block.block_id` comparison called out in the review. `upd_block.block_id` is fetched at line 68 via `query_proof_block_by_seqno` and never compared to the reconstructed root — only the derived Fr form is checked, and only under `debug_assert!`. In a release build with a corrupted GQL leaf array whose fold happens to collide with the block's true Fr (equivalent-modulo-`p` on the low 32 bytes), the mismatch could go undetected until the on-chain verifier rejects.

**Suggested next step.** Small, self-contained follow-up: add `if tree.root != upd_block.block_id_be { bail!(...) }` right after `BlockIdMerkleTree::from_leaves(leaves)` at line 81. Assumes `upd_block.block_id` (or a new `block_id_be` accessor) is available as raw bytes; if the GQL currently only returns the hex form, add a decode + compare.

### #3 — Misleading self-verify comment **[Resolved]**

The reviewed text ("Local checks already enforced by the Merkle equality assertion above") is gone from `bridge-prover-daemon/src/main.rs`. The current self-verify branch at line 586 is honest:

> `"bk-update {}: self-verify does not inline-verify — synthesising verify_ok=true"`

And `bridge-prover-lib/src/live_driver/bk_update.rs`:388 explicitly names the runtime L2 check in `bk_update.rs` as the safety net rather than an "assertion above" that doesn't exist. Nothing further to do.

### #4 — Layer verifier cross-check between C1a and C2 in one bundle **[Open]**

Grep for `layer_block_id | block_id.*hex.*layer | primary.*layer.*block_id` across `bridge-verifier-daemon/` returns no matches. `bridge-verifier-daemon/src/main.rs` still does not assert that the Circuit 1a-verified `block_id` matches the Circuit 2 `layer_block_id_hex` when both belong to the same bundle.

Individual public-input pinning + state-machine monotonicity partially cover the equivalent-effect attack, but the explicit cross-check is a `~5-line` addition that makes the invariant textual rather than emergent.

**Suggested next step.** Add the assertion when the verifier is next touched. Low urgency (no observed attack path with our own prover), low cost.

### #5 — Pagination for `NEXT_UPDATE_AFTER_LOOKBACK` **[Resolved in this PR]**

**Old code.** `bridge-gql-fetcher/src/bk_set_fetcher.rs:128-145` pulled the last 100 events via `query_bk_set_updates_light(100, false)`, filtered `height > cursor_seq_no`, took the min. If more than 100 rotations accumulated between `cursor_seq_no` and chain head, the true next-past-cursor event fell off the bottom of the window and the drain **silently advanced to a later event**, skipping every rotation between them.

**New code.**

- Function rewritten to paginate `query_bk_set_updates_paged(u64::MAX, 500, after)` and return the min-height past-cursor entry from the first page that contains one.
- Ascending-order early-return: relies on AN's `bkSetUpdates` Relay connection returning edges in ascending `chain_order` (which coincides with ascending height). Cost is one round trip for near-head callers.
- Runtime sanity guard: `assert_ascending_by_height` runs on every page against a high-water mark and errors with a descriptive message if the invariant is ever violated — so a future schema / pagination-order change surfaces loudly rather than corrupting the early-return.
- Hard 10 000-page ceiling (same shape as `bk_set_at_height`) as a paranoid backstop against server-side pagination bugs.
- `NEXT_UPDATE_AFTER_LOOKBACK` constant deleted; comment prose replaced with a correctness argument in the `next_update_after` doc block.
- Two pure helpers extracted (`pick_next_past_cursor`, `assert_ascending_by_height`) so the per-page decision and the ordering invariant are unit-testable without a live GQL server.

**Tests added.**

- 9 unit tests under `mod pagination_tests` covering: min-height selection past cursor, none-when-caught-up, missing-height rows, within-page ascending, cross-page ascending, within-page descending rejection, cross-page descending rejection, missing-height transparency, equal-height acceptance. All pass locally.
- One new `#[ignore]` live test `next_update_after_paginates_from_genesis` that runs `next_update_after(client, 0)` against shellnet and pins the earliest-ever rotation to height 2 584 711 — regression coverage against the old silent-skip behavior (fetching last-100 with cursor 0 would have returned some recent event; the new paginated walk returns the true first).
- Existing `#[ignore]` `next_update_after_finds_known_rotation` and `next_update_after_handles_cursor_inside_burst` remain green under the new impl.

**Downstream.** `bridge-prover-daemon` and `bridge-verifier-daemon` both build clean; the drain-loop caller in `bridge-prover-lib/src/live_driver/bk_update.rs:45` still treats `Ok(None)` as "caught up, proceed", so semantics are preserved.

**Phase 2a (server-side `height_start` filter) not landed.** Would require a GQL schema check against a live AN node; deferred until we see per-drain-iteration latency worth the round-trip savings. The current implementation is O(1) round trip for the common near-head case anyway.

### #6a — Unit test open Merkle reconstruction **[Resolved]**

`bridge-prover-lib/src/block_id_tree.rs:187-232` ships three tests, all rewritten for the 16-leaf depth-4 fold:

- `root_and_l0_opening_are_consistent`
- `l2_l3_opening_reconstructs_root` — exactly the invariant the review asked for
- `zero_padded_right_subtree_matches_spec` — pins the L9..L15 zero-collapse against a re-fold from L0's sibling path

### #6b — Parity test against real shellnet block **[Partially resolved]**

There is no dedicated fixture-based parity test comparing an AN node's `leaves[3]` against a locally-recomputed `Poseidon(new_bk_set)`. However, the same invariant is enforced on **every** live drain step at `bridge-prover-lib/src/live_driver/bk_update.rs:133`:

```rust
if recomp_c != l3 { anyhow::bail!("Poseidon(new_pubkeys) != L3", ...) }
```

So every shellnet drain the daemon runs is a parity check with immediate failure on drift. A dedicated fixture test would still be worth adding — it catches regressions in CI without needing a live network — but the review's underlying safety concern is covered.

**Suggested next step.** When we next capture a shellnet bk-update block for regression fixtures, drop the 16 leaves + the parsed rotation blob into a JSON under `bridge-prover-lib/tests/fixtures/` and add a unit test that re-runs the drain's L3 replay. Low urgency.

### #7 — `probe_bk_updates` binary **[Open]**

Only referenced in `crates/an-bridge-prover/docs/bk_set_update_no_circuit3_plan.md`. Never added. Pure operational tooling for BK-rotation cadence study — not needed while rotation is off on shellnet. Defer until we have a rotating deploy where the data would be actionable.

---

## 3. What lands in this PR

- `bridge-gql-fetcher/src/bk_set_fetcher.rs` — new `next_update_after` implementation + two pure helpers + 9 unit tests + 1 live test. `NEXT_UPDATE_AFTER_LOOKBACK` constant removed.
- This status doc.

## 4. What remains open

Ordered by suggested priority:

1. **#2 tree.root cross-check.** ~5 lines in `bk_update.rs`. Ships with #4 in one small PR when convenient.
2. **#4 layer-verifier block_id cross-check.** ~5 lines in `bridge-verifier-daemon`.
3. **#6b parity fixture.** ~50 lines + one JSON, next time we capture a fresh shellnet bk-update.
4. **#1 layer-path `verify_gql_block_merkle`.** Defer unless we see actual GQL-corruption incidents.
5. **#7 `probe_bk_updates`.** Defer until a rotating deploy exists.
