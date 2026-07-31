# bridge-EVM — Follow-up Questions (2026-07-27)

**To:** Pruvendo / Sergey Egorov
**From:** Alina

## NB-Q1 — `WITHDRAW_ANCHOR_LAYER = 1` still hardcoded 

**State.** `AckiNackiBridge.sol:74-75, 1040-1044`:

```solidity
uint8 internal constant WITHDRAW_ANCHOR_LAYER = 1;
// ...
// L1-only witness builder (`layer_idx = 0`). Future: read `anchorLayer`
// from Circuit 4 public input slot [10] when partner extends the layout.
if (!_isKnownLayerAnchor(WITHDRAW_ANCHOR_LAYER, pub.finalRoot)) {
    revert UnknownAnchor(pub.finalRoot);
}
```

Partner side is preparing witnesses anchored at `layer_idx ∈ 1..MAX_LAYER_HASHES` (multi-thread L(N)-tree work). The moment those ship, `withdrawByProof` will `revert UnknownAnchor` for every valid L≥2 proof because the anchor scan is pinned to L1.

**Questions.**

1. Is Option A (Circuit 4 grows PI slot [10] `anchorLayer`, bridge range-checks `1..MAX_LAYER_HASHES` and scans `_layerWindows[pub.anchorLayer]`) still the target? The comment at `:1040` implies yes but there is no tracking issue.
2. Any appetite for a transitional Option D (flat scan over all 10 windows via existing `_isKnownAnchor`) guarded by a feature flag, so partner is not forced to hold L≥2 witness shipping until the Shplonk re-keygen of Circuit 4 lands? 5-line change, revertible.
3. Confirm this gates on the Circuit 4 layout freeze, i.e. it is a single re-aggregator-export away rather than an independent PR.

---

## NB-Q2 — `storedLayerHashes` still overwritten unconditionally across all 10 slots 

**State.** `verifyBlock` still runs (`AckiNackiBridge.sol:727-730`):

```solidity
storedNumLayers = numLayers;
for (uint256 i = 0; i < MAX_LAYER_HASHES; i++) {
    storedLayerHashes[i] = layerHashes[i];
}
```

So a shallow successor (e.g. `numLayers=1` after a `numLayers=3` block) still zeroes slots `[1..9]` of `storedLayerHashes`. The per-layer anchor pick in `_expectedPrevAnchor` sources from `_layerWindows` and is unaffected — but the flat cache still leaks stale zeros through `getStoredLayerHashes()`.

**Questions.**

1. Is the flat overwrite intentional (kept only for the legacy "last block's array" view) or an oversight now that the per-layer windows are the truth?
2. If intentional: please document in the NatSpec that `getStoredLayerHashes()[i]` returns zero for `i >= storedNumLayers` of the *last* block (not the last-known per-layer value). Off-chain readers already exist and this rename would signal it clearly.
3. If oversight: switch the loop to `for (uint8 i = 0; i < numLayers; i++)` and leave higher slots at their previous per-layer value. This is ABI-visible for `getStoredLayerHashes()` — worth calling out in a minor-version note.

---

## NB-Q3 — `storedPrevMaxLevelLayerHash` retained as an "informational" SSTORE 

**State.** `AckiNackiBridge.sol:731-737`:

```solidity
// Record this block's max-level layer hash. This is now informational
// only (the per-layer windows below are the anchor source — see
// `_expectedPrevAnchor` / AB-Q4); kept for backward-compatible reads.
storedPrevMaxLevelLayerHash = layerHashes[numLayers - 1];
```

Read-side users are now:
- genesis bootstrap seed in `_expectedPrevAnchor` (`if (t == 0) return storedPrevMaxLevelLayerHash;`) — a **one-shot** use before the first `_appendLayer`;
- the public getter.

Every subsequent `verifyBlock` pays the SSTORE for a value that no on-chain logic will ever read again.

**Questions.**

1. Can this be split: keep a separate `_genesisPrevMaxLevelLayerHash` immutable seed (set in the constructor / initializer) so the SSTORE on the hot path goes away, and let the public getter return `_layerLatest(_highestActiveLayer())` instead?
2. If Pruvendo wants to keep the field for indexer ABI stability, mark it `deprecated` in NatSpec and stop writing to it — the last-write timestamp would still be correct as of the genesis seed.

---

## NB-Q4 — Two per-layer caches (flat `storedLayerHashes` + `_layerWindows`) coexist

`_layerWindows[L]` is now authoritative for anchoring; `storedLayerHashes` is a legacy view. This invites silent drift: any future change to `_appendLayer` semantics (e.g. skipping duplicates) will not be reflected in the flat cache, and vice versa.

**Question.** Is the plan to delete `storedLayerHashes` + `storedNumLayers` at v2.0 and expose a `getLatestPerLayer()` view backed by `_layerWindows[L].data[writeCursor-1]`? Same intent as NB-Q2 (3) and NB-Q3 (1); asking whether they should all land in one v2.0 sweep or piecemeal.

---

## NB-Q5 — Residual Groth16/gnark NatSpec + deploy-log labels (interfaces + `DeployRealBridge`)

All Groth16 adapter contracts and generated verifiers for 1A/2/4 are gone from `contracts/ethereum/src/` — nothing to delete anymore. But the *interface* NatSpec and one deploy script still describe the proof bytes as `gnark Groth16`, which is wrong now that every wired backend is Shplonk (and 1B specifically was retired on `e2a962b`).

**Sites (verified 2026-07-27):**

| File | Lines | Issue |
|---|---|---|
| `src/IPrimaryVerifier.sol` | 21 | "256-byte Groth16 proof (8 × uint256, gnark MarshalSolidity layout)" |
| `src/IFallbackVerifier.sol` | 18 | same |
| `src/ILayerHashesMovementVerifier.sol` | 29, 35 | "Groth16 proof" / "reverts in the underlying gnark" |
| `src/IBridgeWithdrawalVerifier.sol` | 23, 25, 28, 60, 62 | describes struct as mirroring gnark PI layout and references `IBridgeWithdrawalGroth16Verifier` — a file that no longer exists |
| `script/DeployRealBridge.s.sol` | 17 | `@dev … gnark Groth16 for 1B fallback` — 1B is now Shplonk |
| `script/DeployRealBridge.s.sol` | 193 | `console.log("FallbackVerifier (Groth16):", …)` — operator-visible label, wrong |

Test-side mentions (`ShplonkAggregatorForgery.t.sol`, `MockPrimaryVerifier.sol`, `MockBridgeWithdrawalVerifier.sol`, `AckiNackiBridgeWithdrawByProof.t.sol`, `FuzzVerifiers.t.sol`, `verifiers/README.md`) are legitimate historical context / forgery fixtures — flagging so they're deliberately excluded from the sweep.

**Questions.**

1. OK to sweep the six sites above to "SHPLONK proof bytes (Halo2 KZG aggregator calldata)" and drop the stale `IBridgeWithdrawalGroth16Verifier` cross-reference?
2. Reaffirm policy: no Groth16 adapter will be wired into a live `AckiNackiBridge` constructor on any network including Sepolia, correct?

---

## NB-Q6 — `poseidon-proof/` crate: last consumer is gone, still in-repo

`poseidon-proof/` was kept as reference material for `Blake2bHalo2Verifier.t.sol`, but that Foundry test — and the entire `Blake2b*` verifier surface (`Blake2bHalo2Verifier.sol`, `Halo2Verifier.sol`) — is gone from `contracts/ethereum/`. Only `ShplonkHalo2Verifier.sol` / `IShplonkHalo2Verifier.sol` remain.

Standing references to `poseidon-proof` are now only: `Cargo.toml` workspace member, `AGENTS.md`, `.gitignore`, `contracts/ethereum/foundry.toml`, `docs/BLAKE2B_HALO2_VERIFIER.md`, `bridge_updates_analysis_2026-06-25.md`, top-level `README.md`. No live consumer.

**Questions.**

1. OK to delete `poseidon-proof/` + `docs/BLAKE2B_HALO2_VERIFIER.md`, drop from workspace `Cargo.toml`, and prune the `foundry.toml` / `AGENTS.md` / `README.md` mentions? Nothing binds the crate any more.
2. If a Blake2b regression fixture is still wanted, move `poseidon-proof/data/*.bin` under `contracts/ethereum/test/fixtures/` and delete everything else in the crate.

---

## NB-Q7 — Relayer 256-byte back-compat gate still lets malformed Groth16 blobs pass local validation

`bridge-relayer-daemon/src/proof_validation.rs:23,41` still short-circuits with:

```rust
if proof.len() == GROTH16_PROOF_SIZE { return Ok(()); }   // GROTH16_PROOF_SIZE = 256
```

for both `validate_attestation_proof` and `validate_layer_hashes_proof`. Any 256-byte blob passes local validation and only reverts on-chain — losing us the pre-submit sanity check for the (now overwhelmingly common) SHPLONK path. The SHPLONK aggregator wrap has otherwise landed (`aggregator.rs`, `SHPLONK_MIN_WITHDRAWAL_INSTANCES`, C4 wrap path). Same shape gate lingers in `withdrawal.rs:78` (`if raw.len() != GROTH16_PROOF_SIZE && raw.len() < SHPLONK_MIN_WITHDRAWAL_INSTANCES`).

**Questions.**

1. All three back-compat gates safe to remove — `proof_validation.rs:23`, `proof_validation.rs:41`, and the `!= 256` disjunct in `withdrawal.rs:78`? Any deployed Sepolia bridge still expecting a real 256-byte Groth16 payload?
2. If any deployed instance still needs the back-compat path, gate it behind a `--accept-legacy-groth16` CLI flag defaulted off so mainnet/shellnet get strict SHPLONK-only validation.
3. Companion stale strings: `withdraw_prover.rs:28,81` module docstring + `MockWithdrawalProver` `[0xAA; 256]` fill; `bin/relayer.rs:1447` "proof not 256-B Groth16" log. Regenerate the mock at SHPLONK length in the same PR?

---

## NB-Q8 — `WIRE_WITHDRAW_BY_PROOF` defaults to `false` in deploy scripts even though the C4 `.bin` now exists

`contracts/ethereum/verifiers/BridgeWithdrawalAggregatorVerifier.bin` is committed, `ShplonkDeployLib.deployWithdrawalAdapter()` is ready, `BridgeWithdrawalAggregatorVerifier.sol` compiles — but both deploy scripts still make Circuit 4 opt-in:

- `DeployShellnetE2EBridge.s.sol:52` — `bool wireWithdraw = vm.envOr("WIRE_WITHDRAW_BY_PROOF", false);`
- `DeployRealBridge.s.sol:123` — same; `_buildWithdrawConfig(wire=false, …)` returns `bridgeWithdrawalVerifier: address(0)`.

A default-off flag was a stopgap for the missing artefact. That artefact now exists, so a production deploy that forgets to set the env var silently ships with `withdrawByProof` disabled (reverting with `WithdrawByProofDisabled`).

**Questions.**

1. Flip the default to `true` (or remove the flag entirely) in both scripts, making the C4 aggregator mandatory? A deploy that intentionally omits withdraw wiring seems like a footgun, not a valid production shape.
2. If the flag stays for CI convenience, at minimum have the `!wire` branch of `_buildWithdrawConfig` `revert` on non-test networks — silent `address(0)` wiring is the failure mode we want to prevent.

---

## NB-Q9 — `bridge-prover-orchestrator` crate: only runtime consumer is one subprocess call, otherwise dead weight

**State.** Cross-repo grep of `use bridge_prover_orchestrator::` returns **zero hits outside the crate itself** — its 1,401 LoC of library code (`halo2_snark.rs`, `halo2_tvm_bundle.rs`, `bound_test_data.rs`, `proof_export.rs`, `lib.rs`) is consumed only by its own 5 CLI binaries and tests. It is not a Cargo dep of any other bridge crate.

The one runtime consumer is `bridge-relayer-daemon`, which shells out to **one** binary via `SubprocessCircuit4SnarkProver` (`aggregator.rs:55, 111-151`, wired in `bin/relayer.rs:1226-1235`):

- `export-c4-poseidon-snark` — Circuit 4 (event) Poseidon inner snark. Called at runtime for every `ProveWithdrawShplonk` invocation.

The other four bins (`export-1a1b2-poseidon-snark`, `export-bound-block-proofs`, `export-bound-poseidon-snarks`, `export-halo2-poseidon-snark`) are invoked only by shell scripts / manual runbooks for offline `.bin` verifier regeneration — never by any daemon.

Meanwhile Circuits 1A/1B/2 are already in-processed via `bridge-prover-lib::live_driver::LiveProverDriver` (`prover::generate_primary_proof`, `prover::generate_fallback_proof`, `layer_prover::generate_layer_proof`). Circuit 4 didn't get the same treatment in the 2026-07-27 refactor and kept the subprocess wrapper as scaffolding.

**Note.** `bridge-evm-aggregator` is a fully standalone cargo workspace and has **no** dependency on `bridge-prover-orchestrator` — the two communicate only via `.snark` files on disk. Deleting the orchestrator does not touch the aggregator.

**Caveat — not necessarily a full delete.** The four offline bins (`export-1a1b2-poseidon-snark`, `export-bound-block-proofs`, `export-bound-poseidon-snarks`, `export-halo2-poseidon-snark`) genuinely are useful as dev/ops utilities — verifier `.bin` regeneration, fixture rebuilds, debug snarks on demand. What is wrong here is the *shape*: they are miscategorized under a crate called "orchestrator" (which no longer orchestrates anything at runtime) mixed with a few genuinely dead pieces. So the ask isn't "delete everything," it's **rework + rebrand**: keep what still earns its keep, move it to a crate whose name matches what it does (e.g. `bridge-dev-tools` / `bridge-snark-utils`), and drop the pieces that turn out to have no consumers. The runtime `SubprocessCircuit4SnarkProver` path is the one clearly-fixable architectural miss (Circuits 1A/1B/2 are already in-processed; Circuit 4 should follow).

**Questions.**

1. OK to lift `export_c4_poseidon_snark.rs` (~200 LoC) into `bridge-event-prover-lib` as `pub fn export_circuit4_poseidon_snark(...)` and have `bridge-relayer-daemon` call it in-process — same pattern as Circuits 1A/1B/2 today? (This part I'd argue is clear-cut.)
2. For the rest — rather than deciding delete-vs-keep piecewise now, does it make sense to open a small design ticket "rework `bridge-prover-orchestrator`" whose outcome is one of:
   - **Rebrand + slim:** rename to something like `bridge-snark-utils` / `bridge-dev-tools`, keep the offline bins that ops/dev actually still runs, drop the dead code. Library modules land where they naturally belong:
     - `halo2_snark.rs` (97 LoC, gosh-VK → snark-verifier `Snark` wrapper) → `bridge-event-prover-lib` (same gosh-fork build unit), *if* still used by any surviving bin.
     - `halo2_tvm_bundle.rs` (698 LoC, `VkBlob` for AN-side `ZKHALO2VERIFYWITHVK` opcode) → `bridge-prover-lib` (its natural AN-side home).
     - `proof_export.rs` (101 LoC ser/de helpers) → wherever the callers land.
     - `bound_test_data.rs` (464 LoC fixture generator) → `bridge-event-prover-lib/tests/` (test-support only).
   - **Full delete:** only if every bin turns out to have no ops/dev consumer either.
     The point is: the name "orchestrator" is now misleading (it orchestrates nothing at runtime post-refactor), and the crate has become a bag of loosely-related utilities — some load-bearing for ops, some genuinely dead. Worth a deliberate pass, not a piecewise decision.
3. Any consumer I'm not seeing on the *library* side (`use bridge_prover_orchestrator::...`)? Only cross-repo hits outside the crate are doc examples (`docs/zkhalo2verifywithvk_reference.md:219`, `fixtures/circuit_1b_fallback/README.md:98`) — trivially retargetable. And on the *binary* side — which of the four offline bins are you (or ops) still running by hand, so we know what has to survive the rework?

---

## Cross-cutting

Is there a single tracking issue that batches NB-Q2/3/4 so they land together with one ABI-break note? Would prefer one migration event over three.
