# bridge-EVM — Updates Analysis (2026-06-21 → 2026-06-23)

**Reviewer:** Alina  
**Date written:** 2026-06-25  
**Scope:** Sergey Egorov's 15 commits on `pruvendo/shellnet-e2e-landing` between 2026-06-21 and 2026-06-23, measured against the open questions in `bridge_evm_review_questions_18_06_26.md`.

---

## Commit timeline

| Hash | Date | Subject |
|------|------|---------|
| `bc022cc` | 06-21 15:46 | feat(r15): Shplonk-only deploy, **HistoryWindow anchors**, and n14 proving script |
| `2c13057` | 06-21 16:06 | fix(n14): sync Cargo.lock and build with --locked on remote |
| `d9924fb` | 06-21 16:08 | fix(n14): use cargo +nightly for orchestrator and aggregator builds |
| `e065b60` | 06-21 16:13 | fix(orchestrator): unify halo2-axiom and align Circuit 4 prover API |
| `b84943d` | 06-21 16:24 | fix(orchestrator): load primary PK before bound/primary proof export |
| `6100c0e` | 06-22 03:29 | feat(r15): Poseidon snark export and n14 continue-bc pipeline |
| `a7032dd` | 06-22 03:30 | fix(n14): preserve remote proofs/ and rsync solc binary not symlink |
| `cbb76a4` | 06-22 04:01 | fix(orchestrator): persist bound witness for Poseidon A2 re-prove |
| `1b041bf` | 06-22 13:58 | feat(r15): gosh-VK Snark export in orchestrator + 2/3 production verifiers |
| `ae74159` | 06-22 14:04 | feat(deploy): hybrid verifyBlock — Groth16 for 1B fallback, Shplonk for 1A/2 |
| `a2c3158` | 06-22 14:49 | feat(relayer): hybrid verifyBlock proof shapes and n14 artefact pull |
| `6e75555` | 06-22 15:22 | docs(ops): production plan, preflight gates, and hybrid Foundry smoke |
| `e2a962b` | 06-23 01:12 | **feat(r15): retire fallback Groth16 hybrid — Circuit 1B on production SHPLONK** |
| `3dc2a9b` | 06-23 02:44 | fix(prover): repair bound Circuit 2 witness drift vs partner branch=main |
| `cfa3415` | 06-23 04:17 | **fix(prover): full production verifyBlock green — canonical block_id binding** |
| `4a22334` | 06-23 04:24 | docs: align all-SHPLONK verifyBlock surface; retire stale 1B Groth16 refs |
| `e57436c` | 06-23 04:32 | fix(deploy): gate shellnet C4 withdraw wiring behind WIRE_WITHDRAW_BY_PROOF |
| `56fd796` | 06-23 14:59 | feat(deposit-relayer): align finalizeDeposit submit with deployed (proof, publicInputs) ABI |

---

## Status against `bridge_evm_review_questions_18_06_26.md`

| Q | Topic | Promised 06-19 | Status 06-23 |
|---|---|---|---|
| **Q1/Q2** | Real SHPLONK for 1A/1B/2 | "blocked on n14 keygen, no dates" | ✅ **DONE** — three `.bin` files in `contracts/ethereum/verifiers/`, EIP-170 compliant |
| **Q1/Q2** | Real SHPLONK for Circuit 4 | M4–M7 open | 🔴 **NOT DONE** — `BridgeWithdrawalAggregatorVerifier.bin` missing; deploy gated behind `WIRE_WITHDRAW_BY_PROOF` |
| **Q3** | `HistoryWindow` per-layer storage | "Landed 2026-06-19" | ✅ **DONE** (`bc022cc`) |
| **Q4** | Orchestrator/lib dedup | "Still planned" | 🔴 **NOT DONE** — orchestrator actually grew 4 new modules/binaries |
| **Q5** | Delete `poseidon-proof/` | "Prefer A, not urgent" | 🔴 **NOT DONE** — crate untouched |

---

## How Q1/Q2 was fixed — the production verifier track

**Path landed:** snark-verifier-sdk `AggregationCircuit` → `gen_evm_verifier_shplonk` → Yul `.bin`, wrapped by a Solidity adapter implementing the three `verifyBlock` interfaces. `contracts/ethereum/script/ShplonkDeployLib.sol` does the `CREATE`-from-bytecode dance.

Key commits:

1. **`bc022cc`** wires `DeployRealBridge.s.sol` + `DeployShellnetE2EBridge.s.sol` to `ShplonkDeployLib`; adds `crates/bridge-evm-aggregator/src/bin/export_inner_aggregator.rs`.
2. **`6100c0e` / `cbb76a4`** add Poseidon snark export pipeline: new `export_bound_poseidon_snarks` binary + `halo2_snark` modules in both `bridge-evm-aggregator` and `bridge-prover-orchestrator`. Bound witness persistence so the Poseidon A2 re-prove uses the same instance as Blake2b.
3. **`1b041bf`** commits first real production `.bin` files: **`PrimaryAggregatorVerifier.bin`** (21,493 B) and **`LayerHashesAggregatorVerifier.bin`** (19,100 B), both ≤ EIP-170 24,576 B.
4. **`ae74159` / `a2c3158` / `6e75555`** add a temporary hybrid lane (Groth16 for 1B, SHPLONK for 1A/2) + `proof_validation.rs` in relayer + `AckiNackiBridgeHybridVerifyBlock.t.sol`.
5. **`e2a962b`** retires the hybrid — Circuit 1B re-keygenned at **inner K=21** (vs 1A's K=20) so its aggregator collapses to **22 advice cols** and Yul drops from 28.4 KB → 21,493 B.
   - Deleted: `FallbackGroth16VerifierGenerated.sol`, `FallbackVerifier.sol`, `IFallbackGroth16Verifier.sol`, `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-1b/`, `FallbackVerifier.t.sol`, `install_fallback_groth16_verifier.sh`.
   - Added: `FallbackAggregatorVerifier.bin` (21,493 B) + `.sol` (92,592 B).
6. **`3dc2a9b` + `cfa3415`** repair two bound-witness drifts vs partner `branch=main` and certify `AckiNackiBridgeProductionVerifyBlock.t.sol` green:
   - **Circuit 2 chain-step off-by-one** — partner generator builds `num_prev_chain_steps + 1` active Merkle trees but records the previous count; orchestrator now hands the circuit `+1`.
   - **`block_id` endianness** — 1A/1B LE-pack raw bytes; Circuit 2 reverses BE→LE before packing. Generator stored BE root → 1A's `block_id` was a byte-reversal of Circuit 2's. Now both attestations re-signed over the LE block_id (canonical `uint256(sha256_root)`). The 06-19 "Circuit 2 KZG pairing limitation" was a misdiagnosis of this.

**Net effect:** the full production `verifyBlock` E2E is green in Foundry against **real SHPLONK aggregator calldata**. `test_productionVerifyBlock_boundCalldata_advancesState` exercises bound witness → real proof → state advance; full Foundry suite 163 passed.

---

## How Q3 was fixed (`bc022cc`)

`contracts/ethereum/src/AckiNackiBridge.sol`:

```solidity
struct HistoryWindow {
    uint256[HISTORY_PROOF_WINDOW] data;     // W=128
    uint64[HISTORY_PROOF_WINDOW] heights;
    uint16 writeCursor;
    uint16 dataLen;
    uint64 lastHeight;
}
mapping(uint8 => HistoryWindow) private _layerWindows;
uint8 constant WITHDRAW_ANCHOR_LAYER = 1;
```

- `verifyBlock` calls `_appendLayerHashes(numLayers, layerHashes, blockSeqNo)` → `_appendLayer` for each of the L layers.
- `_appendLayer` (line 810) enforces `blockHeight ≥ w.lastHeight` (monotonic), circular write at `w.writeCursor`, caps `dataLen` at `HISTORY_PROOF_WINDOW`. Emits both legacy `AnchorRecorded` and new `LayerAnchorAppended`.
- `withdrawByProof` (line 968) uses `_isKnownLayerAnchor(WITHDRAW_ANCHOR_LAYER, pub.finalRoot)` — hardcoded layer 1 per the agreed interim.
- Regression test `AckiNackiBridgeWithdrawByProofOrder2.t.sol` added.

Legacy `storedLayerHashes` flat snapshot kept alongside windows (still populated each `verifyBlock`) for backward-compat with `getLayerHashes()`. Flat `_knownAnchors` mapping replaced by an `_isKnownAnchor` view-helper that scans all 10 windows.

---

## What was NOT fixed

1. **Circuit 4 production verifier.** `production_plan.md` Phase 2 marked "blocked: partner M4". No `BridgeWithdrawalAggregatorVerifier.bin`. `DeployShellnetE2EBridge.s.sol` only wires C4 when `WIRE_WITHDRAW_BY_PROOF=true` (gated by `e57436c`); default deploy has no C4 path. The `circuit-4/` gnark dir is partially gutted (only `go.mod`/`go.sum`/`README.md` remain).
2. **gnark wrappers for 1A and 2.** `circuit-1a/` and `circuit-2/` still ship with identity-stub `Define()` (`api.AssertIsEqual(PI[i], PI[i])`). Unused in production deploy but not removed in lockstep with `circuit-1b/`.
3. **Q4 — orchestrator dedup.** Not started. Opposite happened — orchestrator grew 4 new modules/binaries for Poseidon export.
4. **Q5 — `poseidon-proof/`.** Untouched.
5. **`anchorLayer` PI slot 10 on Circuit 4.** Explicitly deferred (`AckiNackiBridge.sol:951` comment).

---

## "Partner-blocked" Circuit 4 — what really blocks it

The production plan says "Partner ships stable Circuit 4 + Poseidon export | Partner | M4". The reality is more nuanced.

### Already on Pruvendo side

1. `crates/bridge-prover-orchestrator/src/circuit4_prover.rs` exists — uses partner's `bridge_event_prove_circuit::BridgeEventProveCircuit`, supports both Blake2b and **Poseidon** transcripts via `generate_circuit4_proof_with_transcript`, declares 10 PIs.
2. `scripts/n14_r15_proving_run.sh:167` has `export_one "${SNARK_DIR}/circuit4.snark" BridgeWithdrawalAggregatorVerifier` — drop a `circuit4.snark` next to the other three and the n14 pipeline auto-produces `BridgeWithdrawalAggregatorVerifier.bin`.
3. `contracts/ethereum/src/BridgeWithdrawalAggregatorVerifier.sol` and `IBridgeWithdrawalVerifier.sol` exist. `ShplonkDeployLib.deployWithdrawalAdapter` is implemented. `DeployRealBridge.s.sol` wires it behind `WIRE_WITHDRAW_BY_PROOF=true`.
4. `AckiNackiBridge.withdrawByProof` uses `_isKnownLayerAnchor(WITHDRAW_ANCHOR_LAYER=1, finalRoot)` against the new HistoryWindow.

### Actually missing

1. **No C4 entry in `export_bound_poseidon_snarks`.** That binary's `CIRCUITS` array (`bin/export_bound_poseidon_snarks.rs:42-58`) lists only `primary`, `fallback`, `layer_hashes`. There is no analogous `circuit4` row → no `circuit4.snark` is ever produced.
2. **No bound withdrawal scenario.** Orchestrator's bound test data (`bound_test_data.rs`) constructs a block-verify scenario (1A+1B+2 on a single key block). For C4 you need a bound `WithdrawalInitiated` event witness — a deterministic withdrawal whose Merkle proof terminates at the same L1 anchor that 1A/2 attest to. That fixture doesn't exist on either side.
3. **Circuit 4 layout stability.** Per memory `bridge_circuit4_single_final_root.md`, Circuit 4 was migrated to single-final-root on the `circuit4-single-final-root` branch of `acki-nacki-to-eth-bridge-halo2-circuits`, not `main`. Pruvendo doesn't want to keygen a SHPLONK aggregator they'll have to throw away.
4. **No K-sizing measurement for C4.** `docs/r15_verifier_sizing_report.md` row 4: `inner K ~20 (partner) | inner PIs 10 | K_outer 21 | bytes .bin —` → **TBD**. Could need K=20→K=21 retune like 1B just did.

### Real ownership

| Blocker | Owner |
|---|---|
| Stable Circuit 4 on `main` + frozen 10-PI single-final-root layout | **Partner (Alina)** |
| Bound withdrawal fixture | Either side (Pruvendo can synthesize once layout is frozen) |
| Add `circuit4` to `export_bound_poseidon_snarks::CIRCUITS` | Pruvendo (mechanical) |
| n14 keygen + Poseidon prove → `circuit4.snark` | Pruvendo (mechanical) |
| EIP-170 sizing + (possibly) inner-K retune | Pruvendo |
| Wire `WIRE_WITHDRAW_BY_PROOF=true` in deploy default | Pruvendo (trivial after `.bin`) |

The single hard dependency on partner = stabilize Circuit 4 on `main` with frozen single-final-root 10-PI layout. Once done, Pruvendo could ship `BridgeWithdrawalAggregatorVerifier.bin` in another 1–2 commits of the shape of `e2a962b`.

---

## Other notable changes (not in review)

1. **`deposit-relayer-daemon` ETH→AN ABI alignment (`56fd796`).** Shellnet `USDCBridge` (acki-nacki `poseidon_dex`, deployed 2026-06-22) now exposes `finalizeDeposit(bytes proof, bytes publicInputs)`. Relayer refactored to forward only those two blobs (`build_finalize_deposit_params` → canonical 11×32 LE); new `finalize-one` CLI; dead config removed (`token_id`, `gas_limit`, `--an-token-id`, `eth_address_to_src_sender_hex`). `scripts/ursus/USDCBridge.abi.json` refreshed.
2. **n14 build pipeline hardening (`2c13057`, `d9924fb`, `a7032dd`, `e065b60`, `b84943d`).** `Cargo.lock` sync, `--locked` builds, `cargo +nightly` for orchestrator/aggregator, rsync solc as real binary, preserve remote `proofs/`. `bridge-prover-orchestrator/Cargo.toml` adds halo2-axiom patch to unify with `tvm_vm`. PK lifecycle fix: `load_primary_pk` before bound/primary export, unload after Circuit 1A.
3. **New ops/docs surface.**
   - `docs/production_plan.md` — phase-gated operator runbook (Phase 0 preflight → Phase 6 mainnet).
   - `scripts/production_preflight.sh` (`make production-preflight`).
   - `scripts/production_env.example`.
   - `scripts/reexport_layer_verifier.sh`.
4. **Relayer `proof_validation.rs`** — per-circuit ABI shape gates, retained after hybrid retirement.
5. **Foundry production assets** — `AckiNackiBridgeProductionVerifyBlock.t.sol`, `ShplonkDeployLib.t.sol`, calldata fixtures.
6. **Doc clean-up** — `r15_verifier_sizing_report.md` extended with K=21 fallback matrix; `audit_trail_v2.md`, `bridge_verification.md`, `integration_analysis.md`, `shellnet_e2e_acceptance_runbook.md`, `testnet_security_status.md` swept for stale 1B Groth16 references.

---

## Bottom line

**Moved:** `verifyBlock` is now real-crypto end-to-end on the AN→ETH side. Three EIP-170-compliant SHPLONK Yul verifiers exist as checked-in `.bin` files; deploy scripts wire them; bound witness drifts vs partner `branch=main` are repaired; the previously diagnosed "Circuit 2 pairing limitation" was actually a `block_id` endianness bug and is closed. Production preflight + plan docs give operators a one-command gate.

**Didn't move:** Circuit 4 / `withdrawByProof` still partner-blocked (gated by partner-side Circuit 4 layout freeze). Orchestrator dedup (Q4) and `poseidon-proof` removal (Q5) remain backlog.
