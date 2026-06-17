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

## Q2 — gnark-wrapper PI docstrings drift from upstream circuit sources

Cross-checked `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-{1a,1b,2,4}/circuit.go` against `gosh-sh/acki-nacki-to-eth-bridge-halo2-circuits`. Counts are all correct; only the comment labels drift. Wrappers themselves are PI-shape opaque (`AssertIsEqual(PI[i], PI[i])`), so this is a documentation/contract-clarity issue, not a soundness bug — but it will mislead anyone reading the wrapper to understand the public-input meaning.

| # | Wrapper says (slot → name) | Upstream source returns | Action |
|---|---|---|---|
| **1A** | `[0] envelope_hash_fr`, `[1] bk_set_commitment_fr` | `primary_circuit.rs:48,170,352`: `(block_id_fr, old_bk_set_commitment, block_seq_no_fr, last_seen_seqno)` | rename `envelope_hash_fr` → `block_id_fr` |
| **1B** | `[0] envelope_hash`, `[1] bk_set_poseidon` | `fallback_circuit.rs` pushes `block_id_fr` at `assigned_instances[0]` | rename `envelope_hash` → `block_id` |
| **2**  | `[0] block_id`, `[1] bk_set_poseidon`, `[2] num_layers`, `[3..=12] layer_hash_frs[0..10]`, `[13] prev_max_level_layer_hash` — 14 PIs | `historical-layer-hashes-movement-checker-circuit/src/circuit.rs:290-296` matches (source label is `bk_set_hash_cell`, semantically the BK-set Poseidon commitment) | OK; optional: align label to `bk_set_poseidon_hash` |
| **4**  | 10 PIs, names per `PUB_*` constants | `bridge-event-prove-circuit/src/bridge_event_prove_circuit.rs:113-123` — `TOTAL_PUBLIC_INPUTS = 10`, identical ordering | OK ✅ |

**Question.** Please confirm we should (a) re-label the 1A/1B wrapper comments to `block_id*` to match the actual values pushed into `assigned_instances[0]` upstream, and (b) leave Circuit 2's `bk_set_poseidon` label as-is or rename to `bk_set_poseidon_hash`. No `circuit.go` *code* changes — only comments — are implied by this; the wrappers' identity-stub `Define()` will still be replaced wholesale under R15.
