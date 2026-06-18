# bridge-EVM — Review Questions

**To:** Pruvendo / Sergey Egorov
**From:** Alina (AN-side circuits + prover)
**Scope:** `bridge-EVM` review from the AN→ETH side (circuits 1A/1B/2/4).

---

# Critical

## Q1 — R15: the only Groth16 verifiers on chain are identity-stub gnark wrappers

**State.** The only Groth16 verifiers actually wired into `contracts/ethereum/src/AckiNackiBridge.sol` are the four gnark-generated `*Groth16VerifierGenerated.sol` (Primary, Fallback, LayerHashes, BridgeWithdrawal — via the adapters `PrimaryVerifier.sol`, `FallbackVerifier.sol`, `LayerHashesMovementVerifier.sol`, `BridgeWithdrawalVerifier.sol`). The `Blake2bHalo2Verifier.sol` / `Halo2Verifier.sol` are test-only legacy. The `bridge-evm-aggregator` (snark-verifier-sdk → Yul) is a separate feasibility spike, not deployed.

**Problem.** Each of those four gnark wrappers — `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-{1a,1b,2,4}/circuit.go` — has a `Define()` body of just:

```go
for i { api.AssertIsEqual(PI[i], PI[i]) }
api.AssertIsDifferent(DomainSize, 0)
```

Self-documented in the source. Circuit 1B (`circuit-1b/circuit.go:21-32`):

> *Define() method here uses identity constraints. Full in-circuit Halo2/SHPLONK verification is planned as a future improvement (it would require a gnark implementation of the Halo2 SHPLONK verifier — non-trivial; see Phase 8 R&D… For now the Groth16 proof commits to the public input values… The off-chain relayer is responsible for having held a verified Halo2 proof before generating the Groth16 wrap.*

Circuit 4 (`circuit-4/circuit.go:29-34`):

> *IMPORTANT — R15 status. This wrapper enforces only identity-stub assertions over the public inputs. The Halo2 SHPLONK proof itself is **NOT verified** inside Groth16… the single biggest open mainnet blocker on the AN→ETH side.*

So on-chain `AckiNackiBridge.verifyBlock` / `withdrawByProof` accept a Groth16 proof that the wrapper PK holder can mint for any PI tuple — the Halo2 layer is not cryptographically attested.

**Branch audit (2026-06-18).** Verified that `Define()` bodies are byte-for-byte identical identity-stubs across **every** open branch on `gosh-sh/bridge-EVM` (`main`, `pruvendo/shellnet-e2e-landing`, `deposit-rlc-vkblob-v2`, `feat/usdt-deposits`, `test/arbitrum-replay-and-n14-runbook`, `docs/agents-git-remotes`, `feature/integrate-all-bridge-prover-code`). No branch has an in-flight real `Define()` for any of the four circuits — R15 is uniformly open repo-wide, not just on `main`.

**Roadmap.** `docs/r15_snark_verifier_roadmap.md` (accepted 2026-05-26) chooses `snark-verifier-sdk::AggregationCircuit` + `gen_evm_verifier_shplonk` → Yul `BridgeWithdrawalAggregatorVerifier.sol`, spiked in `crates/bridge-evm-aggregator/`. Status: M1–M3 ✅, M5 de-risked on a synthetic inner (2026-05-29); M4 (real Circuit 4 inner) blocked on Circuit 4 stability + Blake2b-vs-Poseidon transcript mismatch.

**Questions.**
1. Is the snark-verifier-sdk aggregator + Yul verifier still the chosen path, or has it shifted?
2. Concrete milestone / target dates for M4 + M5 against the real Circuit 4 proof, and for wiring `BridgeWithdrawalAggregatorVerifier.sol` into `AckiNackiBridge.withdrawByProof`?
3. Do circuits 1A/1B/2 also migrate to the aggregator path (Yul), or are they staying on gnark and getting a real `Define()` later? If gnark, what's the plan and timeline?
4. Transcript: do you need us to switch the Circuit 4 prover from Blake2b to Poseidon, or will you add a Blake2b loader to `snark-verifier`? Please confirm which side owns the change.
5. Until R15 closes, please confirm in writing that the intended trust model for Sepolia is relayer-rooted attestation, and that mainnet is explicitly gated on R15 (no `v2.0.0` tag before).

---

## Q2 — `crates/bridge-evm-aggregator` status: Circuit 4 plan is unimplemented, Circuits 1/2 not even planned

**State.** `crates/bridge-evm-aggregator/` is the chosen R15 vehicle (snark-verifier-sdk → Yul). It is a standalone-workspace **feasibility spike** — M2 closed 2026-05-27, M5 instance-exposure de-risked 2026-05-29 on a synthetic inner (`src/multiply.rs` proving `a*b == c`). The Yul output `target/spike/AggregatorVerifierSpike.{sol,bin}` is `.gitignore`d and **not wired into any production contract**. Nothing in `src/` references the real circuits.

**Gap — Circuit 4.** Circuit 4 is now finished on the partner side (`acki-nacki-to-eth-bridge-halo2-circuits`), so the M4 *partner-side* blocker is lifted. But the bridge-EVM side of M4 (swap `multiply.rs` for real Circuit 4 deserialization), and M5/M6/M7 (final K-sizing, move Yul into `contracts/ethereum/src/BridgeWithdrawalAggregatorVerifier.sol`, Foundry E2E) are **all still on paper only**. The roadmap doc has no dates.

**Black hole — Circuits 1A/1B/2.** Every milestone in `docs/r15_snark_verifier_roadmap.md` and `bridge-evm-aggregator/README.md` is written exclusively for Circuit 4 / `BridgeWithdrawalAggregatorVerifier.sol`. Circuits 1A (Primary attestation, per-block), 1B (Fallback, per-block), and 2 (Layer hashes, per attestation) **are not mentioned in any milestone**. Yet on chain they sit behind the same kind of identity-stub gnark wrapper (Q1) — fixing only Circuit 4 closes the withdrawal lane but leaves the entire block-attestation lane cryptographically void.

**Questions.**
1. Concrete dates for M4-EVM (swap `multiply.rs` → real Circuit 4 snark deserialization in `aggregator.rs`), M5 final K-sizing for Circuit 4's ~10 PIs, M6 (wire generated Yul to `BridgeWithdrawalVerifier.sol` adapter), M7 (Foundry E2E).
2. For Circuits 1A/1B/2, pick one and commit to it in the roadmap:
   - **(A)** Extend the aggregator spike to four Yul verifiers — needs K-sizing + per-verify gas estimate (1A/1B are called *every block*; BLS pairings in 1A likely don't fit K=21).
   - **(B)** Real gnark `Define()` bodies for 1A/1B/2 (your own `circuit-1b/circuit.go:21-32` calls this "non-trivial; Phase 8 R&D") — give cryptographer-owner + ETA.
   - **(C)** Leave 1A/1B/2 relayer-attested by design — give the written threat model + on-chain compensating control (multisig, challenge window).
3. Transcript: confirm all four circuits switch from Blake2b to Poseidon (not just Circuit 4), since `snark-verifier`'s EVM loader requires it.
4. Confirm `v2.0.0` (mainnet) is gated on the answer to #2, not just on Circuit 4 R15 closure.

---

# Moderate

## Q3 — `bridge-prover-orchestrator/src/` largely duplicates `bridge-prover-lib`; please refactor

The orchestrator's four `src/bin/` binaries are the only product here, yet only `export_primary_proof.rs` actually imports from `bridge_prover_lib` (`generate_primary_proof`, `KeyManager`). The other three (`export_fallback_proof.rs`, `export_layer_hashes_proof.rs`, `export_bound_block_proofs.rs`) reach for orchestrator-local re-implementations. Roughly **~65% of `src/*.rs` is duplicated logic** — three of the four binaries can move to `bridge_prover_lib` imports with small upstream additions.

**Delete after small lib additions (Circuit 1B + Circuit 2 paths):**

| File | Replace with | Lib addition (status) |
|---|---|---|
| `keys.rs` | `bridge_prover_lib::keys::KeyManager` (`fallback_vk/pk/config()` accessors) | ✅ already present in lib (`keys.rs:294-304`) |
| `layer_hashes_keys.rs` | same `KeyManager` (`layer_vk/pk/config()`, `layer_k/num_unusable_rows/lookup_bits()`) | ✅ already present in lib (`keys.rs:391-413`) |
| `prover.rs` (Blake2b) | `bridge_prover_lib::prover::generate_fallback_proof` | ✅ already present in lib |
| `prover.rs` (Poseidon transcript) | `bridge_prover_lib::prover::create_proof_with_transcript<E, T, C>` driven with orchestrator's `PoseidonWrite` | ✅ **landed** in `acki-nacki-to-eth-bridge-halo2-prover@main` — generic transcript helper exposing the underlying KZG/SHPLONK `create_proof` call |
| `verifier.rs` (Blake2b) | `bridge_prover_lib::verifier::verify_fallback_proof` | ✅ already present in lib |
| `verifier.rs` (Poseidon transcript) | `bridge_prover_lib::verifier::verify_proof_with_transcript<E, T>` driven with `PoseidonRead` | ✅ **landed** in `acki-nacki-to-eth-bridge-halo2-prover@main` — generic transcript helper for the verify path |
| `layer_hashes_prover.rs` | `bridge_prover_lib::layer_prover::generate_layer_proof_with_input` accepting `LayerHashesProofInput<'a>`; returns `LayerHashesProofOutput` | ✅ **landed** in `acki-nacki-to-eth-bridge-halo2-prover@main` — `LAYER_HASHES_NUM_PUBLIC_INPUTS = 14`, struct + wrapper added next to existing `generate_layer_proof` |
| `layer_hashes_test_data.rs` | `bridge_test_data_gen::layer_hashes::build_synthetic_layer_hashes_input` returning `SyntheticLayerHashesInput` | ✅ **landed** in `acki-nacki-to-eth-bridge-halo2-circuits@main` — **production-fixed `TREE_DEPTH = 8` (`HISTORY_PROOF_WINDOW_SIZE = 128`)**. The orchestrator-local copy hardcodes `TREE_DEPTH = 4` (lightweight `WINDOW_SIZE = 8`) — that fixture is not acceptable for mainnet. The upstreamed builder exposes only depth 8; no smaller value is available. Re-export from `bridge_prover_lib::layer_prover` is trivial if a single import root is preferred. |

All five additions are additive (no breaking changes to lib's existing public API) and live next to the existing primitives in `bridge-prover-lib/src/{prover,verifier,layer_prover}.rs` and `bridge-test-data-gen/src/layer_hashes.rs`. Built clean (`cargo build -p bridge-prover-lib`, `cargo build -p bridge-test-data-gen`). The orchestrator's `Cargo.toml` already pins both crates by git URL — just `cargo update` to pick them up.

**Keep — genuinely orchestrator-specific:**
- `poseidon_transcript.rs` — vendored Poseidon FS transcript, only used by the gnark export path.
- `halo2_tvm_bundle.rs` — `ZKHALO2VERIFYWITHVK` TVM opcode payload (AN-side TVM, not relevant to lib consumers).
- `proof_export.rs` — gnark wrapper IO glue.
- `bound_test_data.rs` — cross-circuit fixture for `export-bound-block-proofs`; may eventually graduate into `bridge-prover-lib` test helpers, but not blocking.

**Request.** The lib + test-data-gen additions are already shipped (see `acki-nacki-to-eth-bridge-halo2-prover@main` commit `e4d083d9` and the `bridge-test-data-gen` follow-up). Please refactor `src/bin/{export_fallback_proof,export_layer_hashes_proof,export_bound_block_proofs}.rs` to import from `bridge_prover_lib` / `bridge_test_data_gen` and delete the six files above. **Note on depth:** the upstreamed `build_synthetic_layer_hashes_input` is fixed to `TREE_DEPTH = 8` — the only mainnet-acceptable configuration; the existing depth-4 fixture must not survive the cutover. Net effect: orchestrator `src/` shrinks from ~2.9 kLoC to ~1.5 kLoC of code that is genuinely orchestrator-only.

---

## Q4 — `poseidon-proof/` role and removal candidacy

**State.** `poseidon-proof/` is a standalone, workspace-excluded crate (root `Cargo.toml:10`). It defines a trivial demo Halo2 circuit (`PoseidonPreimageCircuit`: prove `Poseidon(x) = h`, single Fr public input, K=12, ~1.5 KB proof, BN254/SHPLONK/Blake2b transcript) plus four binaries (`generate-proof`, `generate-verifier`, `generate-calldata`, `test-keccak-evm`) that emit fixture artifacts under `poseidon-proof/data/`. No Rust crate links it. Only consumer is the Foundry test suite: `contracts/ethereum/test/Blake2bHalo2Verifier.t.sol:55,201,212` reads `evm_calldata.bin`, `keccak_evm_calldata.bin`, `keccak_verifier_deployment.bin`, and `foundry.toml:14` whitelists `../../poseidon-proof/data/` for `vm.readFileBinary`.

**Verdict (per `docs/integration_analysis.md:42` — "reference / demo material" — and `docs/BLAKE2B_HALO2_VERIFIER.md`).** The production AN→ETH path now uses gnark Groth16 wrappers (`crates/bridge-prover-orchestrator/gnark-wrappers/circuit-{1a,1b,2,4}/`) around the partner's four-circuit stack — that supersedes the direct-Halo2-on-chain route. So `poseidon-proof` itself is historical/reference material that still backs the Blake2b Solidity verifier test suite.

**Halo2 incompatibility.** `poseidon-proof/Cargo.toml` pins crates.io `halo2-base = "=0.5.0"` and `snark-verifier-sdk = "=0.2.3"`. The production graph (deposit-prover, bridge-prover-orchestrator, partner's `bridge-prover-lib`) is anchored on the gosh fork `halo2-lib-zkevm-sha256-and-bls12-381` at `bump-halo2-lib-v0.4.1` (halo2-base 0.4.1 / zkevm-hashes 0.2.1). Mixing the two halo2 lines into one cargo unit is not possible — which is exactly why this crate has its own `[workspace]` boundary today. It also means the global `[patch]` work just landed (snark-verifier pin to `ccdb510`, axiom-eth bump to `51dec0d3`) does not — and cannot — reach this crate.

**Questions.**
1. Is `poseidon-proof/` still needed at all? The only live consumer is the Blake2b verifier Foundry test; the artifacts under `poseidon-proof/data/` are already checked in (`evm_calldata.bin`, `keccak_evm_calldata.bin`, `keccak_verifier_deployment.bin`, `params/kzg_bn254_12.srs`). If the binaries are not re-run, the crate is effectively unused build-time weight.
2. Removal option A — delete `poseidon-proof/` entirely; keep the pre-built `data/*.bin` artifacts moved under `contracts/ethereum/test/fixtures/`. The Blake2b verifier test continues to work; we lose the ability to regenerate the fixtures, but the AN→ETH gnark path doesn't need them.
3. Removal option B — keep the crate but freeze it (no toolchain bumps, no halo2 upgrades). Accept that it will never share the gosh halo2-axiom backend.
4. If neither — what is the intended long-term role of `poseidon-proof/`? Should it be migrated to the gosh fork (non-trivial: drops crates.io 0.5.x → fork 0.4.1, possible API drift), or kept on crates.io 0.5.x as a deliberate isolation?
