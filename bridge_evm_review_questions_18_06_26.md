# bridge-EVM — Review Questions

**To:** Pruvendo / Sergey Egorov  
**From:** Alina (AN-side circuits + prover)  
**Scope:** `bridge-EVM` review from the AN→ETH side (circuits 1A/1B/2/4).

**Document revision (Pruvendo, 2026-06-19):** Factual corrections marked **[Corrected]**; **Response** blocks added per question reflecting branch `pruvendo/shellnet-e2e-landing` @ `8cfd496`. Alina's original analysis (2026-06-18) is preserved below each question.

**Production policy (Pruvendo, 2026-06-19):** **No stubs in production.** Every deployed `AckiNackiBridge` (Sepolia, shellnet E2E, mainnet) must wire only real R15 SHPLONK aggregator verifiers (`Primary/Fallback/LayerHashes/BridgeWithdrawalAggregatorVerifier` + checked-in `.bin` ≤ 24 576 B). Identity-stub Groth16 wrappers, `MockBridgeWithdrawalVerifier`, and the multiply spike verifier are **CI / Foundry / local dev only** — they must not ship in deploy scripts or operator runbooks. Until four real `.bin` files exist, AN→ETH production paths stay **paused** (`pause()`), not stub-backed.

---

# Critical

## Q1 — R15: the only Groth16 verifiers on chain are identity-stub gnark wrappers

**State.** The Groth16 verifiers wired into **production deploy scripts** (`DeployRealBridge.s.sol`, `DeployShellnetE2EBridge.s.sol`) are the gnark-generated `*Groth16VerifierGenerated.sol` for Primary, Fallback, and LayerHashes (via `PrimaryVerifier.sol`, `FallbackVerifier.sol`, `LayerHashesMovementVerifier.sol`). Circuit 4 withdrawal on shellnet deploy uses **`MockBridgeWithdrawalVerifier`**, not a Groth16 C4 verifier. **[Corrected]** The `circuit-4/` gnark wrapper was **deleted** on `pruvendo/shellnet-e2e-landing`; only `circuit-{1a,1b,2}/` stubs remain. `Blake2bHalo2Verifier.sol` / `Halo2Verifier.sol` are test-only legacy. The `bridge-evm-aggregator` (snark-verifier-sdk → Yul) was a feasibility spike; **[Corrected]** it now has checked-in spike artefacts, Solidity SHPLONK adapters for all four circuits, and on-chain spike E2E — but is **not yet** the production deploy path.

**Problem.** Each of the three remaining gnark wrappers — `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-{1a,1b,2}/circuit.go` — has a `Define()` body of just:

```go
for i { api.AssertIsEqual(PI[i], PI[i]) }
api.AssertIsDifferent(DomainSize, 0)
```

Self-documented in the source. Circuit 1B (`circuit-1b/circuit.go:21-32`):

> *Define() method here uses identity constraints. Full in-circuit Halo2/SHPLONK verification is planned as a future improvement (it would require a gnark implementation of the Halo2 SHPLONK verifier — non-trivial; see Phase 8 R&D… For now the Groth16 proof commits to the public input values… The off-chain relayer is responsible for having held a verified Halo2 proof before generating the Groth16 wrap.*

**[Corrected]** Circuit 4 gnark stub (`circuit-4/circuit.go`) removed; withdrawal lane uses `MockBridgeWithdrawalVerifier` (deploy) or `BridgeWithdrawalAggregatorVerifier` (Shplonk forgery tests).

So on-chain `AckiNackiBridge.verifyBlock` / `withdrawByProof` on **current Sepolia deployments** accept Groth16 proofs that only tautologically commit to public inputs — the Halo2 layer is not cryptographically attested there.

**Branch audit (2026-06-18).** Verified that `Define()` bodies are byte-for-byte identical identity-stubs across open branches on `gosh-sh/bridge-EVM` (`main`, `pruvendo/shellnet-e2e-landing`, etc.). No branch had an in-flight real `Define()` for any circuit.

**[Corrected 2026-06-19]** `pruvendo/shellnet-e2e-landing` adds parallel R15 infrastructure (Shplonk adapters, spike on-chain E2E, Poseidon export paths, `applyBkSetUpdate` scaffold) but **does not replace** identity-stub Groth16 in deploy scripts. Stubs remain repo-wide for 1A/1B/2.

**Roadmap.** `docs/r15_snark_verifier_roadmap.md` (accepted 2026-05-26) chooses `snark-verifier-sdk::AggregationCircuit` + `gen_evm_verifier_shplonk` → Yul. Status: M1–M3 ✅; M5 de-risked on synthetic inner; **[Corrected]** M7 spike closed (`ShplonkSpikeOnChain.t.sol`, 13 172 B verifier); M4/M6/M7 for **real** inner proofs still open.

**Questions.**

1. Is the snark-verifier-sdk aggregator + Yul verifier still the chosen path, or has it shifted?
2. Concrete milestone / target dates for M4 + M5 against the real Circuit 4 proof, and for wiring `BridgeWithdrawalAggregatorVerifier.sol` into `AckiNackiBridge.withdrawByProof`?
3. Do circuits 1A/1B/2 also migrate to the aggregator path (Yul), or are they staying on gnark and getting a real `Define()` later? If gnark, what's the plan and timeline?
4. Transcript: do you need us to switch the Circuit 4 prover from Blake2b to Poseidon, or will you add a Blake2b loader to `snark-verifier`? Please confirm which side owns the change.
5. Until R15 closes, please confirm in writing that the intended trust model for Sepolia is relayer-rooted attestation, and that mainnet is explicitly gated on R15 (no `v2.0.0` tag before).

**Response (Pruvendo, 2026-06-19).**

1. **Yes — unchanged.** Aggregator + Yul SHPLONK is the **only** production path. Identity-stub Groth16 and mocks are **test-only** (Foundry fixtures, bound-proof regression in CI). They are **not** acceptable on any live bridge deployment.
2. **M4–M7 (real C4):** blocked on n14 keygen/proving (~hours CPU) + deploy-script wiring to Shplonk adapters only. No calendar dates; acceptance gate is `docs/shellnet_e2e_acceptance_runbook.md`. Spike M7 ✅ (dev fixture); production M7 🔴 until real inner proofs + `.bin` for all four circuits.
3. **1A/1B/2 → same aggregator path (choice A).** `Primary/Fallback/LayerHashesAggregatorVerifier.sol` + sizing matrix in `docs/r15_verifier_sizing_report.md`. Real gnark `Define()` and relayer-attested-by-design **(C)** are both **rejected** — not production options.
4. **Transcript split (frozen):** Blake2b SHPLONK on AN/TVM (`ZKHALO2VERIFYWITHVK`); **Poseidon** for ETH-side aggregator inner SNARK. Pruvendo owns export via orchestrator (`primary_prover.rs`, `layer_hashes_prover.rs`, `circuit4_prover.rs`, `TranscriptKind::Poseidon`). Partner does **not** need to drop Blake2b on AN.
5. **Confirmed — no stub trust model in production.** Sepolia/shellnet/mainnet AN→ETH paths require all four real R15 `.bin` files (EIP-170 ≤ 24 576 B) + forgery tests green before `unpause()`. **No `v2.0.0` / mainnet / public testnet AN→ETH traffic on stub verifiers.** If real `.bin` files are not ready, bridge stays paused — we do not fall back to relayer-rooted stub attestation on chain.

---

## Q2 — `crates/bridge-evm-aggregator` status: Circuit 4 plan is unimplemented, Circuits 1/2 not even planned

**State.** `crates/bridge-evm-aggregator/` is the chosen R15 vehicle (snark-verifier-sdk → Yul). It was a standalone-workspace **feasibility spike** — M2 closed 2026-05-27, M5 de-risked 2026-05-29 on synthetic inner (`src/multiply.rs`). **[Corrected]** Spike artefacts are now checked in under `contracts/ethereum/test/fixtures/r15_spike/` (13 172 B verifier + calldata); Solidity adapters exist for all four circuits; CI gate + `ShplonkSpikeOnChain.t.sol` landed. Real inner deserialization for partner circuits is still 🔴.

**Gap — Circuit 4.** Circuit 4 is finished on the partner side. Bridge-EVM M4 (swap `multiply.rs` for real C4 deserialization), M5/M6/M7 for production C4 are **still open** for real proofs — spike M7 only.

**Black hole — Circuits 1A/1B/2.** **[Corrected]** Roadmap and `docs/r15_verifier_sizing_report.md` now cover all four circuits (1A/1B/2 projected PASS; Circuit 2 **WATCH** at K=22). On chain they still sit behind identity-stub gnark until Shplonk cutover.

**Questions.**

1. Concrete dates for M4-EVM, M5 final K-sizing for Circuit 4's ~10 PIs, M6 (wire generated Yul to adapter), M7 (Foundry E2E).
2. For Circuits 1A/1B/2, pick one and commit in the roadmap: **(A)** four Yul verifiers; **(B)** real gnark `Define()`; **(C)** relayer-attested by design.
3. Transcript: confirm all four circuits switch from Blake2b to Poseidon for the ETH aggregator path.
4. Confirm `v2.0.0` (mainnet) is gated on #2, not just Circuit 4 R15 closure.

**Response (Pruvendo, 2026-06-19).**

1. **Ordered dependencies (no dates):**

   | Milestone | C4 | 1A/1B/2 |
   |-----------|----|---------|
   | M4 real inner in aggregator | 🔴 | 🔴 |
   | M5 K-sizing + `.bin` | 🔴 | 🔴 (C2 watch) |
   | M6 Solidity adapter | ✅ | ✅ |
   | M7 Foundry E2E | 🟡 spike only | 🔴 needs bound fixtures |
   | Deploy cutover | 🔴 | 🔴 |

   Spike: `make generate-spike-artifacts`. Per-circuit export after n14 proving.

2. **Choice (A) only** — four Yul SHPLONK verifiers in production. **(B)** real gnark `Define()` and **(C)** relayer-attested-by-design are **out of scope** — neither is a production path.

3. **Confirmed for ETH aggregator:** Poseidon transcript on export. Blake2b unchanged on AN.

4. **Confirmed:** mainnet / `v2.0.0` / any **unpaused** public deployment gated on **all four** real verifiers. Stubs and mocks never ship in deploy scripts.

---

## Q3 — Bridge-state shape diverges from `GLOBAL_HISTORY_DATA_SPEC.md §8`: latent withdrawal correctness bug + unbounded storage

**State.** `contracts/ethereum/src/AckiNackiBridge.sol` models the on-chain mirror of `GlobalHistoryData` as a **flat snapshot plus an unbounded set**:

```solidity
// AckiNackiBridge.sol:161-239 (paraphrased)
uint64  public storedLastSeenBlockSeqNo;
uint8   public storedNumLayers;                            // 1..=10
uint256[MAX_LAYER_HASHES] public storedLayerHashes;        // MAX_LAYER_HASHES = 10
uint256 public storedPrevMaxLevelLayerHash;
mapping(uint256 => bool) private _knownAnchors;
uint256 public anchorsRecorded;
```

`verifyBlock` commits state with a **verbatim, overwriting copy** of the calldata array followed by a *single* anchor insert:

```solidity
// AckiNackiBridge.sol:680-692
storedLastSeenBlockSeqNo = blockSeqNo;
storedNumLayers          = numLayers;
for (uint256 i = 0; i < MAX_LAYER_HASHES; i++) {
    storedLayerHashes[i] = layerHashes[i];               // (*) overwrites
}
storedPrevMaxLevelLayerHash = layerHashes[numLayers - 1];
_recordAnchor(layerHashes[numLayers - 1]);               // (**) only the top

// AckiNackiBridge.sol:705-711
function _recordAnchor(uint256 anchor) internal {
    _knownAnchors[anchor] = true;
    unchecked { anchorsRecorded = anchorsRecorded + 1; }
    emit AnchorRecorded(anchor, anchorsRecorded);
}
```

`withdrawByProof` performs a single flat membership check:

```solidity
// AckiNackiBridge.sol:821-823
if (!_knownAnchors[pub.finalRoot]) {
    revert UnknownAnchor(pub.finalRoot);
}
```

**Spec divergence.** `acki-nacki-to-eth-bridge-halo2-circuits/GLOBAL_HISTORY_DATA_SPEC.md §8.3` prescribes per-layer `HistoryWindow` circular buffers. Off-chain `bridge-prover-lib::BridgeState` matches; Solidity does not.

**Problems.**

1. **No layered structure in storage.** `_knownAnchors` is a flat bag; layer-of-origin is lost on insert.
2. **`storedLayerHashes` is a snapshot, not a window.** Each `verifyBlock` overwrites all 10 slots.
3. **`_recordAnchor` records the topmost layer only.** For key blocks with `numLayers ≥ 2`, the L1 root in `layerHashes[0]` is **never written into `_knownAnchors`**.
4. **Latent correctness bug — ~6 % silent `UnknownAnchor` reverts.** `bridge-event-witness` hard-codes L1 anchoring (`layer_idx = 0`). When `numLayers ≥ 2`, topmost ≠ L1 → L1 never recorded → withdraw fails despite valid Circuit 2 attestation.
5. **`_knownAnchors` is unbounded.** ~92k SSTORE/year at today's cadence vs spec's fixed 1 280 slots.
6. **Flat membership conflates layers.**
7. **Contract is the lone outlier** vs off-chain mirror + spec.

**Solidity sketch (replacement).** Mirrors §8.3–8.4 — `HistoryWindow`, `_appendLayer`, `_isKnownLayerAnchor`, optional `anchorLayer` PI on Circuit 4. See full sketch in partner review pack; unchanged from Alina's 2026-06-18 draft.

**Recommendation for Pruvendo.**

1. Bring contract into alignment with `GLOBAL_HISTORY_DATA_SPEC.md §8.3-8.4`.
2. Treat as **correctness fix** — ~6 % silent revert under W=128, P=8.
3. Coordinate Circuit-4 `anchorLayer` PI; interim `anchorLayer = 1` hardcode fixes L1-only witness today.
4. **Foundry regression:** verifyBlock with `numLayers ≥ 2` → withdraw on L1 → `UnknownAnchor` today, pass after fix.

**Questions.**

1. Reason for flat design vs spec, or track as fix?
2. Is the ~6 % revert rate known / reproducible in Foundry?
3. Ownership + date for spec-aligned storage + Circuit-4 `anchorLayer` PI?
4. Keep event-witness L1-only until L≥2 escalation funded?

**Response (Pruvendo, 2026-06-19).**

1. **No deliberate departure** — Phase 4.1 minimal scaffold before spec landed in Solidity. **Tracked as correctness fix**; not implemented on `pruvendo/shellnet-e2e-landing`.
2. **Analysis accepted.** Reproduction test not yet in Foundry; we will add `AckiNackiBridgeWithdrawByProofOrder2.t.sol`. Partner fixtures welcome.
3. **Ownership: Pruvendo (Solidity).** **Landed 2026-06-19:** per-layer `HistoryWindow` storage (W=128), `_appendLayer` on every `verifyBlock`, L1-only withdraw check (`WITHDRAW_ANCHOR_LAYER=1`). Regression: `AckiNackiBridgeWithdrawByProofOrder2.t.sol`. Future: PI slot 10 `anchorLayer` when witness grows to L≥2.
4. **Yes** — interim L1-only + hardcoded layer 1 is acceptable.

---

# Moderate

## Q4 — `bridge-prover-orchestrator/src/` largely duplicates `bridge-prover-lib`; please refactor

The orchestrator's four `src/bin/` binaries are the only product here, yet only `export_primary_proof.rs` actually imports from `bridge_prover_lib`. Roughly **~65% of `src/*.rs` is duplicated logic**.

**Delete after small lib additions (Circuit 1B + Circuit 2 paths):**

| File | Replace with | Lib addition (status) |
|---|---|---|
| `keys.rs` | `bridge_prover_lib::keys::KeyManager` | ✅ in lib |
| `layer_hashes_keys.rs` | same `KeyManager` | ✅ in lib |
| `prover.rs` (Blake2b) | `bridge_prover_lib::prover::generate_fallback_proof` | ✅ in lib |
| `prover.rs` (Poseidon) | `create_proof_with_transcript` + `PoseidonWrite` | ✅ landed `@main` |
| `verifier.rs` (Blake2b) | `verify_fallback_proof` | ✅ in lib |
| `verifier.rs` (Poseidon) | `verify_proof_with_transcript` + `PoseidonRead` | ✅ landed `@main` |
| `layer_hashes_prover.rs` | `generate_layer_proof_with_input` | ✅ landed `@main` |
| `layer_hashes_test_data.rs` | `build_synthetic_layer_hashes_input` (TREE_DEPTH=8 only) | ✅ landed `@main` |

**Keep — orchestrator-specific:** `poseidon_transcript.rs`, `halo2_tvm_bundle.rs`, `proof_export.rs`, `bound_test_data.rs`.

**Request.** Refactor bins to import from lib; delete duplicate modules. Depth-4 fixture must not survive cutover.

**Response (Pruvendo, 2026-06-19).** **Not yet done.** Duplicate modules still present; Poseidon paths added locally on top. **Still planned** after `cargo update` to partner `bridge-prover-lib@main` and rewiring export binaries.

---

## Q5 — `poseidon-proof/` role and removal candidacy

**State.** Standalone demo crate; only consumer is `Blake2bHalo2Verifier.t.sol` reading `poseidon-proof/data/*.bin`. Incompatible halo2 line (crates.io 0.5.x vs gosh fork 0.4.1).

**[Corrected]** Production AN→ETH path is moving to Shplonk aggregators, not gnark Groth16 wrappers; `poseidon-proof` remains reference material for Blake2b Solidity verifier tests only.

**Questions.**

1. Still needed?
2. Option A — delete crate; move `data/*.bin` to `contracts/ethereum/test/fixtures/`.
3. Option B — freeze crate.
4. Long-term role if neither?

**Response (Pruvendo, 2026-06-19).** **Prefer option A** long-term. **Not executed yet** — low priority vs R15. Does not block shellnet E2E.

---

# Summary matrix (2026-06-19)

| Topic | Allowed in CI / Foundry | Required for production deploy | Blocker |
|-------|-------------------------|--------------------------------|---------|
| verifyBlock crypto | Stub Groth16 1A/1B/2 (fixture tests) | Real Shplonk `.bin` × 3 | n14 keygen + deploy cutover |
| withdrawByProof | Mock / multiply spike | Real C4 Shplonk `.bin` | same |
| Layer anchor storage | Flat `_knownAnchors` (legacy) | **HistoryWindow** (W=128 × 10 layers) | `anchorLayer` PI for L≥2 |
| ETH→AN deposit | — | Real Halo2 VK on `USDCBridge` | Partner VK v2 redeploy |
| Stubs / mocks on chain | ✅ tests only | ❌ **never** | pause until real verifiers |

**Policy:** Production = real crypto only. Existing Sepolia deployments that still use stubs must be treated as **invalid for AN→ETH** until redeployed with four Shplonk `.bin` verifiers or left **paused**.

**Next Pruvendo actions:** (1) four real aggregator `.bin` on n14 via `export-inner-aggregator`; (2) bound Shplonk Foundry fixtures; (3) deploy + unpause after sign-off; (4) ~~Q3 HistoryWindow~~ done; (5) orchestrator dedup.

**References:** `docs/testnet_security_status.md`, `docs/r15_verifier_sizing_report.md`, `docs/shellnet_e2e_acceptance_runbook.md`, `docs/shellnet_usdcbridge_deposit_vk_redeploy.md`, `crates/bridge-prover-orchestrator/gnark-wrappers/SECURITY.md`.
