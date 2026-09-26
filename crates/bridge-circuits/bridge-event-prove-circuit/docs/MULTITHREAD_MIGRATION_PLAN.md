# Multi-Thread Migration Plan — Bridge Event-Prove Circuit

Companion to [`MULTITHREAD_BRIDGE_EVENT_CIRCUIT_SPECIFICATION.md`](./MULTITHREAD_BRIDGE_EVENT_CIRCUIT_SPECIFICATION.md). Where the spec answers *what* the multi-thread circuit must compute, this plan answers *how* to get from today's single-thread `bridge-event-prove-circuit` to the two-circuit bundle described in the spec, in a landing order that stays green on `bridge-circuits.yaml` after every commit.

The plan mirrors the migration order that shipped for `dexdo-halo2-kit/dex-halo2-circuit` on branch `feature/multithreading` (`git log main..feature/multithreading` from `db55663` forward), with every salt / anonymization / uniformity commit removed. Rationale for each drop is in §11.

Target repo: `bridge` (`origin` = `https://github.com/gosh-sh/bridge.git`).
Target branch: `feature/multithreading` (this branch; the spec landed as `333bdd2`).
CI gate: `bridge-circuits.yaml` (PR-triggered — the only Rust job that runs on a bridge PR).
Changelog: append to `CHANGELOG.md` under `## [Unreleased]` per `AGENTS.md`.

Every commit below is meant to compile, `cargo test -p bridge-event-prove-circuit` green, and leave downstream consumers unbroken. Where a commit must land two crates atomically to keep `bridge-circuits.yaml` green, that is called out explicitly.

---

## 0. Guiding principles

- **Additive-first.** Every step introduces a new file/type/PI alongside the existing single-thread surface. The single-thread `BridgeEventProveCircuit` (11 PIs) stays green under `cargo test` until the very last commit swaps consumers over. This is exactly the "prefer adding new over modifying existing" pattern the DEX migration followed and that lives in user memory.
- **VK rotation is a breaking change.** `AGENTS.md` calls this out explicitly. `bridge-event-prove-circuit`'s VK will change **twice** in this migration — once when the FinalProof PI count grows from 11 → 13 (§4), and again when we lock the aggregator wire format (§8). Group these so we rotate at most one production VK per PR.
- **Fixed vs variable bundle length.** The spec forbids padding `N_BUNDLE` (§1.4). The circuit ships as **two** distinct halo2 circuits — `BridgeEventFinalProof` (always present) and `BridgeMultiHopProof` (0..N_BUNDLE snarks). SHPLONK aggregator handles the runtime-variable count on the EVM side; the plan follows suit.
- **No salt code lands.** `dexdo-halo2-kit/dex-halo2-circuit/src/salt.rs`, the `bundle_index` witness, `salt_commitment` PI, position tags, and `SALTED_*_BLOCK_ID` PIs from `bundle_verifier.rs` are **not** ported. The bridge glues snarks with clear `hop_start_block_id`, `hop_end_block_id` Fr equality — a strict subset of the DEX plumbing.
- **Reuse the vendored primitives.** `dense_merkle_bound.rs` (`walk_dense_merkle_bind_pos`), `event_data_helper.rs`, `boc_helper.rs`, `poseidon.rs` already live in `bridge-event-prove-circuit/src/`. The new files below are the minimum extension on top; nothing is copied "for parallel structure with DEX".
- **Never modify committed spec text mid-migration.** If the migration exposes a spec ambiguity, update the spec in the same commit that consumes it — the DEX branch did this repeatedly and it kept the "code wins" invariant of `AGENTS.md/docs/DOCS.md` intact.

---

## 1. File inventory — before, target, and migration verb

| File in `bridge-event-prove-circuit/src/` | Today | Target | Migration verb |
|--|--|--|--|
| `lib.rs` | 5 pub mods | 9 pub mods | **add** modules below |
| `bridge_event_prove_circuit.rs` (1840 lines) | Single-thread circuit, 11 PIs | **`BridgeEventFinalProof`** (renamed later), 13 PIs, X/Y-split | **modify in place** (last commit only; §7) |
| `boc_helper.rs` | unchanged | unchanged | keep |
| `dense_merkle_bound.rs` | `walk_dense_merkle_bind_pos` | unchanged | keep |
| `event_data_helper.rs` | field-extraction on X's ext-out body | unchanged | keep |
| `poseidon.rs` | Poseidon96 wrappers | unchanged | keep |
| `test_helpers.rs` | Single-thread synthetic witness | Extended with multi-hop synth builders | **modify** (add `synth_multithread_*`) |
| `block_id_tree.rs` | — | Ported from DEX (SHA-256 depth-4 walker + siblings) | **new** (§2) |
| `multi_hop_witness.rs` | — | Ported from DEX minus salt fields | **new** (§3) |
| `multi_hop_proof.rs` | — | Ported from DEX minus salted-endpoint gadget | **new** (§5) |
| `bundle_verifier.rs` | — | Pure-Rust mock of `withdrawByProofBundle` | **new** (§6) |
| `kzg_source.rs` | — | Hermez SRS wrapper (bin + lib) | **new** (§8) |

Bridge deltas from DEX file set:
- **no** `salt.rs`
- **no** `poseidon_dex_helper.rs` (DEX-only Poseidon-DEX identity plumbing)
- `voucher_event_helper.rs` → the bridge equivalent already lives at `event_data_helper.rs`; no rename needed.

---

## 2. Commit 1 — Lift outer block-id tree to depth-4 / 16 leaves *(mirrors DEX `db55663`)*

**Scope.** Add `block_id_tree.rs` containing the SHA-256 depth-4 walker, sibling-path structures, and the four fixed cells for `L9..L15 = [0u8; 32]`. Wire it as a `pub mod` in `lib.rs`. Nothing else in the crate uses it yet.

**Concrete steps.**

1. Copy the SHA-256 depth-4 walker + sibling arithmetic from `dex-halo2-circuit/src/block_id_tree.rs` (155 lines) into `bridge-event-prove-circuit/src/block_id_tree.rs`. Keep the file self-contained: only depends on the workspace SHA-256 chip (`gosh-sha256-chip`) and `halo2_base`.
2. **Do not** import from DEX-specific helpers (`poseidon_dex_helper`, `salt`, `voucher_event_helper`). The DEX file already avoids these; a straight port is safe.
3. Add `pub mod block_id_tree;` to `src/lib.rs` (alphabetical order, before `boc_helper`).
4. Copy the `constants_match_gql_proof_layout`, `block_merkle_root_and_proof_roundtrip`, and `block_merkle_proof_rejects_tampering` unit tests. These are pure-Rust; they run under the fast MockProver step of `bridge-circuits.yaml`.

**CI risk.** None — no existing type sees the new module.

**Downstream risk.** None.

**Test to add.** `cargo test -p bridge-event-prove-circuit --lib block_id_tree::` — three tests, all pure-Rust, ~milliseconds.

**Changelog entry.** *Added.* "`bridge-event-prove-circuit`: depth-4 SHA-256 block-id tree primitives (`block_id_tree` module). No public surface change — used by the multi-thread work in progress."

---

## 3. Commit 2 — Add native multi-hop witness types + L7 walk mirrors *(mirrors DEX `multi_hop_witness.rs` slice of `db55663`, minus salt fields)*

**Scope.** Add `multi_hop_witness.rs` with `BlockWitness`, `HopWitness`, `MultiHopProofWitness` — **minus** `bundle_index` and `salt_commitment`, and with `HopWitness.salted_start_block_id` / `salted_end_block_id` replaced by clear-byte fields.

**Concrete steps.**

1. Copy `dex-halo2-circuit/src/multi_hop_witness.rs` (678 lines) into `bridge-event-prove-circuit/src/multi_hop_witness.rs`.
2. **Delete** the following DEX-only surface:
   - `HopWitness.salted_start_block_id: Fr` → replace with `hop_start_block_id: [u8; 32]`
   - `HopWitness.salted_end_block_id: Fr` → replace with `hop_end_block_id: [u8; 32]`
   - `MultiHopProofWitness.salt_commitment: Fr` → **delete outright**
   - `MultiHopProofWitness.bundle_index: u32` → **delete outright**
3. Keep every constant *identically named and identically valued*:
   - `BLOCK_MERKLE_LEAF_COUNT = 16`, `BLOCK_MERKLE_DEPTH = 4`, `MAX_HISTORY_PROOF_LAYERS = 10`, `HISTORY_PROOF_WINDOW_SIZE = 128`, `H_HOPS_PER_PROOF = 5`, `MAX_PROOF_BLOCK_REFS = 256`, `MAX_PROOF_BLOCK_REFS_DEPTH = 8`, `REFERENCED_PARENT_BLOCK_TAG`, `REFERENCED_REF_BLOCK_TAG`.
   - `N_BUNDLE` — **rename** to `N_BUNDLE_MAX` and set to `4` for the prototype (matches spec §0). Add a doc-comment pointing at production target 60.
4. Keep every native helper *unchanged*: `poseidon_bytes_flat_native`, `ref_leaf_hash_native`, `ref_inner_combine_native`, `proof_block_refs_root_native`, `proof_block_ref_inner_path_native`, `verify_proof_block_ref_inner_path`, `block_merkle_root`, `block_merkle_leaf_proof`, `verify_block_merkle_leaf_proof`, `refs_tree_depth_native`, `assert_ref_index_is_cross_thread`.
5. Add `pub mod multi_hop_witness;` to `src/lib.rs`.
6. Port the DEX unit tests **verbatim** — they test native helpers that we did not modify, so they must remain green.

**CI risk.** None — additive.

**Test count.** ~7 native unit tests (all present in DEX file).

**Changelog entry.** *Added.* "`bridge-event-prove-circuit`: native witness types + L7 walk / block-merkle helpers for the multi-thread claim (`multi_hop_witness` module)."

---

## 4. Commit 3 — Extend `BridgeEventProveCircuit` PI layout to 13 slots (rename to `BridgeEventFinalProof`, X/Y split) *(mirrors DEX `ee57e68`, minus salt PIs)*

This is the **first breaking change**. It rotates the production VK of Circuit 4. Land it in a PR that is exclusively about this rotation so the changelog entry is unambiguous, and coordinate with `bridge-relayer-daemon` / `ackinacki-bridge` PR authors before merge.

**Scope.**

- Rename type: `BridgeEventProveCircuit` → `BridgeEventFinalProof`. Keep the file at `bridge_event_prove_circuit.rs` for one commit (renaming the file has independent value; do it in a separate follow-up if the diff churn becomes noisy). The `Cargo.toml` package name stays.
- Split the single `block_id` witness into two:
  - `x_block_id: [u8; 32]` — the event block, feeds the nullifier and is reconstructed by opening `event_hash → L8` on X's block-id tree.
  - `y_block_id: [u8; 32]` — the anchor block, feeds the layer-1 `block_leaf` and the Y-side dense-chain.
- Add 2 new public inputs, both at the tail of the existing layout:
  - `PUB_X_BLOCK_ID = 11` — Fr-encoded `x_block_id`.
  - `PUB_Y_BLOCK_ID = 12` — Fr-encoded `y_block_id`.
- `TOTAL_PUBLIC_INPUTS: 11 → 13`.
- PIs `[0..=10]` stay **byte-identical** to today — this preserves the daemon's PI parser for the first 11 slots and only requires it to read two more.
- When `t = 0` (same-thread case) the circuit still binds `x_block_id == y_block_id` via a direct copy-constraint. There is no separate "single-thread mode" — the FinalProof is one shape whether the bundle has 0 or N_BUNDLE hops. That copy-constraint is skipped when `t ≠ 0` (a `Bool` selector fed from the witness). See spec §7.

**Concrete steps.**

1. In `bridge_event_prove_circuit.rs:346`, the current `let block_id = ctx.load_witness(...)` becomes `let x_block_id = ...; let y_block_id = ...;`.
2. Line ~822 (Y-side `block_leaf` construction) reads `y_block_id`.
3. Line ~972 (nullifier construction) reads `x_block_id`.
4. Line ~810 (`ext_out_root` reconstruction from BOC) stays on the X-side. Under the multi-thread spec the L8 opening lifts *from* `ext_out_root` *to* `x_block_id` — the L8 walker in `block_id_tree.rs` (added in Commit 1) is called here for the first time. Bind position `pos = 8` inside the depth-4 tree.
5. Add two PI equality constraints at the very end of `synthesize`: `x_block_id_fr` → PI 11, `y_block_id_fr` → PI 12.
6. Add a witness selector `is_same_thread: bool` and gate the `x_block_id == y_block_id` copy-constraint on it. Native witness builder in `test_helpers` sets `true` for L=0 samples, `false` otherwise.
7. Update `TOTAL_PUBLIC_INPUTS` at line 152.
8. Rename the `pub struct BridgeEventProveCircuit` → `BridgeEventFinalProof`, and re-export the old name as a `#[deprecated]` type alias for one release cycle to give the daemon time to migrate:
    ```rust
    #[deprecated(note = "Renamed to BridgeEventFinalProof; will be removed after the next VK rotation")]
    pub type BridgeEventProveCircuit = BridgeEventFinalProof;
    ```

**Downstream impact (must land in the same PR):**

- `crates/bridge-prover-libraries/bridge-event-prover-lib/src/prover.rs` — witness builder now needs to populate `x_block_id`, `y_block_id`, and `is_same_thread`. Same-thread hard-codes `x_block_id = y_block_id = block_id_of_the_event_block` and `is_same_thread = true`. The daemon still runs against the current live Acki Nacki node (only thread 0 events) so every real witness produced today is a same-thread witness — this makes the initial rollout safe.
- `crates/bridge-prover-libraries/bridge-event-prover-lib/src/verifier.rs` — read PIs `[11..13]`; assert `x_block_id_pi == y_block_id_pi` when `is_same_thread` claim (implicit — carry the flag in daemon config, not the proof).
- `crates/bridge-prover-libraries/bridge-event-witness/src/schema.rs` — extend `EventWitness` (or the equivalent GQL-mirroring struct) with `x_block_id` / `y_block_id` fields; the enrichment step in `enrich.rs` populates them from the fetched block.
- `crates/bridge-relayer-daemon/`: no change other than re-typing the imported `BridgeEventProveCircuit` → `BridgeEventFinalProof` after the alias is removed.
- `contracts/ethereum/src/AckiNackiBridge.sol` — the `withdrawByProof` function currently reads 11 PIs. Extend the caller-side PI array size from 11 → 13. **The two new PIs are informational at this step** — the on-chain check remains `_isKnownLayerAnchor(finalRoot, anchorLayer)`. Bundle continuity checks land in Commit 6.

**Rotate VK.** Regenerate the Circuit 4 VK. Land the updated `bound_scenario.json` fixtures under `crates/bridge-snark-utils/proofs/bound/` and the updated `verifiers/BridgeWithdrawalAggregatorVerifier.*` in the same PR.

**Changelog entry.** *Breaking Changes.* "Circuit 4 (`bridge-event-prove-circuit`) rotates its verification key. `TOTAL_PUBLIC_INPUTS` grows from 11 to 13: PI[11] = `x_block_id`, PI[12] = `y_block_id`. Same-thread claims send `x_block_id == y_block_id`. The Solidity `withdrawByProof` selector is unchanged but the caller must supply 13 PIs. Regenerate proofs and redeploy `BridgeWithdrawalAggregatorVerifier`."

---

## 5. Commit 4 — Add `BridgeMultiHopProof` circuit *(mirrors DEX `multi_hop_proof.rs` + `22bb07b` variable-depth L7 walk)*

**Scope.** Add `multi_hop_proof.rs` — the second halo2 circuit shipped by this crate.

**Public-input layout (spec §5.2).** 2 slots total:
```
[0] hop_start_block_id : Fr  (= first hop's start block_id)
[1] hop_end_block_id   : Fr  (= last hop's end block_id)
```

Delta vs DEX: DEX has 3 PIs (`[0]=salted_start_block_id, [1]=salted_end_block_id, [2]=salt_commitment`). We drop `[2]` entirely and swap the two `salted_*` slots to their clear `hop_*_block_id` equivalents.

**Concrete steps.**

1. Copy `dex-halo2-circuit/src/multi_hop_proof.rs` (946 lines) to `bridge-event-prove-circuit/src/multi_hop_proof.rs`.
2. **Delete** the following:
   - Any reference to `sk_u` (private witness) and `sk_u_commit` pinning. Bridge has no user secret.
   - The `salt` witness and every call into `salt::salted_block_id_poseidon_circuit`. Replace with a direct `bytes_to_fr(hop_end_block_id)` publication.
   - `MULTI_HOP_PUBLIC_LEN: 3 → 2`.
   - `bundle_index` witness (only used by DEX for the position tag).
   - The salted-endpoint gadget (`prove_hop_salted_endpoints` in DEX). Replaced by a trivial `bytes_to_fr` publish.
3. Keep and rename lightly:
   - `prove_hop_ref_tree_opening` — verbatim.
   - `prove_hop_block_id_reconstruction` — verbatim.
   - The `is_active` gating on ref-openings — verbatim. Its "propagation rule" changes from salted-endpoint equality to clear-byte equality: inactive hop enforces `hop_next_block_id == hop_current_block_id` (spec §4).
4. Bind `ref_index` into the walk gate (DEX `3722da6`). Verbatim — the fix has nothing to do with anonymization.
5. Do **not** port the `662a4cb` position-tag commit. Bridge has no anonymity dimension for the tags to close.
6. Add `pub mod multi_hop_proof;` to `src/lib.rs`.

**K-budget.** DEX MultiHopProof is K=17. The bridge variant is strictly smaller (no salted-endpoint Poseidons, no bundle_index witness). Prototype at K=17 and confirm ≤80% column utilization with a `cargo test -p bridge-event-prove-circuit multi_hop_proof::` MockProver run before locking. If margin is tight, keep K=17 — real prover cost per hop-snark is DEX-proven.

**Test to add.**
- `tests/test_multi_hop_positive.rs` — 5 active hops, MockProver, ~30 s.
- `tests/test_multi_hop_padding.rs` — 3 active + 2 padded hops, verifies padded-hop identity propagation.
- `tests/test_multi_hop_negative.rs` — 5 negative cases: wrong `hop_end_block_id`, tampered L7 sibling, wrong `ref_index`, mismatched refs_tree_depth, adjacent-hop `hop_end != hop_start` mismatch.

All three tests must fit inside the `bridge-circuits.yaml` fast step (each ≤60 s MockProver). Real-prover keygen is gated behind `#[ignore]` and included in a follow-on commit.

**Downstream impact.** None yet — no consumer wires the new circuit. Explicitly *not* modifying `bridge-event-prover-lib` in this commit.

**Changelog entry.** *Added.* "`bridge-event-prove-circuit`: `BridgeMultiHopProof` halo2 circuit for cross-thread L7 chains (5 hops per snark, 2 public inputs). Not yet consumed by the daemon."

---

## 6. Commit 5 — Add `bundle_verifier.rs` (pure-Rust mock of `withdrawByProofBundle`) *(mirrors DEX `bundle_verifier.rs`)*

**Scope.** Add `bundle_verifier.rs`. Pure-Rust; no halo2 dependencies except for `Fr` arithmetic. Its job is to be the *specification-executable* version of the on-chain `withdrawByProofBundle` acceptance logic — the same relationship DEX's `bundle_verifier.rs` has to `RootPN.sol`.

**Delta from DEX.**

| DEX check | Bridge equivalent |
|--|--|
| DexFinal.PI[SALTED_X_START] == first MultiHop.PI[SALTED_START] | FinalProof.PI[`PUB_X_BLOCK_ID`] == first MultiHop.PI[0] |
| DexFinal.PI[SALTED_Y_END] == last MultiHop.PI[SALTED_END] | FinalProof.PI[`PUB_Y_BLOCK_ID`] == last MultiHop.PI[1] |
| All MultiHop.PI[SALT_COMMITMENT] equal | **not applicable** — no salt |
| DexFinal.PI[SALT_COMMITMENT] == MultiHop.PI[SALT_COMMITMENT] | **not applicable** — no salt |
| MultiHop[i].PI[SALTED_END] == MultiHop[i+1].PI[SALTED_START] (adjacency) | MultiHop[i].PI[1] == MultiHop[i+1].PI[0] (clear-byte adjacency) |
| Bundle length is fixed constant | Bundle length is `ceil(L / H)` and can be 0. `n == 0 ⇒ x_block_id == y_block_id`. |
| Anchor check via `check_layer_hash` | Anchor check via `_isKnownLayerAnchor(finalRoot, anchorLayer)` — mocked here as a callback |
| DEX identity pins (`x_account_dapp_id`, `x_account_id`) | **not applicable** — bridge doesn't bind an account identity to the withdrawal recipient |

**Concrete steps.**

1. Copy `dex-halo2-circuit/src/bundle_verifier.rs` (647 lines) to `bridge-event-prove-circuit/src/bundle_verifier.rs`.
2. Rewrite the offset tables:
   ```rust
   const FINAL_LEN: usize = 13;      // PIs of BridgeEventFinalProof
   const MULTI_HOP_LEN: usize = 2;   // PIs of BridgeMultiHopProof
   mod final_offset {
       pub const X_BLOCK_ID: usize = 11;
       pub const Y_BLOCK_ID: usize = 12;
       // 0..=10 unchanged from single-thread layout — see the spec §7 PI table
   }
   mod multi_hop_offset {
       pub const HOP_START_BLOCK_ID: usize = 0;
       pub const HOP_END_BLOCK_ID: usize = 1;
   }
   ```
3. Delete every reference to `SALT_COMMITMENT`, `SALTED_*`, `X_ACCOUNT_DAPP_ID_LO/HI`, `X_ACCOUNT_ID_*`, `bundle_index`.
4. Extend `BundleError`:
   ```rust
   pub enum BundleError {
       HopBundleLengthOverflow { got: usize, max: usize },        // n > N_BUNDLE_MAX
       SameThreadRequiresEmptyHopChain { hop_count: usize },      // n == 0 must ⇒ x == y
       SameThreadEndpointsMismatch,                               // n == 0 ⇒ x_block_id != y_block_id
       HopChainHeadMismatch,                                      // x_block_id != first hop start
       HopChainTailMismatch,                                      // y_block_id != last hop end
       AdjacentHopBlockIdMismatch { at: usize },                  // MultiHop[i].end != MultiHop[i+1].start
       AnchorNotInLayerWindow { final_root: [u8; 32], anchor_layer: u8 },
   }
   ```
5. Provide `verify_bundle(final_pi: &[Fr; 13], hop_pis: &[[Fr; 2]], anchor_ok: impl Fn(&[u8;32], u8) -> bool) -> Result<(), BundleError>` with exactly those checks in order.
6. Add `pub mod bundle_verifier;` to `src/lib.rs`.
7. Unit tests: `same_thread_bundle_verifies`, `cross_thread_bundle_verifies`, and one negative per `BundleError` variant. All pure-Rust, all sub-second.

**Downstream impact.** None yet — the daemon still calls the single verifier. This commit ships the executable spec that the Solidity commit in §7 will mirror.

**Changelog entry.** *Added.* "`bridge-event-prove-circuit`: `bundle_verifier` — pure-Rust reference implementation of the multi-hop bundle acceptance logic (matches the Solidity `withdrawByProofBundle` extension coming in a follow-up)."

---

## 7. Commit 6 — Extend `AckiNackiBridge.sol` with `withdrawByProofBundle`

**Scope.** Solidity + Foundry fixture only. Rust unchanged.

**Concrete steps.**

1. In `contracts/ethereum/src/AckiNackiBridge.sol`, add:
   ```solidity
   function withdrawByProofBundle(
       uint256[] calldata finalPublicInputs,        // length == 13
       bytes    calldata finalProof,
       uint256[][] calldata hopPublicInputs,        // hopPublicInputs[i].length == 2
       bytes[]  calldata hopProofs                  // hopProofs.length == hopPublicInputs.length
   ) external { ... }
   ```
2. The function body executes exactly the checks in the Rust `bundle_verifier::verify_bundle`, in the same order. Emit distinct custom errors matching `BundleError` variants:
   ```solidity
   error HopBundleLengthOverflow(uint256 got, uint256 max);
   error SameThreadRequiresEmptyHopChain(uint256 hopCount);
   error SameThreadEndpointsMismatch();
   error HopChainHeadMismatch();
   error HopChainTailMismatch();
   error AdjacentHopBlockIdMismatch(uint256 at);
   ```
3. Anchor check re-uses `_isKnownLayerAnchor(finalPublicInputs[9] /* PUB_FINAL_ROOT */, finalPublicInputs[10] /* PUB_ANCHOR_LAYER */)`.
4. After all cross-snark checks pass, call `BridgeWithdrawalAggregatorVerifier.verifyProof(finalProof, finalPublicInputs)` and, for each i, `BridgeMultiHopAggregatorVerifier.verifyProof(hopProofs[i], hopPublicInputs[i])`. The second verifier is added in §8.
5. `withdrawByProof` (single-proof legacy) stays untouched — same-thread callers can still use it, or migrate to `withdrawByProofBundle` with `hopPublicInputs.length == 0`. Deprecate later.
6. Foundry tests under `contracts/ethereum/test/AckiNackiBridge.t.sol`: one green-path unit test per bundle length (0, 1, 5), one revert test per custom error.
7. Coverage: this passes `forge coverage` even with the extra bundle logic — a fuzz test that varies `hopPublicInputs.length` uses `vm.assume(len < 60)` and `bound()` (`AGENTS.md` warning against `vm.assume` rejection storms).

**CI.** Runs on `solidity.yaml` (PR-triggered). No Rust changes = `bridge-circuits.yaml` unaffected.

**Downstream impact.** None — the multi-hop verifier is not yet deployed, so `hopPublicInputs.length > 0` will revert at the verifier call. This is fine: the daemon still submits only same-thread claims until §9.

**Changelog entry.** *Added.* "`AckiNackiBridge.withdrawByProofBundle` for multi-hop cross-thread withdrawals. Same-thread callers pass `hopPublicInputs.length == 0` and receive identical semantics to `withdrawByProof`. Custom errors: `HopBundleLengthOverflow`, `SameThreadRequiresEmptyHopChain`, `SameThreadEndpointsMismatch`, `HopChainHeadMismatch`, `HopChainTailMismatch`, `AdjacentHopBlockIdMismatch`."

---

## 8. Commit 7 — SHPLONK aggregator wiring for the second VK *(mirrors DEX `ad44758` + `6da700a`)*

**Scope.** `crates/bridge-evm-aggregator/` gains a second inner-verifier binding for `BridgeMultiHopProof`; a companion `BridgeMultiHopAggregatorVerifier.sol` is exported and deployed.

**Concrete steps.**

1. Copy `dex-halo2-circuit/src/kzg_source.rs` (Hermez SRS wrapper) into `bridge-event-prove-circuit/src/kzg_source.rs` — same shape as DEX `ad44758`. Keep the `bin/keygen_bridge_final.rs` / `bin/keygen_bridge_multi_hop.rs` bin targets in `src/bin/` so key rotation is reproducible offline. Add `pub mod kzg_source;` to `lib.rs`.
2. In `crates/bridge-evm-aggregator/src/aggregator.rs`, add a second `BoundCircuit` entry for `BridgeMultiHopProof` alongside the existing `BridgeEventFinalProof` binding. Follow the exact pattern already used for the primary/fallback pair.
3. In `crates/bridge-evm-aggregator/src/evm_export.rs`, extend the `export-inner-aggregator` binary to emit `BridgeMultiHopAggregatorVerifier.sol` + `.bin` + `_calldata.bin` into `contracts/ethereum/verifiers/`. Add SHA256 entries to `verifiers/SHA256SUMS`. Add row to `verifiers/SIZES`.
4. `verifier_sources.yaml` will byte-compare on the next PR touching `contracts/ethereum/verifiers/` — this is that PR.
5. Wire the new verifier address into `AckiNackiBridge` — a public immutable `IBridgeMultiHopVerifier public immutable multiHopVerifier` set in the constructor. The corresponding interface goes in `contracts/ethereum/src/IBridgeMultiHopVerifier.sol`.
6. `contracts/ethereum/verifiers/README.md` gets a row for the new artifact.
7. EIP-170 size check: `bridge-evm-aggregator/src/eip170.rs` already runs on every verifier build; verify the aggregated output stays under 24 KB. If it exceeds, split the two inner circuits into two distinct aggregators (DEX ran into this at a comparable circuit shape; the SHPLONK aggregator pattern handles both single-VK and multi-VK aggregation — user memory covers this).

**Downstream impact.** The daemon is now able to *submit* multi-hop bundles, but still doesn't build them. Nothing breaks — same-thread flow is unchanged.

**Changelog entry.** *Added.* "New on-chain verifier `BridgeMultiHopAggregatorVerifier` for the cross-thread hop snarks. Deploy alongside `BridgeWithdrawalAggregatorVerifier` and set its address on `AckiNackiBridge` at construction. `bridge-evm-aggregator` gains an `export-inner-aggregator` output for it."

---

## 9. Commit 8 — Live-daemon plumbing: witness builder for multi-hop bundles *(mirrors DEX `6da700a` witness half + `cb2bc80` unification)*

**Scope.** `bridge-event-prover-lib` + `bridge-event-witness` learn to build multi-hop bundles from real GQL data. `bridge-relayer-daemon` learns to submit them.

**Concrete steps.**

1. In `crates/bridge-prover-libraries/bridge-event-witness/src/schema.rs`, add a `MultiHopBundleWitness` struct mirroring `MultiHopProofWitness` from Commit 2 (`Vec<BlockWitness>` per hop; the outer container is `Vec<MultiHopProofWitness>` capped at `N_BUNDLE_MAX`). Match the GQL shape produced by the acki-nacki node's `helpers/proof_helper/src/gql_proof.rs` — the DEX branch's `multi_hop_witness.rs` already documents this mapping and the bridge port is compatible.
2. In `bridge-event-witness/src/enrich.rs`, add a `resolve_cross_thread_chain(event_block: BlockId, target_thread_0_block: BlockId) -> Result<Vec<HopWitness>>` walker that follows L7 refs from `event_block` toward some thread-0 anchor, one GQL fetch per hop.
3. In `bridge-event-prover-lib/src/prover.rs`:
   - Add `prove_multi_hop_snark(&MultiHopProofWitness) -> ProofBlob`.
   - Extend the existing `prove_event(&EventWitness) -> ProofBlob` to detect cross-thread events (event block's `thread_id != 0`) and return a `BundleProof { final: ProofBlob, hops: Vec<ProofBlob> }`. Same-thread events keep returning the single-blob variant behind a `BundleProof { final, hops: vec![] }` for API uniformity.
4. In `bridge-event-prover-lib/src/verifier.rs`, add `verify_bundle(BundleProof) -> Result<(), VerifyError>` that calls `bundle_verifier::verify_bundle` first, then per-snark SHPLONK-verify.
5. In `bridge-relayer-daemon`, extend the `daemon-bridge` / `daemon-withdraw` submission path to call `AckiNackiBridge.withdrawByProofBundle` when `hops.len() > 0`. `daemon-live` (in-process prover) does the same.
6. `bridge-prover-daemon` writes the bundle to disk as `proof_event_<seqno>.json`, which now carries both the final blob and the hop blobs — extend the JSON schema (backwards-compatible: readers of `hops: []` still work).
7. Environment-driven bootstrap (user memory: `BRIDGE_GQL_ENDPOINT`, `BRIDGE_BOOTSTRAP_SEQNO`) unchanged.

**CI.** This does not affect `bridge-circuits.yaml`. It does affect `make pre-push` (relayer clippy + tests). Run locally.

**Downstream sanity.** The very first cross-thread event on live shellnet will exercise this path — plan a dedicated shellnet E2E run and reference the shellnet-side gotchas in user memory (`BRIDGE_BK_SET_CONFIG` must be set for shellnet; live keygen may need `params_dir` ≥20 GB; `bootstrap_hermez_srs` needs the multi-hop K value too).

**Changelog entry.** *Added.* "Relayer + prover daemons build and submit multi-hop bundles for cross-thread `WithdrawalInitiated` events. `proof_event_<N>.json` gains an optional `hops` array. Same-thread events unchanged."

---

## 10. Commit 9 — Aggregate CHANGELOG rollup + real-prover CI addition

**Scope.**

1. Bring the `## [Unreleased]` section together into one narrative — do not rewrite the individual entries from Commits 1-8, but add a paragraph at the top of `[Unreleased]` under `Breaking Changes` that summarizes the migration and points at this plan.
2. In `bridge-circuits.yaml`, add the ignored real-prover test for `BridgeMultiHopProof` to the heavy step:
   ```
   cargo test -p bridge-event-prove-circuit -- --ignored real_proof_multi_hop_fixed_k
   ```
   Mirror `AGENTS.md`'s Circuit 4 heavy-step invocation for the FinalProof.
3. Update `bridge-circuits/bridge-event-prove-circuit/docs/MULTITHREAD_BRIDGE_EVENT_CIRCUIT_SPECIFICATION.md` §10 (K-budget open question) with the measured K and column utilization from Commit 4's MockProver run + Commit 8's real-prover run.
4. Delete the `#[deprecated] pub type BridgeEventProveCircuit = ...` alias introduced in Commit 3 once every downstream consumer references the new name.

**Changelog entry.** *Changed.* Cross-links to the plan doc from every `[Unreleased]` entry that touches Circuit 4.

---

## 11. What we deliberately skipped from the DEX migration

Every DEX commit on `feature/multithreading` that we did *not* mirror, and why.

| DEX commit | What it does | Why we skip |
|--|--|--|
| `9cb1983` | Extract `salted_block_id_poseidon_circuit` into `salt.rs` | No salt in bridge. Spec §1.4, §4. |
| `662a4cb` | Position-tag salted endpoints (anonymity fix) | No anonymity dimension in bridge to close. Spec §9. |
| `ae792f8` | Spec text: position-tagged salted endpoints | Not applicable. |
| `bfcbe2f` | Expose `x_account_dapp_id` / `x_account_id` as DarkDex publics `[8..12]` | Bridge does not bind an on-chain identity — recipient is an Ethereum address (`PUB_RECIPIENT_HI`, `PUB_RECIPIENT_LO` already exist). |
| `d48cc56` | Drop parent-slot branch from hop | DEX-side bookkeeping; bridge port in Commit 4 never adds the branch. |
| `cb2bc80` / `a9c8d89` | Merge two witness builders / unify DexFinalWitness types | Bridge starts unified — Commit 3 emits a single `BridgeEventFinalProofWitness`. |
| `ff571d9` | Add `poseidon_dex_helper` | DEX-only Poseidon-DEX identity plumbing. |
| `04985f2` / `6da700a` (halo2-proover halves) | Migrate DEX proover to V2 layout | Bridge does not use `halo2-proover`; the daemon integration in Commit 8 is the equivalent. |

If any of these becomes retroactively relevant (e.g. if the team ever adds a "hide event thread id" requirement) they can be layered on top — each is a self-contained addition, exactly as it was on the DEX side.

---

## 12. Rollback story

- After **Commit 3** (VK rotation): rollback = redeploy the pre-rotation `BridgeWithdrawalAggregatorVerifier` and revert `AckiNackiBridge`'s reference to it. The daemon still runs against the pre-rotation proof shape from the parallel state directory.
- After **Commit 6** (Solidity): rollback = leave `withdrawByProofBundle` in place, just don't call it. `withdrawByProof` continues to serve same-thread claims. Costless.
- After **Commit 7** (new verifier deploy): rollback = clear the `multiHopVerifier` immutable and redeploy the bridge. Any bundle with `hops.len() > 0` will revert at verify.
- After **Commit 8** (daemon plumbing): rollback = downgrade the daemon binary. `proof_event_<N>.json` files with `hops: []` remain compatible with the pre-Commit-8 verifier daemon.

There is no point during the migration where a rollback stops in-flight same-thread withdrawals from completing.

---

## 13. Open items to close before landing Commit 1

- Sanity-check the K-budget for the extended `BridgeEventFinalProof` (13 PIs, +4 SHA compressions for the L8 opening) — run a MockProver with `k=17` and inspect column utilization. If ≥95%, decide between "bump columns" or "bump K to 18" *before* Commit 3 ships to production.
- Confirm with the on-chain team that `AckiNackiBridge`'s per-layer window `layerWindows[anchorLayer]` semantics are unchanged by this migration (they should be — this plan doesn't touch `MAX_LAYER_HASHES = 10` and reuses `_isKnownLayerAnchor`). If any Ethereum-side surface *does* change (e.g. the recent ETH-35 / ETH-39 work landed on `main`), reflect it in the spec §1.2 before Commit 6.
- Confirm the acki-nacki-side `helpers/proof_helper/src/gql_proof.rs` GQL shape has not diverged since DEX branched. Bridge's `bridge-event-witness/enrich.rs` will consume the same shape.
- Reserve a shellnet slot for the Commit 8 E2E dry-run; carry the shellnet quirks list from user memory (msig amounts, USDCBridge.mintAndSend, dapp address form, faucet timing).

Once these are cleared, Commit 1 (adding `block_id_tree.rs`) is a purely-additive first step that unblocks the rest of the sequence.
