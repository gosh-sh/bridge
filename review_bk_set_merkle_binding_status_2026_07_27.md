# Status: `review_bk_set_merkle_binding_2026-06-08.md` — resolutions as of 2026-07-27

**Scope.** Per-item status of the 7 recommendations from the 2026-06-08 review of the AN-side BK-set update Merkle binding, checked against `crates/an-bridge-prover/` at HEAD (branch `feture/block_id_16_leafs_support_plus_exta_bridge_prover_refactoring_and_cleaning`).

## Changelog

- **2026-07-27 (initial).** #5 landed as real fix (pagination). #2 landed as diagnostic. #3/#4 confirmed resolved by earlier refactors. #6a confirmed already covered by existing tests. #6b noted as partial (runtime check exists, no fixture). #1/#7 open, deferred.
- **2026-07-27 (later, same day).** #6b upgraded to resolved — landed fixture-driven parity test + capture example (skip-on-missing-fixture behavior; fixture JSON itself needs one shellnet round-trip to commit).
- **2026-07-27 (later, same day).** #7 upgraded to resolved — landed `probe_bk_updates` binary ahead of shellnet BK rotation being enabled. Quickstart at `crates/an-bridge-prover/bridge-prover-daemon/docs/PROBE_BK_UPDATES.md`. Only #1 remains open.

---

## 0. Structural drift since the review

Two things about the reviewed baseline are no longer true and need to be read into every item below:

- **8-leaf → 16-leaf tree.** The review's §2 8-leaf schema is superseded by the 16-leaf depth-4 SHA-256 block-id tree (`bridge-prover-lib/src/block_id_tree.rs`, `BLOCK_ID_TREE_LEAF_COUNT = 16`). L2 = Poseidon(old BK set), L3 = Poseidon(new BK set) are unchanged; L8 = `tracked_ext_out_messages_root` is new; L9..L15 zero-padding. The L2/L3-binding semantics the review analyzes still hold — only the fold depth and sibling count changed. Open siblings for L2/L3 opening are now three (`h01`, `h4_7`, `h8_15`) instead of two.
- **Crate split.** `bk_set_fetcher.rs` moved from `bridge-prover-lib/src/` to a new `bridge-gql-fetcher` crate. Drain loop split into `bridge-prover-daemon/src/main.rs::handle_bk_update` and `bridge-prover-lib/src/live_driver/bk_update.rs::drive_next_bk_update`. All references below are to the current paths.

---

## 1. Summary

Ranked by honest severity — most items in this review are defense-in-depth / diagnostic improvements, not correctness fixes. The one exception (item #5) shipped in this PR.

| # | Recommendation | Status | Honest importance |
|---|---|---|---|
| 5 | `NEXT_UPDATE_AFTER_LOOKBACK = 100` → pagination | **Resolved (this PR)** | **Real bug.** Silent-skip of BK-updates if prover ever lagged >100 rotation events. Not observable on shellnet (rotation off); a footgun the moment a rotating deploy runs. |
| 6b | Parity test: shellnet block → `Poseidon(new_bk_set) == leaves[3]` | **Resolved (this PR)** — capture example + fixture-driven test scaffolded; skip-on-missing-fixture, activates on first capture | Modest. Runtime check is already load-bearing; the fixture test is CI insurance against future schema drift in `bridge-poseidon` / `parse_bk_set_changes_pub` / `block_id_tree`. |
| 3 | Misleading self-verify comment | **Resolved (by refactor)** | Cosmetic. Now honest. |
| 6a | Unit test open Merkle reconstruction | **Resolved** | Low. Three tests exist in `block_id_tree.rs`, rewritten for 16-leaf. |
| 2 | Drain fail-fast `tree.root == upd_block.block_id` | **Resolved (this PR)** | **Low — diagnostic/fail-fast only, not a security fix.** See §#2 for the honest analysis. |
| 4 | Layer-verifier cross-check `block_id` between C1a and C2 | **Resolved (by refactor)** | **Zero — invariant now enforced by IPC schema.** See §#4 for the surprise finding. |
| 1 | Layer-path `verify_gql_block_merkle` off-circuit port | **Open** | Low. Failure mode is already loud (Circuit 2 witness builder / on-chain verifier rejects malformed leaves). Deferred. |
| 7 | `probe_bk_updates` binary | **Resolved (this PR)** — landed in advance of shellnet BK rotation being enabled | Zero today, will be moderate the day rotation goes live on shellnet — needed to size drain-loop batching and per-day rotation-proof budget. |

---

## 2. Per-item detail

### #1 — Layer-path `verify_gql_block_merkle` **[Open]**

The review asked for a port of `acki-nacki/helpers/proof_helper/src/gql_proof.rs`'s `verify_gql_proof_block` / `verify_gql_block_merkle` into the AN prover, to be called before the Circuit 2 witness builder consumes GQL leaves.

Grep across `bridge/crates/` for `verify_gql_block_merkle | verify_gql_proof_block | verify_gql | gql_proof | verify_bk_replay` finds zero hits in code — only a reference in `crates/an-bridge-prover/docs/bk_set_update_no_circuit3_plan.md`.

Current layer path still relies on the `leaves[2] == bk_set_commitment` fail-fast (and Circuit 2 witness/on-chain verifier rejection) as the only defenses against malformed GQL leaves. The failure mode is loud, not silent, so this is a **defense-in-depth gap** rather than a correctness bug.

**Honest importance.** Low. Deferred until we see actual GQL-corruption incidents or the layer verifier grows enough surface that a shared helper pays for itself.

### #2 — Drain fail-fast `upd_block.block_id == tree.root` **[Resolved (this PR)]**

**Landed at** `bridge-prover-lib/src/live_driver/bk_update.rs`: right after `let tree = BlockIdMerkleTree::from_leaves(leaves);`, added:

```rust
if tree.root != upd_block.block_id {
    anyhow::bail!(
        "bk-update {}: reconstructed tree.root {} != upd_block.block_id {} — \
         GQL leaves inconsistent with block header", ...
    );
}
```

Complements the pre-existing checks in the same function: L2 vs commitment (line ~89), Poseidon(new_pubkeys) == L3 (line ~133), and the `debug_assert_eq!(fold(tree.root), upd_proof.block_id_fr)` further down.

**Honest importance: LOW.** This is *not* a security fix. The verifier's step 2b already catches every case where GQL returns bad leaves:

1. Bad leaves → `tree.root` is garbage.
2. Prover writes `block_id_be = tree.root` (garbage) into the IPC bundle.
3. Verifier re-folds `L2‖L3 + siblings` up → same garbage root; matches `block_id_be`, step 2 passes.
4. Step 2b: `fold(garbage_root) == upd_proof.block_id_fr`? `upd_proof.block_id_fr` comes from the *attestation* (over the *real* block_id), so they differ → verifier bails.

A collision that made these agree would require SHA-256 second preimage (infeasible). What the new check actually buys us:

- **Fail-fast latency.** Skip Circuit 1a proof gen (~seconds) + IPC roundtrip when leaves are broken; error immediately instead of paying full aggregator cost + waiting for verifier.
- **Release-build parity.** `debug_assert_eq!` at line ~222 is compiled out in release. The new `bail!` enforces the invariant in release too.
- **Diagnostic clarity.** "GQL leaves inconsistent with block header" is more actionable than a verifier-side "step 2b failed".

Shipped because it's five lines and closes the review item cleanly. Would not have been worth doing on its own merits.

### #3 — Misleading self-verify comment **[Resolved (by refactor)]**

The reviewed text ("Local checks already enforced by the Merkle equality assertion above") is gone from `bridge-prover-daemon/src/main.rs`. The current self-verify branch at line 586 is honest:

> `"bk-update {}: self-verify does not inline-verify — synthesising verify_ok=true"`

And `bridge-prover-lib/src/live_driver/bk_update.rs`:388 explicitly names the runtime L2 check in `bk_update.rs` as the safety net rather than an "assertion above" that doesn't exist. Nothing further to do.

### #4 — Layer-verifier cross-check between C1a and C2 in one bundle **[Resolved (by refactor)]**

**Surprise finding.** The 2026-07-22 IPC schema-v5 refactor (documented at `bridge-prover-lib/src/ipc.rs:19`) **removed** the separate `layer_block_id_hex` field:

> "to 5 when `layer_block_id_hex` was removed — after the 2026-07-22 Circuit 1 byte-order fix"

`ProofRequest` now carries a single `block_id_hex` (line 88) that feeds *both* Circuit 1a and Circuit 2 public inputs. At `bridge-verifier-daemon/src/main.rs:411` the verifier uses the same `block_id_fr` for both proofs' instances (comment lines 407-409 makes this explicit).

**This makes mismatched bundles unrepresentable in the IPC schema.** If prover packaged C1a for block X and C2 for block Y, only one `block_id_hex` can be sent — verifier uses it for both proofs' public inputs, so whichever proof was over the other block fails ZK verification.

**Honest importance: ZERO.** Better than a runtime check would have been. The review's concern was valid at time of writing (schema still had two fields); the refactor closed it by construction.

Nothing landed for this item.

### #5 — Pagination for `NEXT_UPDATE_AFTER_LOOKBACK` **[Resolved (this PR)]**

**Honest importance: HIGH.** The one actual bug in this review — a silent-skip of BK-update events, not caught by any downstream check.

**Old code.** `bridge-gql-fetcher/src/bk_set_fetcher.rs:128-145` pulled the last 100 events via `query_bk_set_updates_light(100, false)`, filtered `height > cursor_seq_no`, took the min. If more than 100 rotations accumulated between `cursor_seq_no` and chain head, the true next-past-cursor event fell off the bottom of the window and the drain **silently advanced to a later event**, skipping every rotation between them. Prover's `stored_bk_set_commitment` would jump forward past valid rotations; the first bundle whose `leaves[2]` equaled a skipped rotation's L3 would then fail — but only after wasting the aggregator wrap. On shellnet BK rotation is off, so no observable failure today.

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

**Downstream.** `bridge-prover-daemon` and `bridge-verifier-daemon` both build clean; the drain-loop caller in `bridge-prover-lib/src/live_driver/bk_update.rs` still treats `Ok(None)` as "caught up, proceed", so semantics are preserved.

**Phase 2a (server-side `height_start` filter) not landed.** Would require a GQL schema check against a live AN node; deferred until we see per-drain-iteration latency worth the round-trip savings. The current implementation is O(1) round trip for the common near-head case anyway.

### #6a — Unit test open Merkle reconstruction **[Resolved]**

`bridge-prover-lib/src/block_id_tree.rs:187-232` ships three tests, all rewritten for the 16-leaf depth-4 fold:

- `root_and_l0_opening_are_consistent`
- `l2_l3_opening_reconstructs_root` — exactly the invariant the review asked for
- `zero_padded_right_subtree_matches_spec` — pins the L9..L15 zero-collapse against a re-fold from L0's sibling path

### #6b — Parity test against real shellnet block **[Resolved (this PR)]**

Two pieces landed:

1. **`bridge-prover-lib/tests/parity_bk_update.rs`** — fixture-driven test. Loads `tests/fixtures/bk_update_shellnet.json` if present and re-executes the four checks `drive_next_bk_update` runs at drain time (tree fold, `Poseidon(old) == leaves[2]`, delta application, `Poseidon(new) == leaves[3]`) against the captured node output. When the fixture file is absent the test prints a skip hint (with the capture command) and returns green — so a fresh checkout that has never run the capture is not a CI failure. Land a fixture once, and the test starts guarding drift automatically.
2. **`bridge-prover-daemon/examples/capture_bk_update_fixture.rs`** — one-shot capture tool. Given `--height N`, `--genesis-bk-set PATH`, and `BRIDGE_GQL_ENDPOINT`, it folds all rotations with `height < N` onto the genesis anchor to derive `old_pubkeys`, fetches the target block's 16 leaves and its `bk_set_update_hex`, applies the delta to derive `expected_new_pubkeys`, and writes the JSON fixture the parity test consumes. `tests/fixtures/README.md` documents the flow.

**What the test catches at CI time that runtime coverage misses.** A drift PR — say a `bridge-poseidon` version bump, a change to `parse_bk_set_changes_pub` blob layout, a leaf-order / fold-direction change in `BlockIdMerkleTree` — will compile clean and pass the crate's own unit tests (they test the module against itself). Today the failure would only surface at first live drain on a running deploy, panicking `Poseidon(new_pubkeys) != L3` after already emitting a proof request. With the fixture in place, the same PR fails `cargo test` at review time with a diff pointing at the exact broken invariant.

**Honest importance: MODEST.** The fixture is CI insurance against a specific class of future regression, not a live-bug fix. Runtime drain coverage already exists. The reason to land the scaffold now rather than "when someone captures one" is that the machinery (schema + test + capture tool + skip-when-missing path) is the awkward part; capturing the JSON later is a five-minute operation.

**Not landed:** the JSON fixture itself. Requires a live shellnet round-trip and a shellnet `bk_set.shellnet.json` anchor on disk. Capture the moment someone has shellnet access — no code change needed, just drop the file at `tests/fixtures/bk_update_shellnet.json`.

### #7 — `probe_bk_updates` binary **[Resolved (this PR)]**

Landed at `bridge-prover-daemon/src/bin/probe_bk_updates.rs`. Read-only cadence study for the AN `bkSetUpdates` stream, matching the spec in `docs/bk_set_update_no_circuit3_plan.md` §Phase 2.

**Shape.**

- Paginated ascending walk over the full stream (`query_bk_set_updates_paged` in 500-event pages) — same pagination the prover uses, so a server-side hiccup surfaces the same way it would in production.
- `--last N` truncates the reported window after the full walk (server-side `last: N` would hide the earliest-height context).
- Reports per-event `(height, delta_from_prev, block_id)` plus a summary: total walked, first/last height, gap min/median/p90/max/mean, cluster count + largest cluster at `--gap-threshold`.
- `--json` for machine-readable output (script into a spreadsheet / notebook).
- Warns (does not fail) on out-of-order or missing-height events so the probe stays useful when the chain misbehaves.
- Endpoint comes from `--gql` or `BRIDGE_GQL_ENDPOINT` — matches the ergonomics of the other prover-side tools.

**Why land it now instead of waiting for the first rotating deploy.**

You are about to enable BK rotation on shellnet. Having the probe already built and reviewed means: the moment rotation is on, one command against shellnet returns real cadence numbers — no coding under pressure, no "add tooling to the debug loop" tax when you actually want to be watching drain behavior.

**Cost audit.** ~250 LoC, one new binary, no coupling to `bridge-prover-lib`, zero new deps (uses `serde_json` + `bridge-gql-fetcher` already in scope). Compiles clean.

**Honest importance.** Zero today (shellnet rotation still off; the probe would return either a static pre-freeze snapshot or nothing). Moderate the day rotation is enabled — informs whether one-at-a-time drain is fine or drain-loop batching is needed, and lets you estimate per-day rotation-proof budget so daemon throughput sizing has a real number behind it.

---

## 3. What lands in this PR

- `bridge-gql-fetcher/src/bk_set_fetcher.rs` — new `next_update_after` implementation + two pure helpers + 9 unit tests + 1 live test. `NEXT_UPDATE_AFTER_LOOKBACK` constant removed. *(Item #5, real fix.)*
- `bridge-prover-lib/src/live_driver/bk_update.rs` — `tree.root != upd_block.block_id` bail-out after tree construction. *(Item #2, diagnostic/fail-fast only.)*
- `bridge-prover-lib/tests/parity_bk_update.rs` + `bridge-prover-lib/tests/fixtures/README.md` — fixture-driven parity test with skip-on-missing-fixture behavior. *(Item #6b, CI insurance scaffold.)*
- `bridge-prover-daemon/examples/capture_bk_update_fixture.rs` — one-shot capture tool that produces the fixture from a live GQL endpoint. *(Item #6b.)*
- `bridge-prover-daemon/src/bin/probe_bk_updates.rs` — cadence-study binary for a rotating deploy. *(Item #7, landed in advance of shellnet BK rotation being enabled.)*
- This status doc.

## 4. What remains open

Ordered by suggested priority:

1. **#6b capture** — run `capture_bk_update_fixture` against shellnet and commit the resulting JSON. Trivial once shellnet access + a genesis anchor are available; not blocked on any code.
2. **#7 first run** — after BK rotation is enabled on shellnet, run `probe_bk_updates --json` and pin the resulting cadence numbers somewhere durable (memory / status doc) so daemon sizing decisions have a real baseline.
3. **#1 layer-path `verify_gql_block_merkle`.** Defer unless we see actual GQL-corruption incidents.

## 5. Overall honest read

Of the 7 review items, **one** was a real bug (#5, silent-skip). **Two** were resolved by unrelated refactors (#3 comment cleanup, #4 IPC-schema unification making mismatched bundles unrepresentable). **Three** are diagnostic/fail-fast improvements that don't change security posture but do close CI gaps (#2 release-build parity, #6a existing tests confirmed, #6b fixture-based CI insurance). **Two** remain deferred as low-value operational polish (#1, #7).

The review was useful primarily for surfacing #5; the remaining items were either already fixed by other work or shake out as defense-in-depth rather than correctness gaps. #6b in particular is scaffolded but inert until someone drops in a captured shellnet fixture — deliberately, to keep fresh-checkout CI green.
