# bridge-EVM — Review Questions

**To:** Pruvendo / Sergey Egorov
**From:** Alina (AN-side circuits + prover)
**Scope:** `bridge-EVM` review from the AN→ETH side (circuits 1A/1B/2/4).

---

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

**Roadmap.** `docs/r15_snark_verifier_roadmap.md` (accepted 2026-05-26) chooses `snark-verifier-sdk::AggregationCircuit` + `gen_evm_verifier_shplonk` → Yul `BridgeWithdrawalAggregatorVerifier.sol`, spiked in `crates/bridge-evm-aggregator/`. Status: M1–M3 ✅, M5 de-risked on a synthetic inner (2026-05-29); M4 (real Circuit 4 inner) blocked on Circuit 4 stability + Blake2b-vs-Poseidon transcript mismatch.

**Questions.**
1. Is the snark-verifier-sdk aggregator + Yul verifier still the chosen path, or has it shifted?
2. Concrete milestone / target dates for M4 + M5 against the real Circuit 4 proof, and for wiring `BridgeWithdrawalAggregatorVerifier.sol` into `AckiNackiBridge.withdrawByProof`?
3. Do circuits 1A/1B/2 also migrate to the aggregator path (Yul), or are they staying on gnark and getting a real `Define()` later? If gnark, what's the plan and timeline?
4. Transcript: do you need us to switch the Circuit 4 prover from Blake2b to Poseidon, or will you add a Blake2b loader to `snark-verifier`? Please confirm which side owns the change.
5. Until R15 closes, please confirm in writing that the intended trust model for Sepolia is relayer-rooted attestation, and that mainnet is explicitly gated on R15 (no `v2.0.0` tag before).

---

## Q2 — `bridge-prover-orchestrator/src/` largely duplicates `bridge-prover-lib`; please refactor

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

All four additions are additive (no breaking changes to lib's existing public API) and live next to the existing primitives in `bridge-prover-lib/src/{prover,verifier,layer_prover}.rs`. Built clean (`cargo build -p bridge-prover-lib`). The orchestrator's `Cargo.toml` already pins the lib by git URL — just `cargo update` to pick them up.

**Keep — genuinely orchestrator-specific:**
- `poseidon_transcript.rs` — vendored Poseidon FS transcript, only used by the gnark export path.
- `halo2_tvm_bundle.rs` — `ZKHALO2VERIFYWITHVK` TVM opcode payload (AN-side TVM, not relevant to lib consumers).
- `proof_export.rs` — gnark wrapper IO glue.
- `bound_test_data.rs`, `layer_hashes_test_data.rs` — cross-circuit fixtures for the bin binaries; may eventually graduate into `bridge-prover-lib` test helpers, but not blocking.

**Request.** After we ship the lib accessors + transcript-kind overload, please refactor `src/bin/{export_fallback_proof,export_layer_hashes_proof,export_bound_block_proofs}.rs` to import from `bridge_prover_lib` and delete the five files above. Net effect: orchestrator `src/` shrinks from ~2.9 kLoC to ~1.7 kLoC of code that is genuinely orchestrator-only.
