# Circuit 1A — Yul verifier implementation plan

**Scope:** concrete, file-level plan to replace the identity-stub gnark Groth16 wrapper for **Circuit 1A (Primary attestation)** with a real Yul Halo2 verifier, following the same `snark-verifier-sdk` pipeline Axiom V2 uses in production.

**Cross-references:** `bridge_evm_review_questions.md` Q1/Q2, `halo2_on_chain_verification_paths.md`, `docs/r15_snark_verifier_roadmap.md`. M2/M5 of the roadmap are the prerequisite spike work that's already closed.

---

## Main obstacle — and why it's smaller than it looks

**The single technical obstacle is the transcript.** Everything else is plumbing.

- Circuit 1A's prover (`bridge_prover_lib::prover::generate_primary_proof`) emits a **Blake2b**-transcripted SHPLONK proof.
- The aggregator (`snark_verifier_sdk::AggregationCircuit`) can only verify in-circuit against a **Poseidon**-transcripted SHPLONK proof — there is no Blake2b loader in `snark-verifier`.
- Until Circuit 1A's prover is invoked with Poseidon, `bridge-evm-aggregator` cannot consume its output.

**The infrastructure is already there:**
- `bridge-prover-lib/src/prover.rs:213` exposes the generic helper `create_proof_with_transcript<E, T, C>` — explicitly designed to swap Blake2b for Poseidon without re-implementing keygen.
- `bridge-prover-lib/src/prover.rs:205-212` documents the intent: *"a Poseidon transcript for snark-verifier consumption"*.
- `crates/bridge-snark-utils/src/prover.rs:90` already has a `TranscriptKind::Poseidon` scaffolding.

So the obstacle is "call this helper with `PoseidonWrite` instead of `Blake2bWrite`" — half a day of code, plus byte-level snark serialization to disk so the workspace-isolated aggregator can read it back.

Everything else (curve, PI shape, K-sizing, instance exposure) is already handled by what's in `bridge-evm-aggregator/src/aggregator.rs` today.

---

## What the current `bridge-evm-aggregator/src/` already does

```rust
pub fn prove_inner(params, a, b) -> Snark          // multiply placeholder
pub fn aggregate(agg_params, inner_snark) -> Snark // wraps ANY Snark
pub fn generate_yul_verifier(...) -> usize         // emits Yul from aggregator VK
```

`aggregate` and `generate_yul_verifier` are **already generic** — they take a `Snark` and don't care what produced it. `expose_previous_instances(false)` re-surfaces *whatever* PIs the inner had — Circuit 1A's 4 PIs (`[block_id_fr, bk_set_commitment_fr, block_seq_no, last_seen_block_seqno]`) drop in unchanged. Constants `K_OUTER=21`, `NUM_ACCUMULATOR_INSTANCES=12` carry over.

The only file with multiply-specific semantics is `prove_inner` + the entire `multiply.rs`. Those get replaced by a **loader**, not a re-implementation.

---

## What must be added to `bridge-evm-aggregator/`

### New file `src/primary_snark_loader.rs`

Replaces `prove_inner` for the Primary path. Reads bytes the orchestrator wrote with Poseidon transcript.

```rust
pub struct PrimarySnarkArtifacts {
    pub proof_path: PathBuf,       // SHPLONK proof bytes
    pub instances_path: PathBuf,   // [block_id, bk_commitment, seqno, last_seen]
    pub vk_path: PathBuf,          // serialized VerifyingKey<G1Affine>
    pub k_inner: u32,              // Circuit 1A's K (likely 17-19)
}

pub fn load_primary_snark(
    params: &ParamsKZG<Bn256>,
    artifacts: &PrimarySnarkArtifacts,
) -> anyhow::Result<Snark> {
    // 1. Deserialize VerifyingKey<G1Affine, _>
    // 2. Read raw proof bytes
    // 3. Read instances Vec<Vec<Fr>>
    // 4. Build snark_verifier_sdk::Snark { protocol, instances, proof }
}
```

**Critical detail — VK serialization format must match what the partner side writes.** Two options:

- **Option L1 (preferred):** Standardize on `snark_verifier_sdk::Snark::from_path` / bincode helpers. Both sides depend on the same `snark-verifier-sdk` version. Simplest.
- **Option L2:** Use existing `VkBlob` format from `bridge_prover_lib::keys::VkBlob`. More glue but avoids forcing partner side to add a snark-verifier-sdk dependency.

### Refactor `aggregator.rs`

No change to `aggregate` or `generate_yul_verifier` — they already take a `Snark`. Caller swaps where the snark comes from.

### New binary `src/bin/build_primary_aggregator_verifier.rs`

```
1. ParamsKZG::read(srs_path)
2. let inner_snark = load_primary_snark(&params, &artifacts)?
3. let _agg_snark = aggregate(&params_outer, inner_snark.clone())?  // smoke test
4. let bytecode_size = generate_yul_verifier(
       &params_outer,
       &inner_snark,
       Path::new("../../contracts/ethereum/src/PrimaryAggregatorVerifier.bin"),
   )?
5. assert!(bytecode_size < 24576)  // EIP-170 guard
```

### New `tests/primary_real_inner.rs`

End-to-end with a real-from-disk Circuit 1A artifact. Mirrors `tests/round_trip.rs` but with Primary instances instead of `[77]`.

### `Cargo.toml` additions

Nothing structural. The spike's `Cargo.toml` already pulls `snark-verifier-sdk` with `halo2-pse`. The loader only needs `serde`, `bincode`, `std::fs` — all already present.

---

## What must be added upstream (`acki-nacki-to-eth-bridge-halo2-prover`)

### `bridge-prover-lib/src/prover.rs` — add `generate_primary_proof_poseidon`

```rust
pub fn generate_primary_proof_poseidon(
    params: &ParamsKZG<Bn256>,
    pk: &ProvingKey<G1Affine>,
    circuit: BaseCircuitBuilder<Fr>,
    instances: Vec<Vec<Fr>>,
) -> anyhow::Result<PrimaryPoseidonOutput> {
    // Drive create_proof_with_transcript<_, PoseidonWrite, _>
    // (helper already exists at prover.rs:213)
}

pub struct PrimaryPoseidonOutput {
    pub snark_bytes: Vec<u8>,  // serialized snark_verifier_sdk::Snark
    pub vk_bytes: Vec<u8>,     // serialized VerifyingKey<G1Affine>
    pub instances: Vec<Vec<Fr>>,
}
```

Reuse the existing `create_proof_with_transcript` helper — no new keygen logic.

### `bridge-snark-utils` — new export binary

`crates/bridge-snark-utils/src/bin/export_primary_poseidon_snark.rs`:
- Writes `snark.bin`, `instances.json`, `vk.bin` into a stable directory the bridge-evm-aggregator binary reads from.

---

## What must be added in `contracts/ethereum/`

### New `src/PrimaryAggregatorVerifier.sol` adapter

One-line proxy that forwards calldata to the deployed raw-Yul-bytecode contract address. Pattern already exists — copy from `BridgeWithdrawalVerifier.sol`.

Keep the `IGroth16Verifier` interface signature so `AckiNackiBridge.sol` doesn't change its callsite contract. Calldata shape differs (~256 B Groth16 → ~24 KB Yul); the adapter does the marshalling.

### Update `AckiNackiBridge.sol`

```diff
- import { PrimaryVerifier } from "./PrimaryVerifier.sol";
+ import { PrimaryAggregatorVerifier as PrimaryVerifier } from "./PrimaryAggregatorVerifier.sol";
```

### Foundry test harness

Pattern already exists for `vm.readFileBinary` of Yul bytecode — copy from `test/Blake2bHalo2Verifier.t.sol` lines 55, 201, 212.

---

## Step-by-step plan, ordered

| # | Where | Work | Effort | Depends on |
|---|---|---|---|---|
| 1 | `bridge-prover-lib` | Add `generate_primary_proof_poseidon` calling existing `create_proof_with_transcript` | 0.5 day | — |
| 2 | `bridge-snark-utils` | Add `export_primary_poseidon_snark` binary writing `snark.bin` / `instances.json` / `vk.bin` | 1 day | (1) |
| 3 | `bridge-evm-aggregator` | Add `src/primary_snark_loader.rs` | 1 day | (2) for artifacts to test against |
| 4 | `bridge-evm-aggregator` | Add `src/bin/build_primary_aggregator_verifier.rs` | 0.5 day | (3) |
| 5 | `bridge-evm-aggregator` | Add `tests/primary_real_inner.rs` round-trip | 1 day | (4) |
| 6 | `bridge-evm-aggregator` | Verify Yul bytecode < 24 576 B with 12 + 4 = 16 instance slots | hours | (4) |
| 7 | `contracts/ethereum/src/` | Add `PrimaryAggregatorVerifier.sol` adapter | 0.5 day | (4) |
| 8 | `contracts/ethereum/test/` | Add `PrimaryAggregatorVerifier.t.sol` Foundry harness | 1–2 days | (5)(7) |
| 9 | `contracts/ethereum/src/AckiNackiBridge.sol` | Swap import to new adapter | hours | (8) |
| 10 | full chain | E2E on Sepolia / local devnet | 2–3 days | (9) |

**Total: ~7–10 engineer-days for Circuit 1A end-to-end.**

After this, Circuits 1B/2/4 are largely cut-and-paste — same loader, same binary skeleton, same Solidity adapter pattern. Each adds ~2–3 days because:
- different PIs → different `num_instance` (need to re-verify K=21 fits each)
- different inner K (affects SRS choice)
- different `generate_*_poseidon` upstream wrapper

**All four circuits: ~3–4 weeks of engineering** if the partner-side Poseidon export is co-developed.

---

## Risks (small, ordered by likelihood)

1. **K=21 doesn't fit Circuit 1A's 16 instance slots** — very unlikely (instance count is dwarfed by advice). Mitigation: bump to K=22; Yul size grows logarithmically.
2. **Yul size bumps over 24 576 B** — empirically Axiom V2 runs ~16 KB with similar shapes; the spike is at 13 172 B with 13 PIs. Mitigation: expose only the PIs the bridge contract actually reads (seqnos may not need on-chain access).
3. **Calldata cost** — Circuit 1A is called per-block from `verifyBlock`. ~24 KB × ~10 gas/byte = ~240k gas of calldata alone; full verify ≈ 600k–800k gas per block. Acceptable today, worth measuring pre-mainnet. Mitigation: A/B with Keccak transcript (Axiom's choice).
4. **VK drift** — if Circuit 1A's circuit changes, the Yul verifier changes. Mitigation: pin Circuit 1A's circuits-repo commit in CI + check the VK fingerprint.

---

## Bottom line

The "Phase 8 R&D" framing in the existing gnark wrappers is misleading. The Yul path for Circuit 1A is **a ~2-week engineering task**, not a research project. M2/M5 already proved the hard parts (instance exposure, K-sizing, EIP-170 fit) on a synthetic inner.

What's left:
- 1 day partner side: swap Blake2b for Poseidon in one call site.
- 1 day of byte-level snark serialization glue between the two workspaces.
- 2–3 days of bridge contract adapter + Foundry harness.
- 1–2 days of E2E.

The rest is testing, audit prep, and cross-circuit replication. The aggregator code in `src/` doesn't need re-architecting — `aggregate` and `generate_yul_verifier` are already circuit-agnostic. They just need a non-multiply inner snark to point at.
