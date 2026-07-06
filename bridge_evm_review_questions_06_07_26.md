# bridge-EVM — Review Questions

**To:** Pruvendo / Sergey Egorov  
**From:** Alina (AN-side circuits + prover)  
**Scope:** `bridge` review from the AN→ETH side.

# Round 3 (2026-07-06) — `contracts/ethereum/`

**Scope:** Follow-up on `contracts/ethereum/` after the SHPLONK aggregator landing on `pruvendo/shellnet-e2e-landing`. One blocking item (AB-Q3, Circuit 4 `.bin`), two housekeeping items.

## AB-Q1 — `withdrawByProof` hardcodes `WITHDRAW_ANCHOR_LAYER = 1`; generalize to arbitrary anchor layer

**State.** `AckiNackiBridge.sol:74` defines `uint8 internal constant WITHDRAW_ANCHOR_LAYER = 1;` and `:968` restricts `withdrawByProof` to a single L1 anchor scan (`_isKnownLayerAnchor(WITHDRAW_ANCHOR_LAYER, pub.finalRoot)`). Correct today because the AN witness builder always anchors withdrawal proofs at L1; will silently reject valid L≥2 proofs the moment the partner-side witness lifts that restriction.

**Preferred design.** Extend Circuit 4 public inputs with `anchorLayer` at slot [10] (`WithdrawalPublicInputs` grows one `uint256` field). Bridge reads `pub.anchorLayer`, range-checks `1 ≤ anchorLayer ≤ MAX_LAYER_HASHES`, then `_isKnownLayerAnchor(pub.anchorLayer, pub.finalRoot)`. Layer↔`finalRoot` binding stays cryptographic (constrained inside Circuit 4), no attacker-supplied layer selector. Costs: one round of Circuit 4 re-keygen + aggregator re-export + `.bin` refresh (already in the M4 queue).

**Questions.**

1. Confirm Option A (PI[10] `anchorLayer`) is still the plan — the source comment at `:967` foreshadows it; the alternative of a calldata `anchorLayer` argument (unbound by proof) is rejected as it collapses the per-layer separation gained by AB-Q3 (Round 2).
2. Can it land in the same PR as the pending M4 Circuit 4 `.bin` refresh, or should it be a separate follow-up commit after M4 stabilizes?
3. Keep `WITHDRAW_ANCHOR_LAYER` as a legacy constant for backward-compat tests, or delete it once PI[10] lands?

Cross-reference: this is a compacted restatement of Round 2 AB-Q3.

## AB-Q2 — Delete dead Solidity: Groth16 residue + Blake2b/direct-Halo2 exploration files

**State.** Round 2 AB-Q2 flagged the asymmetric Groth16 cleanup (1B was cleaned, 1A/2/4 were not). None of that has landed on `pruvendo/shellnet-e2e-landing` as of today, and a second class of dead code was missed in Round 2: the Blake2b / direct-Halo2 exploratory verifiers, which are wired to no bridge and no deploy script.

Runtime wiring today: `AckiNackiBridge.sol` imports `IPrimaryVerifier`, `IFallbackVerifier`, `ILayerHashesMovementVerifier`, `IBridgeWithdrawalVerifier`; all four are instantiated by `ShplonkDeployLib` from the aggregator adapters (`PrimaryAggregatorVerifier`, `FallbackAggregatorVerifier`, `LayerHashesAggregatorVerifier`, `BridgeWithdrawalAggregatorVerifier`). Everything below is reachable only from other stale files or from tests that mirror the pre-SHPLONK path.

**Full deletion list (verified via import graph).**

| Bucket | File | Why stale |
|---|---|---|
| Groth16 adapter | `src/PrimaryVerifier.sol` | Replaced by `PrimaryAggregatorVerifier`; only imported by 3 legacy tests |
| Groth16 adapter | `src/LayerHashesMovementVerifier.sol` | Replaced by `LayerHashesAggregatorVerifier`; only imported by 3 legacy tests |
| Groth16 adapter | `src/BridgeWithdrawalVerifier.sol` | Replaced by `BridgeWithdrawalAggregatorVerifier`; no non-self imports |
| gnark-generated | `src/PrimaryGroth16VerifierGenerated.sol` | Only referenced from `PrimaryVerifier.sol` + its tests |
| gnark-generated | `src/LayerHashesGroth16VerifierGenerated.sol` | Only referenced from `LayerHashesMovementVerifier.sol` + its tests |
| Groth16 interface | `src/IPrimaryGroth16Verifier.sol` | Only referenced from `PrimaryVerifier.sol` |
| Groth16 interface | `src/ILayerHashesGroth16Verifier.sol` | Only referenced from `LayerHashesMovementVerifier.sol` |
| Groth16 interface | `src/IBridgeWithdrawalGroth16Verifier.sol` | Only referenced from `BridgeWithdrawalVerifier.sol` |
| Direct-Halo2 exploration | `src/Halo2Verifier.sol` | Never imported by `AckiNackiBridge.sol` or any deploy script; tested only by `Halo2PoseidonVerifier.t.sol` + `FuzzVerifiers.t.sol` |
| Direct-Halo2 exploration | `src/Blake2bHalo2Verifier.sol` | Same — direct on-chain Blake2b-transcript halo2 verifier, superseded by SHPLONK aggregator path |
| Direct-Halo2 exploration | `src/Blake2bChallengeComputer.sol` | Only used by `Blake2bHalo2Verifier.sol` + its test |
| Direct-Halo2 exploration | `src/Blake2bTranscript.sol` | Only used by `Blake2bChallengeComputer.sol` |
| Groth16 test | `test/PrimaryVerifier.t.sol` | Tests the Groth16 adapter |
| Groth16 test | `test/LayerHashesMovementVerifier.t.sol` | Tests the Groth16 adapter |
| Groth16 test | `test/AckiNackiBridgeVerifyBlock.t.sol` | Exercises Groth16 path; SHPLONK equivalent lives in `AckiNackiBridgeProductionVerifyBlock.t.sol` |
| Groth16 test | `test/FuzzAckiNackiBridgeVerifyBlock.t.sol` | Same — Fuzz variant against Groth16 verifiers |
| Direct-Halo2 test | `test/Blake2bHalo2Verifier.t.sol` | Tests dead code |
| Direct-Halo2 test | `test/Halo2PoseidonVerifier.t.sol` | Tests dead `Halo2Verifier.sol` |
| Direct-Halo2 test | `test/FuzzVerifiers.t.sol` | Fuzzes dead `Halo2Verifier.sol` |
| Direct-Halo2 fixture | `test/blake2b_verifier_bytecode.bin`, `test/halo2_verifier_bytecode.bin`, `test/halo2_proof_calldata.bin` | Pre-compiled bytecode for the dead verifiers above |

**Stale comments (not files, but same commit welcome):**

- `script/DeployRealBridge.s.sol:17` — `@dev AN→ETH verifiers: R15 SHPLONK aggregators for 1A + 2; gnark Groth16 for 1B fallback.` 1B is SHPLONK.
- `script/DeployRealBridge.s.sol:193` — `console.log("FallbackVerifier (Groth16):", ...)`. Label is Groth16, wired value is SHPLONK.

**Reasoning (why now).** Two production surfaces (Solidity src + Foundry tests) currently document a Groth16 backend that no deploy script wires and no bridge function calls. Auditors and integrators reading `contracts/ethereum/src/` first must cross-check `ShplonkDeployLib` + `AckiNackiBridge.sol` to disambiguate. Deleting the files above removes the ambiguity without touching any production path — every entry in the table has a verified-empty inbound import graph from production code (`src/AckiNackiBridge.sol`, `script/*`, `test/AckiNackiBridge*ProductionVerifyBlock*` / `*ApplyBkSetUpdate*` / `*LayerAnchor*` / `*Pause*` / `*WithdrawByProof*` / `*RelayerLoop*`, `test/ShplonkDeployLib.t.sol`, `test/ShplonkAggregatorForgery.t.sol`, `test/ShplonkSpikeOnChain.t.sol`). SHPLONK-facing tests already exist for every capability the Groth16 tests exercise (`AckiNackiBridgeProductionVerifyBlock.t.sol` + the Shplonk/Deploy/Forgery suite), so coverage is not lost.

**Questions.**

1. Any objection to landing the full deletion list above (18 files + 3 fixture bins) plus the two-line NatSpec/console-log sweep in a single commit? If a Foundry test in the list is retained deliberately (forgery corpus, historical regression, etc.), please flag it before deletion.
2. Should the 3 Blake2b/direct-Halo2 exploration `src/` files stay in the tree under `contracts/ethereum/experiments/` (or a similar clearly-labelled subdir) for reference, or be deleted outright? Preference is deletion — the git history preserves them if ever needed.
3. Confirm `gnark-wrappers/circuit-1a/`, `circuit-2/`, `circuit-4/` (called out in Round 2 AB-Q2 §3) belong in the same cleanup PR.

## AB-Q3 — Circuit 4 SHPLONK Yul verifier (`BridgeWithdrawalAggregatorVerifier.bin`) missing — blocks real `withdrawByProof` wiring

**State.** `contracts/ethereum/verifiers/` ships three SHPLONK `.bin` artefacts (Primary, Fallback, LayerHashes). Circuit 4 is absent; `README.md:10` marks it `TBD (M4)`. Downstream glue is fully written and idle:

- `ShplonkDeployLib.sol:50-54, 88-94` — `withdrawalBinPath()` + `deployWithdrawalAdapter()` ready to consume `verifiers/BridgeWithdrawalAggregatorVerifier.bin`.
- `src/BridgeWithdrawalAggregatorVerifier.sol` + `IBridgeWithdrawalVerifier` present in the source tree.
- Both `DeployRealBridge.s.sol:295-308` and `DeployShellnetE2EBridge.s.sol:82-87` gate Circuit 4 wiring behind `WIRE_WITHDRAW_BY_PROOF`; when false, `bridgeWithdrawalVerifier = address(0)` (real `withdrawByProof` calls revert). Shellnet E2E works only via `MockBridgeWithdrawalVerifier` (`test/mocks/`), not a real SHPLONK verifier.

**Circuit is stable, not the blocker.** `acki-nacki-to-eth-bridge-halo2-circuits/bridge-event-prove-circuit/src/bridge_event_prove_circuit.rs:123` fixes `pub const TOTAL_PUBLIC_INPUTS: usize = 10;` — the layout has been settled since the circuit landed (5 `compute_leading_public_inputs` call sites, `assert_eq!(instances.len(), TOTAL_PUBLIC_INPUTS)` throughout). Nothing on the AN side is holding back Phase-1/2/3 generation of the C4 aggregator. Note: AB-Q1 (`anchorLayer` PI[10]) is a proposed future extension — it is *not* a decided change and should not delay the initial C4 `.bin` at its current 10-PI layout.

**Concerns.**

1. **Withdrawal path is production-inert.** Every `withdrawByProof` on any deployed bridge today is either (a) reverting (verifier at `address(0)`) or (b) trusting a mock that returns `true`. AN→ETH withdrawal is E2E-demonstrated only under the mock.
2. **Same three-phase pipeline as 1A/1B/2 — no new machinery needed.** Phase-1 native prove exists (`bridge-event-prove-circuit`), Phase-2 wrap (`export-halo2-poseidon-snark`) is generic, Phase-3 aggregator (`export-inner-aggregator`) is parameter-only per-circuit (outer `k`, `lookup_bits`, `universality` — presets in `aggregator.rs:86-117` already list `withdrawal`). Producing the `.bin` is an ops task, not a design task.
3. **EIP-170 headroom is unknown until run.** 10 PIs is between 1A/1B (4) and Layer-Hashes (14). Expected Yul size ~19–22 KB — likely fits, but not yet measured; `eip170.rs` gate would catch it at export time.

**Required script updates once C4 `.bin` lands.** Both deploy scripts currently ship the withdrawal verifier as `address(0)` in their default branch — this is a *temporary workaround* for the missing `.bin`, not the intended production shape.

- `script/DeployShellnetE2EBridge.s.sol:53-64, 82-88` — `wireWithdraw = vm.envOr("WIRE_WITHDRAW_BY_PROOF", false)` gates the C4 adapter deploy. When `false` (the default and today's only working path), `wd.verifier = IBridgeWithdrawalVerifier(address(0))` is passed into the bridge constructor. Fix required: once the `.bin` is in-tree, this script should either (a) flip the default to `true` so shellnet Sepolia deploys wire the real C4 verifier out of the box, or (b) remove the flag entirely and always deploy the adapter (the flag is purely a stopgap for the missing artefact).
- `script/DeployRealBridge.s.sol:122, 131-138, 287-304` — same shape, additionally the fallback returns `bridgeWithdrawalVerifier: IBridgeWithdrawalVerifier(address(0))` inside `_buildWithdrawConfig(wire=false, ...)`. Same fix required: the `!wire` branch of `_buildWithdrawConfig` is a placeholder that must not survive M4. A production deploy that intentionally omits withdraw wiring would leave the bridge with a dead `withdrawByProof` — not a valid production configuration.
- Stale Groth16 comments in `DeployRealBridge.s.sol` — lines 17 and 193 still document / log the retired Groth16 fallback for Circuit 1B, which is now SHPLONK. Fix in the same commit (also captured in AB-Q2).

**Questions.**

1. What is the current owner + ETA for producing `verifiers/BridgeWithdrawalAggregatorVerifier.{bin,sol,_calldata.bin}` at the fixed 10-PI layout? Is this in Pruvendo's queue or waiting on an AN-side hand-off? If hand-off is required, which artefact (specific `.vk` / `.snark`)?
2. If the artefact exists but hasn't been committed (e.g. n14 produced it, not yet pushed), can it be landed against the current shellnet-e2e-landing HEAD without further layout changes?
3. Once the `.bin` lands, is the plan to flip `WIRE_WITHDRAW_BY_PROOF=true` on the shellnet Sepolia deploy in the same PR, or as a separate deploy step? And in the same PR, remove the `!wire` zero-address fallback from `_buildWithdrawConfig` in `DeployRealBridge.s.sol` — the withdrawal verifier should be mandatory in a production deploy, not env-gated.
4. Should the AB-Q1 (`anchorLayer` PI) proposal be explicitly deferred to a *second* C4 aggregator refresh, so it doesn't block the first `.bin` from landing at 10 PIs?
5. Confirm the two stale Groth16 references in `DeployRealBridge.s.sol` (line 17 NatSpec, line 193 log label) are swept in the M4 PR alongside the C4 wiring — they mislead anyone reading the deploy script into thinking 1B is still Groth16.

## AB-Q4 — `bridge-prover-orchestrator` cleanup: retire gnark/Groth16 residue + collapse duplicated key managers

**Scope.** This item lives entirely under `crates/bridge-prover-orchestrator/` (Sergey's ownership); no `contracts/ethereum/` code is touched. Two independent cleanups that should land together — both are code that pre-dates the R15 SHPLONK landing and no longer has a production consumer now that 1A/1B/2 all ship via `ShplonkDeployLib`.

### AB-Q4.a — Delete all gnark Groth16 residue from the orchestrator

**State.** The gnark hybrid path is fully retired at the on-chain layer (see AB-Q2 for the Solidity-side deletions). The orchestrator still ships the Rust code and Go wrappers that fed it. None of it is used by any current prover/verifier binary, the SHPLONK export pipeline (`export-bound-block-proofs` → `export-bound-poseidon-snarks` → `export-inner-aggregator`), or any test on `pruvendo/shellnet-e2e-landing`.

**Full deletion list.**

| Bucket | Path | Why stale |
|---|---|---|
| gnark wrappers (Go) | `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-1a/` (whole dir) | Consumed only the `halo2_proof.json` written by `export_primary_proof` for the Groth16 hybrid; SHPLONK export path bypasses gnark entirely |
| gnark wrappers (Go) | `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-2/` (whole dir) | Same, for Circuit 2 |
| gnark wrappers (Go) | `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-4/` (whole dir) | Same, for Circuit 4 (also called out in Round 2 AB-Q2 §3) |
| gnark wrappers (Go) | `crates/bridge-prover-orchestrator/gnark-wrappers/SECURITY.md` | Documents the retired Go layer |
| Rust export helper | `crates/bridge-prover-orchestrator/src/proof_export.rs` | Formats halo2 proofs as gnark-consumable JSON (`Fr` decimal, per-witness `halo2_proof.json`). SHPLONK path uses raw `.bin` calldata (`export_inner_aggregator`), no JSON hop |
| Rust binary | `crates/bridge-prover-orchestrator/src/bin/export_primary_proof.rs` | Emits `halo2_proof.json` for gnark's Circuit-1A wrapper; superseded by `export-bound-poseidon-snarks` (Poseidon-transcript snark → aggregator) |
| Rust binary | `crates/bridge-prover-orchestrator/src/bin/export_fallback_proof.rs` | Same, for the Circuit-1B gnark wrapper |
| Rust binary | `crates/bridge-prover-orchestrator/src/bin/export_layer_hashes_proof.rs` | Same, for the Circuit-2 gnark wrapper |
| Rust binary | `crates/bridge-prover-orchestrator/src/bin/mock_prove_bound_layer.rs` | Blake2b mock-prover driver for the pre-SHPLONK Circuit 2 exploration; not called by any test or CI job today. Please double-check before deletion — if it is still useful as a dev harness, at minimum drop the gnark-facing serialisation |
| Stale comment | `crates/bridge-prover-orchestrator/src/bound_test_data.rs:11` | `//! ... SHPLONK / gnark Groth16 proofs ...` — trim the gnark clause |

**Reasoning.** The gnark hybrid was the fallback plan that let Circuit 1B ship before the R15 SHPLONK aggregator was in place. Since the aggregator now lands 1A/1B/2 on-chain at ≤21 KB Yul under EIP-170 (see `contracts/ethereum/verifiers/README.md`), the hybrid is architecturally dead. Keeping the export binaries + Go wrappers alive creates a plausible-looking alternative pipeline that misleads new contributors — every hop in that pipeline is a footgun.

**Questions.**

1. Any objection to deleting the 8 items in the table above in a single commit? If any of the Rust binaries are still called by an out-of-tree scaffold (developer CLI script, ad-hoc Sepolia deploy notebook, etc.), please flag it before deletion — the ask is only for what's demonstrably dead from `pruvendo/shellnet-e2e-landing`.
2. `bin/mock_prove_bound_layer.rs` is the only entry I'm unsure about — it's Blake2b-transcript-flavoured and appears unwired, but it may still be useful as a dev harness. Delete outright, or keep and just retire the gnark-side JSON serialisation?
3. Confirm this cleanup can land as a separate PR from AB-Q2 (Solidity-side gnark deletion) — the two touch disjoint file sets and rebase independently.

### AB-Q4.b — Collapse the duplicated `FallbackKeyManager` and `LayerHashesKeyManager` into `bridge-prover-lib`

**State.** Two files in the orchestrator are near-identical copies of code that already lives in `bridge-prover-lib`:

| Orchestrator file | LOC | What it duplicates | Actual delta |
|---|---:|---|---|
| `crates/bridge-prover-orchestrator/src/keys.rs` | 244 | `bridge_prover_lib::keys::fallback::FallbackKeyManager` | Overrides `K` from `20` (prover-lib historic default) to `21` (EIP-170 requirement for the SHPLONK aggregator Yul) — the *only* meaningful difference |
| `crates/bridge-prover-orchestrator/src/layer_hashes_keys.rs` | 225 | `bridge_prover_lib::keys::layer::LayerHashesKeyManager` | **Zero** substantive divergence — imports the same `K=17` constant, uses the same lookup width, the same synthetic-witness path. Pure copy-paste |

**Upstream change already landed (`bridge-prover-lib`).** As of the same PR carrying this doc, `bridge-prover-lib`'s `keys` module is refactored:

- Old monolithic `pub struct KeyManager` split into four per-circuit managers under `bridge_prover_lib::keys::{primary::PrimaryKeyManager, fallback::FallbackKeyManager, layer::LayerHashesKeyManager, event::EventKeyManager}`.
- Each manager has `pub const DEFAULT_K` (Primary 20, Fallback **21** — SHPLONK-production default, Layer 17, Event 19), plus `new(&Path)` at the default and `new_with_k(&Path, k)` for explicit control.
- The old `KeyManager` type stays as a thin facade (`pub struct KeyManager { pub params_dir, pub srs, pub primary, pub fallback, pub layer, pub event }`) with every historic accessor (`primary_vk()`, `ensure_fallback_keys()`, `load_layer_pk()`, ...) forwarded to the corresponding sub-manager. Orchestrator's current imports (`use bridge_prover_lib::keys::KeyManager;` + method calls in `primary_prover.rs`, `circuit4_prover.rs`, `bin/export_primary_proof.rs`, `bin/export_bound_block_proofs.rs`, `bin/export_bound_poseidon_snarks.rs`) continue to compile without modification.
- Ceremony-file / on-disk cache format (`primary_vk.bin`, `fallback_pk.bin`, `layer_config_params.json`, …) is byte-identical — no migration.

**Preferred migration for orchestrator.**

1. **Delete** `crates/bridge-prover-orchestrator/src/keys.rs` (whole file) and `crates/bridge-prover-orchestrator/src/layer_hashes_keys.rs` (whole file).
2. **Delete** the corresponding declarations from `crates/bridge-prover-orchestrator/src/lib.rs` (`pub mod keys;`, `pub mod layer_hashes_keys;`, plus the re-exports `pub use keys::FallbackKeyManager;` and `pub use layer_hashes_keys::{LayerHashesKeyManager, ...};`).
3. **Redirect** all `use crate::keys::FallbackKeyManager;` and `use crate::layer_hashes_keys::LayerHashesKeyManager;` to `use bridge_prover_lib::keys::FallbackKeyManager;` and `use bridge_prover_lib::keys::LayerHashesKeyManager;`.
4. **Constructor calls** stay the same shape — `FallbackKeyManager::new(&params_dir)` now uses the K=21 SHPLONK-production default from `bridge-prover-lib`; likewise `LayerHashesKeyManager::new(&params_dir)` at K=17. If the orchestrator ever wants a different K for a one-off experiment, call `::new_with_k(&params_dir, k)` explicitly instead.
5. Constants historically re-exported from `layer_hashes_keys` (`LAYER_HASHES_K`, `LAYER_HASHES_LOOKUP_BITS`, `LAYER_HASHES_NUM_UNUSABLE_ROWS`) — replace call sites with `LayerHashesKeyManager::DEFAULT_K`, `<mgr>.lookup_bits()`, `<mgr>.num_unusable_rows()`. Or, if a constant is genuinely needed outside the manager (test data generators?), lift it to `bridge-prover-lib` in the same PR.

**Reasoning.** The whole reason `bridge-prover-orchestrator/src/keys.rs` exists is that `bridge-prover-lib`'s pre-split `KeyManager` hardcoded `K=20` for fallback and the orchestrator needed `K=21` for the SHPLONK aggregator Yul to fit EIP-170. That's now a `const DEFAULT_K` on the per-circuit manager — no need to duplicate the whole file to change one number. `layer_hashes_keys.rs` never had *any* divergence at all; it was a copy-paste and its continued existence has been actively confusing (double book-keeping of `LAYER_HASHES_K`, two config JSON writers pointing at the same `layer_config_params.json`, etc.). Net LOC delta after migration: **−469 LOC** in orchestrator, zero LOC change in downstream callers (only import-path rewrites).

**Questions.**

1. Any objection to the exact migration steps above (delete the two files, rewrite the ~5 import lines, keep on-disk artefact filenames intact)? Constructors and public method surfaces are unchanged, so `bin/*` binaries and integration tests (`tests/fallback_round_trip.rs`, `tests/layer_hashes_round_trip.rs`, `tests/halo2_tvm_bundle_round_trip.rs`) should just rebuild.
2. The upstream refactor picked `FallbackKeyManager::DEFAULT_K = 21` (SHPLONK production, matches today's orchestrator override). If any orchestrator dev-path needs the legacy `K=20` native fallback, call `::new_with_k(&params_dir, 20)` explicitly at that site — please confirm which, if any, of the current fallback constructors were relying on K=20 (grep suggests none). The bridge-prover-daemon's monolithic `KeyManager` now uses the K=21 fallback default too, so the daemon and orchestrator produce compatible fallback `.bin` artefacts (previously the daemon wrote K=20 and the orchestrator wrote K=21 to the same `fallback_pk.bin` — silent divergence hazard). If this is undesirable for the daemon's legacy native-only path, flag it and we'll add an explicit K=20 constructor call there.
3. Preference on where the `LAYER_HASHES_*` constants live long-term: on `LayerHashesKeyManager` as `pub const DEFAULT_K` + `lookup_bits()` accessor (current shape), or as free `pub const`s at `bridge_prover_lib::keys::layer::LAYER_HASHES_*` for import-compatibility with existing orchestrator callers? Only affects the mechanical part of the migration.
4. Can AB-Q4.a (gnark deletion) and AB-Q4.b (key-manager consolidation) land in the same PR, or would you prefer them split? They touch disjoint files inside the orchestrator so either shape works; a combined PR is smaller net diff.

## AB-Q5 — `bridge-prover-orchestrator` cleanup: retire duplicated prover/verifier code, migrate to `bridge-prover-lib` `_with_transcript` API

**Scope.** Same ownership boundary as AB-Q4 — everything below lives under `crates/bridge-prover-orchestrator/`. Independent from AB-Q4.a (gnark) and AB-Q4.b (key managers); can land in the same PR or separately. This item retires the five prover/verifier files that duplicate `bridge-prover-lib` and `bridge-event-prover-lib` code — the only real functional difference (Poseidon transcript) has now been landed upstream on the AN-side crates.

### What landed upstream (available to orchestrator today)

The same PR carrying this doc adds first-class Poseidon-transcript support in `bridge-prover-lib` and `bridge-event-prover-lib`:

- **New module** `bridge_prover_lib::transcript` with:
  - `TranscriptKind` — `#[repr(u8)]` enum `Blake2b = 0, Poseidon = 2` (wire-value-stable, `from_u8` / `as_u8` for on-wire round-trips). This is the same enum that today lives in `orchestrator::halo2_tvm_bundle::TranscriptKind`; the on-wire values are identical so `VkBlob` header parsing keeps working after orchestrator switches its import to `bridge_prover_lib::transcript::TranscriptKind` (see migration step 5 below).
  - `PoseidonRead<R>` / `PoseidonWrite<W>` / `PoseidonChallenge` — verbatim port of `orchestrator::poseidon_transcript::*`. Same `OptimizedPoseidonSpec` (`T=3, RATE=2, R_F=8, R_P=57, SECURE_MDS=0`), same absorption order, same on-wire encoding. Byte-for-byte compatible with `snark-verifier-sdk`'s `PoseidonTranscript<NativeLoader, _>` — this is what preserves the "prove in gosh-halo2-lib workspace, feed into axiom-crypto/halo2-lib `AggregationCircuit`" bridge that R15's aggregator pipeline needs.

- **New `_with_transcript` sibling functions** on every prover/verifier that has an orchestrator counterpart:

  | Existing (Blake2b default) | New sibling |
  |---|---|
  | `bridge_prover_lib::prover::generate_primary_proof` | `generate_primary_proof_with_transcript(…, TranscriptKind)` |
  | `bridge_prover_lib::prover::generate_fallback_proof` | `generate_fallback_proof_with_transcript(…, TranscriptKind)` |
  | `bridge_prover_lib::layer_prover::generate_layer_proof` | `generate_layer_proof_with_transcript(…, TranscriptKind)` |
  | `bridge_prover_lib::layer_prover::generate_layer_proof_with_input` | `generate_layer_proof_with_input_and_transcript(…, TranscriptKind)` |
  | `bridge_prover_lib::verifier::verify_primary_proof` | `verify_primary_proof_with_transcript(…, TranscriptKind)` |
  | `bridge_prover_lib::verifier::verify_fallback_proof` | `verify_fallback_proof_with_transcript(…, TranscriptKind)` |
  | `bridge_prover_lib::verifier::verify_layer_proof` | `verify_layer_proof_with_transcript(…, TranscriptKind)` |
  | `bridge_prover_lib::verifier::verify_kzg_proof` | `verify_kzg_proof_with_transcript(…, TranscriptKind)` (shared core, re-exported for event-lib) |
  | `bridge_event_prover_lib::prover::generate_event_proof` | `generate_event_proof_with_transcript(…, TranscriptKind)` |
  | `bridge_event_prover_lib::prover::generate_event_proof_from_circuit` | `generate_event_proof_from_circuit_with_transcript(…, TranscriptKind)` |
  | `bridge_event_prover_lib::verifier::verify_event_proof` | `verify_event_proof_with_transcript(…, TranscriptKind)` |

  In every case the Blake2b entry point is now a thin wrapper that calls `_with_transcript(TranscriptKind::Blake2b)`, so the existing AN-facing API is unchanged (daemons, tests, cross-repo callers keep compiling).

- **Signature note** — orchestrator's `generate_circuit4_proof_with_transcript` takes `&mut KeyManager` and calls `ensure_event_keys` + `load_event_pk` internally; `bridge_event_prover_lib::prover::generate_event_proof_with_transcript` follows prover-lib's shared convention of taking `&KeyManager` and requiring the caller to have the event PK loaded (`key_manager.load_event_pk()?` then `unload_event_pk()` after — same on-demand pattern the daemon uses for Primary/Layer PKs). Migration step 3 below adjusts the two orchestrator bin call sites accordingly.

### Full deletion list

| Bucket | Path | LOC | What it duplicates |
|---|---|---:|---|
| Poseidon transcript | `src/poseidon_transcript.rs` | 581 | `bridge_prover_lib::transcript::poseidon::*` (verbatim port; unit tests included) |
| Circuit 1A prover (Poseidon+Blake2b dispatch) | `src/primary_prover.rs` | 179 | `bridge_prover_lib::prover::generate_primary_proof_with_transcript` |
| Circuit 1B prover (Poseidon+Blake2b dispatch) | `src/prover.rs` | 216 | `bridge_prover_lib::prover::generate_fallback_proof_with_transcript` |
| Circuit 1B verifier (Poseidon+Blake2b dispatch) | `src/verifier.rs` | 90 | `bridge_prover_lib::verifier::verify_fallback_proof_with_transcript` |
| Circuit 2 prover+verifier (Poseidon+Blake2b dispatch) | `src/layer_hashes_prover.rs` | 273 | `bridge_prover_lib::layer_prover::generate_layer_proof_with_transcript` + `bridge_prover_lib::verifier::verify_layer_proof_with_transcript` |
| Circuit 4 prover (Poseidon+Blake2b dispatch) | `src/circuit4_prover.rs` | 103 | `bridge_event_prover_lib::prover::generate_event_proof_from_circuit_with_transcript` |
| Total | | **1,442** | |

`halo2_tvm_bundle.rs`, `halo2_snark.rs`, `bound_test_data.rs`, `layer_hashes_test_data.rs`, and the SHPLONK-pipeline `bin/*` binaries (`export_bound_block_proofs`, `export_bound_poseidon_snarks`, `export_halo2_poseidon_snark`, `mock_prove_bound_layer`) are intentionally **left in place**. Those are wire-format (`VkBlob`), pipeline-orchestration, and test-fixture code that doesn't overlap with `bridge-prover-lib` — no consolidation value in moving them.

### Preferred migration for orchestrator

1. **Delete** all six files listed above (`src/poseidon_transcript.rs`, `src/primary_prover.rs`, `src/prover.rs`, `src/verifier.rs`, `src/layer_hashes_prover.rs`, `src/circuit4_prover.rs`).

2. **Delete** the corresponding declarations from `crates/bridge-prover-orchestrator/src/lib.rs` (`pub mod poseidon_transcript;`, `pub mod primary_prover;`, `pub mod prover;`, `pub mod verifier;`, `pub mod layer_hashes_prover;`, `pub mod circuit4_prover;`) and the re-exports at the bottom (`pub use poseidon_transcript::{PoseidonChallenge, PoseidonRead, PoseidonWrite};`, `pub use primary_prover::{generate_primary_proof, generate_primary_proof_with_transcript, PrimaryProofOutput};`, `pub use prover::{generate_fallback_proof, generate_fallback_proof_with_transcript, FallbackProofOutput};`, `pub use verifier::{verify_fallback_proof, verify_fallback_proof_with_transcript};`, `pub use layer_hashes_prover::{…};`, `pub use circuit4_prover::{…};`).

3. **Redirect** all remaining call sites in the retained orchestrator files (`bin/export_bound_block_proofs.rs`, `bin/export_bound_poseidon_snarks.rs`, `bin/export_halo2_poseidon_snark.rs`, `bin/mock_prove_bound_layer.rs`, `bound_test_data.rs`, `layer_hashes_test_data.rs`, `halo2_tvm_bundle.rs`):

   | Old orchestrator import / call | New AN-side call |
   |---|---|
   | `use crate::poseidon_transcript::{PoseidonRead, PoseidonWrite, PoseidonChallenge};` | `use bridge_prover_lib::transcript::{PoseidonRead, PoseidonWrite, PoseidonChallenge};` |
   | `use crate::primary_prover::{generate_primary_proof, generate_primary_proof_with_transcript};` | `use bridge_prover_lib::prover::{generate_primary_proof, generate_primary_proof_with_transcript};` |
   | `use crate::prover::{generate_fallback_proof, generate_fallback_proof_with_transcript};` | `use bridge_prover_lib::prover::{generate_fallback_proof, generate_fallback_proof_with_transcript};` |
   | `use crate::verifier::{verify_fallback_proof, verify_fallback_proof_with_transcript};` | `use bridge_prover_lib::verifier::{verify_fallback_proof, verify_fallback_proof_with_transcript};` |
   | `use crate::layer_hashes_prover::{generate_layer_hashes_proof, generate_layer_hashes_proof_with_transcript, verify_layer_hashes_proof, verify_layer_hashes_proof_with_transcript, LayerHashesProofInput, LayerHashesProofOutput};` | `use bridge_prover_lib::layer_prover::{generate_layer_proof_with_input as generate_layer_hashes_proof, generate_layer_proof_with_input_and_transcript as generate_layer_hashes_proof_with_transcript, LayerHashesProofInput, LayerHashesProofOutput};` **plus** `use bridge_prover_lib::verifier::{verify_layer_proof as verify_layer_hashes_proof, verify_layer_proof_with_transcript as verify_layer_hashes_proof_with_transcript};` — the AN-side names differ (`layer_proof` vs. `layer_hashes_proof`); `as`-rename keeps orchestrator-side callers unchanged, or rename call sites for consistency |
   | `use crate::circuit4_prover::{generate_circuit4_proof, generate_circuit4_proof_with_transcript, Circuit4ProofOutput};` | `use bridge_event_prover_lib::prover::{generate_event_proof_from_circuit as generate_circuit4_proof, generate_event_proof_from_circuit_with_transcript as generate_circuit4_proof_with_transcript, EventProofOutput as Circuit4ProofOutput};` — same-shape 10-PI Fr instances, so `EventProofOutput` (whose `public_instances: Vec<Fr>`) drop-in replaces `Circuit4ProofOutput` (`instances: [Fr; 10]`) if the two call sites in `bin/*` accept the widened type. If they need the fixed-length shape, keep a small local shim: `Circuit4ProofOutput { proof_bytes: out.proof_bytes, instances: out.public_instances.try_into().unwrap() }` |

4. **Adjust the two Circuit 4 call sites** (`bin/export_bound_block_proofs.rs`, or wherever `generate_circuit4_proof_with_transcript(&mut key_manager, …)` is invoked) to the `&KeyManager` shape: hoist `key_manager.ensure_event_keys()?;` + `key_manager.load_event_pk()?;` to just before the call, and add `key_manager.unload_event_pk();` after the proof is emitted (matches the daemon's Primary/Layer pattern). Total delta: 3 lines added per call site.

5. **`TranscriptKind` in `halo2_tvm_bundle.rs`** — the orchestrator's `halo2_tvm_bundle.rs` currently declares its own `TranscriptKind` enum. Recommended: replace the local declaration with `pub use bridge_prover_lib::transcript::TranscriptKind;` (the wire values `0 = Blake2b, 2 = Poseidon` match by design). This lets `VkBlob::write`/`read` continue to serialise the exact same header byte while dropping ~20 LOC of duplicate enum. Optional — if you'd rather keep `halo2_tvm_bundle` fully self-contained for stability of the on-wire format, leave the local enum in place and just add a `From<bridge_prover_lib::transcript::TranscriptKind>` at call sites where the two need to bridge.

6. **Verify** by running `cargo check --all-targets` in the orchestrator; the `bin/*` binaries + `halo2_tvm_bundle_round_trip.rs` / `bound_snark_pipeline.rs` tests should just rebuild (no on-wire format change).

### Reasoning

Every file in the deletion list is a Poseidon-transcript extension of code that already existed in `bridge-prover-lib` under a Blake2b-only interface. The historical reason for that split was ownership: the AN-side crates targeted the AN VM's `ZKHALO2VERIFYWITHVK` opcode (Blake2b-only), and the SHPLONK aggregator pipeline needed Poseidon transcripts, so R15's aggregator work happened in `bridge-prover-orchestrator` to avoid churning AN-facing code paths.

With R15 stabilized and the aggregator pipeline running against the same halo2 SHPLONK proof internals as the AN-facing daemons, keeping two parallel copies of the same `create_proof(…, TranscriptKind::{Blake2b, Poseidon})` dispatch across two crates is now pure drift risk. Concrete failure modes observed elsewhere in the repo (see the `LayerHashesKeyManager` duplication called out in AB-Q4.b) — future changes to circuit constants, key params, or halo2 API drift have to be applied in two places and silent divergence eventually happens.

The AN-side `_with_transcript` API is a superset of the orchestrator's — same `TranscriptKind` values, same on-wire proof bytes, same public-instance layouts. Migration is entirely import-path rewrites plus the one `&mut → &KeyManager` shape adjustment for Circuit 4 in step 4. Net LOC delta after migration: **−1,442 LOC** in orchestrator, zero LOC change in downstream callers.

### Post-migration architecture

Once AB-Q4 + AB-Q5 land, `bridge-prover-orchestrator/src/` is expected to contain **only**:

- `halo2_tvm_bundle.rs` — TVM wire format (`VkBlob`, `Halo2TvmOperands`) for the on-chain `ZKHALO2VERIFYWITHVK` opcode. AN-side callers don't need this today; leaving it in orchestrator keeps the concerns separated.
- `halo2_snark.rs` — small on-wire helper for the SHPLONK pipeline.
- `bound_test_data.rs` — bound-block test fixtures used by the pipeline `bin/*`. Could move to `bridge-test-data-gen` if useful; not required.
- (`layer_hashes_test_data.rs` is retired by AB-Q6 below — the canonical fixture builder lives upstream in `bridge-test-data-gen::layer_hashes` at production tree depth 8.)
- `bin/export_bound_block_proofs.rs`, `bin/export_bound_poseidon_snarks.rs`, `bin/export_halo2_poseidon_snark.rs`, `bin/mock_prove_bound_layer.rs` — the SHPLONK pipeline drivers themselves.
- `lib.rs` — a much thinner surface: pipeline entry points + `halo2_tvm_bundle` re-exports.

Every "generate a proof" / "verify a proof" concern lives in `bridge-prover-lib` + `bridge-event-prover-lib` at that point.

### Questions

1. Any objection to the deletion list above (6 files, 1,442 LOC) and the migration steps as written? If any of the six files still hosts orchestrator-specific behaviour I've missed (extra tracing, a diagnostic branch, an experimental strategy), please flag it before deletion — I only diff'd against orchestrator HEAD as of 2026-07-06 and prover-lib HEAD after the transcript refactor.
2. Preference on step 3 for the Circuit 2 & Circuit 4 name mismatch (`layer_hashes_proof` vs `layer_proof`, `Circuit4ProofOutput` vs `EventProofOutput`): `as`-rename in the `use` statement to keep local names stable, or rename call sites for cross-repo consistency? The `as`-rename is smaller diff; renaming call sites reads more cleanly for a new contributor.
3. Preference on step 5 (`TranscriptKind` in `halo2_tvm_bundle`): switch to `pub use bridge_prover_lib::transcript::TranscriptKind;`, or keep the local enum and thread `From` at call sites? The former is cleaner and matches how `bridge-prover-lib` already exposes the enum; the latter isolates the on-wire type from any future upstream enum reshape. My preference is the `pub use`, but either works.
4. Can AB-Q5 land in the same PR as AB-Q4.a (gnark deletion) + AB-Q4.b (key managers)? All three touch disjoint files inside the orchestrator so a single combined "orchestrator M4 cleanup" PR would be cleanest. Total delta: ~469 (Q4.b) + ~1,038 (Q4.a) + ~1,442 (Q5) = **~2,949 LOC** deleted from orchestrator, one commit.
5. If splitting is preferred, the natural ordering is AB-Q4.b (keys) first — it unblocks nothing but is the least risky. AB-Q5 (transcripts + provers) second — larger diff but purely mechanical. AB-Q4.a (gnark) third — retires production-inert code, no rush. Confirm this ordering is fine, or flag if a different sequence better matches your queue.

---

## AB-Q6 — Retire `bridge-prover-orchestrator/src/layer_hashes_test_data.rs`, switch to upstream `bridge-test-data-gen::layer_hashes`

**Scope.** Same ownership boundary as AB-Q4 / AB-Q5 — file lives under `crates/bridge-prover-orchestrator/`. Independent from the earlier cleanup items; can land in the same PR or separately.

### State

Orchestrator's `src/layer_hashes_test_data.rs` (183 LOC) is a byte-for-byte precursor of the canonical fixture builder that now lives in the circuits repo at `acki-nacki-to-eth-bridge-halo2-circuits/test-data-gen/src/layer_hashes.rs` — the upstream file even documents itself as the replacement:

> `layer_hashes.rs:254-255` — "It is the upstream replacement for the partner orchestrator's local `layer_hashes_test_data.rs`, and is fixed to production tree depth (8)."

The only substantive difference is the tree depth:

| Field | orchestrator local | `bridge-test-data-gen::layer_hashes` |
|---|---|---|
| `TREE_DEPTH` | **4** (lightweight, hardcoded; source comment: "we'll re-fixture later") | **8** (mainnet — `HISTORY_PROOF_WINDOW_SIZE = 128` ⇒ 130 leaves ⇒ pad to 2^8 = 256) |
| `CHAIN_LEAF_POSITION` | inline `2.min(...)` | named `pub const CHAIN_LEAF_POSITION: usize = 2` |
| Poseidon tree build | random-siblings-only chain | full tree with `build_tree_with_chain_leaf_depth` (matches production shape) |
| Public-instance count | re-exported from `crate::layer_hashes_prover` | defined locally as `1 + 1 + 1 + MAX_LAYERS + 1 = 14` |
| Tests | none | 4 unit tests |
| API | `build_synthetic_layer_hashes_input` + `SyntheticLayerHashesInput` | same names, superset of exports (also exposes lower-level `generate_layer_hash_chain_with_depth` + `LayerHashChainData`) |

Because `TREE_DEPTH=4` changes constraint count → different VK/PK, the orchestrator's local builder produces witnesses that are *not shape-compatible* with any deployable circuit configuration. The upstream builder is the only mainnet-acceptable source of Circuit 2 test data.

### Deletion list

| Kind | Path | Size |
|---|---|---|
| Rust module | `crates/bridge-prover-orchestrator/src/layer_hashes_test_data.rs` | 183 LOC |
| Re-export | `crates/bridge-prover-orchestrator/src/lib.rs:57` — `pub use layer_hashes_test_data::{build_synthetic_layer_hashes_input, SyntheticLayerHashesInput};` | 1 line |
| Module declaration | `crates/bridge-prover-orchestrator/src/lib.rs:20` — `pub mod layer_hashes_test_data;` | 1 line |

### Live consumers to redirect

`build_synthetic_layer_hashes_input` / `SyntheticLayerHashesInput` are referenced in exactly 3 places outside the module itself:

| Consumer | Fate |
|---|---|
| `src/lib.rs:57` — re-export | Delete with the module (this item). |
| `src/bin/export_layer_hashes_proof.rs:20,87,98` | Already scheduled for deletion under AB-Q4.a (gnark exporter chain). No import swap needed — file goes away. |
| `tests/layer_hashes_round_trip.rs:11,36,48` | Already scheduled for deletion under AB-Q5 (round-trip covered by `bridge-prover-lib` unit tests). No import swap needed. |

**No living consumer survives the AB-Q4.a + AB-Q5 cleanup** — meaning AB-Q6 is a *pure deletion* once those two land. If AB-Q6 is applied *before* AB-Q4.a/Q5 in a separate PR, the two consumer files need a one-line import rewrite:

```rust
// Before
use bridge_prover_orchestrator::{build_synthetic_layer_hashes_input, SyntheticLayerHashesInput};

// After (add bridge-test-data-gen to the crate's Cargo.toml first)
use bridge_test_data_gen::layer_hashes::{build_synthetic_layer_hashes_input, SyntheticLayerHashesInput};
```

Since the API names are identical, no call-site changes are needed. The only behavioural difference (`TREE_DEPTH: 4 → 8`) is *silent*: proof/witness sizes change, and any hardcoded proof-size assertions in tests would need updating. Neither `export_layer_hashes_proof` nor `layer_hashes_round_trip` inspect the tree depth or hardcode a shape, so the swap is drop-in.

### Doc-only follow-up

`src/bound_test_data.rs:265` mentions the file by name in a comment:

> "(see the synthetic builder in `layer_hashes_test_data.rs`, which mirrors the circuit and reverses before `bytes_le_to_fr`)"

Please retarget to `bridge_test_data_gen::layer_hashes::build_synthetic_layer_hashes_input` in the same PR — one-line comment edit, no code impact.

### Reasoning

Keeping a second (shape-inconsistent) copy of the Circuit 2 synthetic-input builder next to the canonical one is exactly the kind of drift AB-Q4 / AB-Q5 are cleaning up. The upstream file is authored explicitly as its replacement, is depth-correct for production, and has test coverage; the local copy has none. Deleting it removes a footgun where a new contributor could produce witnesses at TREE_DEPTH=4 that no shipped VK/PK will ever accept.

### Questions

1. Any objection to the deletion + upstream swap? The API contract is identical — same struct shape, same function signature; only `TREE_DEPTH` and internal chain-building details differ (both in the direction of production correctness).
2. Preference on landing sequence: fold AB-Q6 into the AB-Q4.a PR (both touch `export_layer_hashes_proof.rs`, both zero-consumer post-cleanup), or ship it as its own follow-on after Q4.a + Q5 land (pure deletion, no import edits at that point)?
3. Any orchestrator-internal reason `TREE_DEPTH=4` was preserved — e.g. a proof-generation benchmark that relies on the smaller shape for wall-time? I couldn't find one, but flagging in case there's an out-of-tree consumer I haven't seen.

---

## Appendix — Post-migration keygen + proof-gen flow (SHPLONK / Yul lane)

After AB-Q4/Q5/Q6 land, the R15 proving pipeline that produces the on-chain Yul verifiers looks like this. Byte-for-byte identical output to today; only import surface collapses.

### Ownership

- **Keys (all 4 circuits):** `bridge-prover-lib::keys::KeyManager`. Methods `ensure_{primary,fallback,layer,event}_keys` run keygen + persist `params_dir/<c>_{pk,vk}.bin` + `<c>_config.json`; `load_/unload_<c>_pk` handle multi-GB PK memory.
- **Proofs (Blake2b flavour — AN VM opcode side):** `bridge-prover-lib::{prover,layer_prover}::generate_*_proof`, `bridge-event-prover-lib::prover::generate_event_proof_from_circuit`.
- **Proofs (Poseidon flavour — SHPLONK aggregator side):** same functions, `..._with_transcript(TranscriptKind::Poseidon)` siblings. **Same PK/VK — only the Fiat–Shamir transcript differs.**
- **snark-verifier-sdk boundary:** `bridge-prover-orchestrator::halo2_snark::export_poseidon_snark` wraps `(vk, config, proof, instances)` into a bincode `Snark`.
- **SHPLONK aggregation + Yul emit:** `bridge-evm-aggregator::export-inner-aggregator` — reads `.snark`, emits `<Name>AggregatorVerifier.{bin,_calldata.bin}` (≤ 21 KB EIP-170).
- **On-chain:** `contracts/ethereum/verifiers/*.sol` deployed and driven from `AckiNackiBridge.verifyBlock`.

### End-to-end (per circuit)

```
KeyManager::ensure_<c>_keys           ← persists PK/VK to params_dir
     │
     ├─► Phase A  (n14 script, Blake2b)
     │      generate_<c>_proof                                  → proof.bin + instances.bin
     │      (consumed by Foundry tests + halo2_tvm_bundle round-trip)
     │
     └─► Phase A2 (n14 script, Poseidon)
            generate_<c>_proof_with_transcript(Poseidon)
                        │
                        ▼
            halo2_snark::export_poseidon_snark(km.<c>_vk(), km.<c>_config(), proof, instances)
                        │
                        ▼
                    <c>.snark            ← gosh↔axiom boundary crosses via this file
                        │
                        ▼   Phase C
            bridge-evm-aggregator::export-inner-aggregator
                        │
                        ▼
            contracts/ethereum/verifiers/<Name>AggregatorVerifier.{bin,_calldata.bin}
                        │
                        ▼
            AckiNackiBridge.verifyBlock → USDCBridge.mintAndSend
```

### What changes in `orchestrator/src/bin/export_bound_{block_proofs,poseidon_snarks}.rs`

Import surface only. `FallbackKeyManager`, `LayerHashesKeyManager`, `generate_fallback_proof`, `generate_layer_hashes_proof` (orchestrator) → collapse to `bridge_prover_lib::{keys::KeyManager, prover::*, layer_prover::*, transcript::TranscriptKind}`. Behaviour, on-disk artefacts, and on-chain verification are unchanged.

### Circuit 4 gap (not blocking Q4/Q5/Q6)

Prover side landed (`bridge-event-prover-lib`), but Phase A2 has no Circuit-4 branch yet. Once `compose_event_input` (bound-witness analogue) and a `BridgeWithdrawalAggregatorVerifier.bin` build target land, the four-circuit lane is fully symmetric — same `KeyManager`, same `_with_transcript(Poseidon)`, same `export_poseidon_snark`, same `export-inner-aggregator`.

---

## Appendix — Pipeline in one page

Three stages, all driven by `scripts/n14_r15_proving_run.sh`:

| Stage | Binary | Crate | Reads | Writes |
|---|---|---|---|---|
| A  | `export-bound-block-proofs` | `bridge-prover-orchestrator` | bound block | `params/<c>_{pk,vk}.bin`, `<c>_config.json`; `proofs/bound/<c>/*.{proof,instances}.bin` (Blake2b) |
| A2 | `export-bound-poseidon-snarks` | `bridge-prover-orchestrator` | Stage A artefacts | `proofs/bound/poseidon-snark/<c>.snark` (Poseidon-transcript, bincode `snark_verifier_sdk::Snark`) |
| C  | `export-inner-aggregator` | **`bridge-evm-aggregator`** | one `.snark` | `contracts/ethereum/verifiers/<Name>AggregatorVerifier.{sol,bin,_calldata.bin}` |

Stage C reads the `.snark` from disk, keygens its own `AggregationCircuit` **in memory** (never persisted), emits the Yul verifier `.bin` via `gen_evm_verifier_shplonk`, and enforces EIP-170. Native circuit keys (`params/*_{pk,vk}.bin`) come from Stage A and only their VK *shape* reaches Stage C — via the `.snark`'s embedded `Protocol`.

Foundry deploy scripts (`DeployGenesisCursorBridge.s.sol`, `DeployReuseVerifiersBridge.s.sol`) `vm.readFileBinary` each `.bin` and deploy it, then deploy the thin `<Name>AggregatorVerifier.sol` adapter (~1 KB) pointing at it. `AckiNackiBridge` calls the adapters via `IPrimaryVerifier` / `IFallbackVerifier` / `ILayerHashesMovementVerifier`.

**Circuit 4 gap:** the two orchestrator binaries `export-bound-block-proofs` + `export-bound-poseidon-snarks` have no Circuit-4 branch — they only keygen/prove/re-prove Circuits 1A, 1B, 2. They should, so Stage C can be invoked with `--name BridgeWithdrawalAggregatorVerifier`. Prover-lib side is ready (`bridge-event-prover-lib` + `KeyManager::ensure_event_keys`); only the orchestrator wiring is missing.

**Aux binaries in `bridge-evm-aggregator/src/bin/` — retirement candidates.** Only `export-inner-aggregator` is production (Stage C above). The other two are safe to delete:

- `export-spike-artifacts` — the leftover M2 sanity check that runs the `multiply.rs` `a·b=c` toy circuit end-to-end and writes fixtures under `contracts/ethereum/test/fixtures/r15_spike/`. `scripts/n14_r15_proving_run.sh` (Phase B) calls it only if `solc` is present and tolerates failure. Value ended when the real 1A/1B/2 `.bin` verifiers were trusted; keeping it live invites confusion about what the "real" aggregator entry point is.
- `export-halo2-poseidon-snark` — built by the driver script but never invoked. It would duplicate what `bridge-prover-orchestrator::bin/export_bound_poseidon_snarks` already does in-process (Stage A2). Dead code today, redundant tomorrow.

Recommendation: delete both `bin/` files (and the corresponding `[[bin]]` entries), drop the `cargo build --bin export-halo2-poseidon-snark` line from `n14_r15_proving_run.sh`, and either delete the `Phase B` spike block outright or move the multiply fixture generator into a `#[cfg(test)]` integration test. Net effect: `bridge-evm-aggregator` exposes one binary (`export-inner-aggregator`) matching one production role.

**`bridge-evm-aggregator/README.md` is stale** — it still describes the crate as the **M2 feasibility spike** proving `a * b == c` (Status table pinned to 2026-05-27/29, "What this crate *is not*" section says "It is **not** the real on-chain Circuit 4 verifier yet", "Layout" lists only `multiply.rs` + `aggregator.rs` + one `round_trip` test, "Pointers to next steps" talks about M3/M4/M5/M6/M7 as future work). Reality today: the crate hosts the production `export-inner-aggregator` binary that emits the on-chain 1A/1B/2 Yul verifiers under EIP-170, `AggregatorConfig::for_verifier_name` carries per-circuit presets (including `withdrawal`), and the multiply toy is auxiliary. Please rewrite the README to describe the current production role — the M2 spike history can move to a short "History" footnote or into `docs/r15_snark_verifier_roadmap.md`.

## Appendix — `bridge-relayer-daemon` residual gnark shape (`GROTH16_PROOF_SIZE = 256`)

`proof_validation.rs` runs on a live production path (`ProverProofsBlockSource::load_block` at `source.rs:408,414`, which the daemon binary instantiates at `bin/relayer.rs:1131,1170,1225,1601`), but its shape checks are unsound for R15:

- `validate_attestation_proof` / `validate_layer_hashes_proof` each contain an `if proof.len() == GROTH16_PROOF_SIZE { return Ok(()); }` short-circuit that accepts **any 256-byte blob** without further inspection. The module's own docstring admits this is "back-compat with the per-circuit Groth16 adapters retained for test coverage" — but the on-chain 1A/1B/2 SHPLONK verifiers don't accept 256-byte calldata. Effect: a malformed 256-byte proof passes validation locally and is rejected on-chain (revert-on-submit instead of parked pre-submit). Recommendation: delete the 256-byte branches; require `≥ SHPLONK_MIN_*_INSTANCES` unconditionally.

More problematic, `GROTH16_PROOF_SIZE = 256` (`withdrawal.rs:18`, re-exported at `lib.rs:101`) is hardcoded as the *required* withdrawal proof length in production paths:

- `withdrawal.rs:60` — `PartnerWithdrawalProof::validate` treats `raw.len() != 256` as an **error** with the message *"withdrawal proof is {} bytes; Ethereum bridge expects {}-byte Groth16 (run gnark-wrappers/circuit-4 prove on the Halo2 export first)"*.
- `bin/relayer.rs:1447` — daemon logs *"proof not 256-B Groth16 (gnark-wrap first); parking"* and refuses to submit anything else.
- `withdraw_prover.rs:81,277` — `MockWithdrawalProver` synthesises `[0xAA; 256]` canned proofs consumed by the same gate.

This is the direct on-chain blocker for AB-Q3 (`BridgeWithdrawalAggregatorVerifier`): the R15 Circuit-4 SHPLONK output is multi-kB calldata (12 accumulator limbs + 10 inner PIs + proof body ≈ 3–5 KB), and `PartnerWithdrawalProof::validate` will reject it before the tx is composed. `withdraw_prover.rs:23-30` already flags this as "reconciling the on-chain verifier shape is tracked separately (R15 M3–M7)".

Recommended cleanup, in one PR:

1. Remove `GROTH16_PROOF_SIZE` from `withdrawal.rs` and its `lib.rs` re-export.
2. Replace the size gate in `PartnerWithdrawalProof::validate` with `raw.len() >= SHPLONK_MIN_WITHDRAWAL_INSTANCES` (define alongside the other two `SHPLONK_MIN_*` constants in `proof_validation.rs`; value is `(12 + 10) × 32 = 704`).
3. Update `MockWithdrawalProver` to synth an appropriately-sized SHPLONK-shape stub (or make its size configurable so the unit tests can still drive short blobs through the mock bridge).
4. Delete the two 256-byte `if` branches in `proof_validation.rs`.
5. Update `bin/relayer.rs:1447` log line to drop the "gnark-wrap first" hint.

This is a `bridge-relayer-daemon`-only change; no on-chain contract or prover-lib change is needed. It should probably land in the same PR as AB-Q3 (Circuit-4 aggregator wiring) since either half without the other leaves the withdrawal lane broken.

## Appendix — There is no "Circuit 3"

`AckiNackiBridge.sol:614` and older notes reference a future "Circuit 3" for BK-set rotation. It does not exist and is not in scope. Workspace at `acki-nacki-to-eth-bridge-halo2-circuits/` ships only Circuits 1A/1B (`attestation-bls-checker-circuit`), 2 (`historical-layer-hashes-movement-checker-circuit`), 4 (`bridge-event-prove-circuit`).

Rotation on-chain today = `AckiNackiBridge.sol::applyBkSetUpdate` (line 758): verify the update block with the existing **Circuit 1A/1B** verifier, recompute a 3-hop SHA-256 Merkle path binding `blockId` to `newCommitmentL3`, assign `storedBkSetCommitment`. No new VK/circuit/lane.

Consequences: don't build a Circuit 3 during Q4/Q5/Q6; `bk_set_sentry.rs` recovers via `applyBkSetUpdate` + a 1A/1B proof; the stale line-614 Solidity comment is a doc-only follow-up.
