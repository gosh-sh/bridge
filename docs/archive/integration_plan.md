> **⚠️ ARCHIVED 2026-08-18 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/ETH-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

# Acki Nacki Bridge — Integration Plan (Legacy, M0–M9)

> **⚠ Superseded for M7–M9 (relayer + AN-side verification) by `docs/an_partner_integration_plan.md`.**
> The single-circuit architecture (`LayerHashesUpdateCircuit` + `LayerHashBridge.sol`) described
> below was retired in **Phase 4.2** (2026-05-10). The active four-circuit architecture is
> documented in `docs/four_circuit_architecture.md`. This plan is preserved as historical
> record only — M7–M9 are obsolete and replaced by Phases 4–7 of the partner integration plan.
>
> **Additional Phase 4.3 supersession (2026-05-17).** The "ETH-side deposit verifier" portions
> of M0–M6 are also obsolete: the legacy refund-style `withdraw(...)`, the `IAckiNackiVerifier`
> interface, the `Groth16DepositVerifier` adapter, the gnark-generated `Groth16Verifier` for the
> deposit-prover, the `DummyVerifier`, and the `deposit-prover/gnark-wrapper/` Go module + the
> Rust `groth16_wrapper` glue were all removed. The ETH→AN deposit proof is now consumed
> natively on the AN side via the future `VERHALO2SHPLONK` TVM opcode. See Decision Log
> 2026-05-17 in `docs/an_partner_integration_plan.md` for the rationale and the §6.5 R&D track
> that this supersession references. The deposit-prover Halo2 circuit itself (M3) is still the
> source of truth; the AAVE V3 yield bolt-on (M6) and the block-hash oracle (M5) are also
> still in production — the oracle is currently unused but preserved for a future burn-proof
> ETH-side withdrawal flow.
>
> Do not use as a forward-looking reference.

---

## 1. Goal

Enable the Ethereum bridge contract to track Acki Nacki blockchain state by verifying Groth16-wrapped Halo2 proofs of block attestations and updating stored layer hashes accordingly.

**End state**: A relayer watches the Acki Nacki node, generates a Halo2 proof of each new key block's layer hashes, wraps it in Groth16, and submits it to an Ethereum contract that verifies the proof and updates its on-chain layer hash state.

---

## 2. Architecture

```
Acki Nacki Node
    │
    ▼
circuit-data-exporter (captures block + attestation + BK set)
    │
    ▼
LayerHashesUpdateCircuit (Halo2, K=19, BN254)
    │  13 public inputs: [bk_set_commit, num_layers, layer_hashes[0..10], prev_hash]
    ▼
Export Halo2 proof → JSON
    │
    ▼
gnark-wrapper (Go): Halo2 → Groth16 on BN254
    │  256-byte Groth16 proof + public inputs
    ▼
Ethereum: LayerHashVerifier contract
    │  Calls Groth16Verifier (gnark-generated)
    ▼
LayerHashBridge contract
    │  Stores and updates layer hashes
    ▼
Bridge can now prove AN→ETH transfers against stored layer hashes
```

---

## 3. Prerequisites

### 3.1 Repository Access

| Repository | Status | Action |
|------------|--------|--------|
| `gosh-sh/gosh-halo2-crypto-lib` | Access denied | Request from Alina; clone to `../gosh-halo2-crypto-lib` |
| `gosh-sh/layer-hashes-update-halo2-circuit` | Available | Already cloned at `../layer-hashes-update-halo2-circuit` |
| `gosh-sh/gosh-zk-snark-halo2-utils` | Available | Already cloned at `../gosh-zk-snark-halo2-utils` |

### 3.2 Build Verification

Once `gosh-halo2-crypto-lib` is cloned:

```bash
# Build the layer-hashes circuit
cd ../layer-hashes-update-halo2-circuit
cargo build --features small-window

# Run MockProver tests (fast, no real proof generation)
cargo test --features small-window -- --test-threads=1

# Run real data tests (requires fixtures in circuit/tests/fixtures/)
cargo test --features small-window test_real_data -- --test-threads=1
```

### 3.3 Key Generation

Using `gosh-zk-snark-halo2-utils`:

```bash
cd ../gosh-zk-snark-halo2-utils

# Generate PK/VK for depth-3 (small-window) circuit (~5 GB proving key)
cargo test --features small-window test_layer_hashes_keygen_d3 -- --nocapture

# Verify proof generation and verification work
cargo test --features small-window test_layer_hashes_prove_and_verify_all_fixtures_d3 -- --nocapture
```

---

## 4. Integration Steps

### Step A: Adapt gnark-wrapper for Layer Hashes Circuit

**Goal**: Wrap the partner's Halo2 proof in a Groth16 proof verifiable on Ethereum.

**Current state**: Our `deposit-prover/gnark-wrapper` wraps deposit circuit proofs (7 public inputs). It needs adaptation for the layer-hashes circuit (13 public inputs, different VK/protocol metadata).

**What to change**:

1. **Create a new gnark-wrapper variant** (or parameterize the existing one):
   - Location: `deposit-prover/gnark-wrapper-layer-hashes/` (new Go module) or a configurable path in the existing wrapper
   - The circuit struct (`circuit.go`) needs `PublicInputs [13]frontend.Variable` instead of `[7]`
   - Witness commitment array sizes must match the new proof format
   - `PreprocessedCommitments` length will differ
   - Evaluations count will differ

2. **Export a sample Halo2 proof in JSON format**:
   - Write a Rust binary (in `deposit-prover/` or as a standalone tool) that:
     - Loads a fixture from `layer-hashes-update-halo2-circuit/circuit/tests/fixtures/`
     - Builds the circuit and generates a Halo2 proof using `gosh-zk-snark-halo2-utils`
     - Exports the proof + protocol metadata in the JSON format expected by gnark
   - The JSON must include: `public_inputs` (13 decimal strings), `proof_bytes` (hex), `protocol` (k, num_witness per phase, preprocessed commitments, etc.)

3. **Run gnark setup**:
   ```bash
   cd gnark-wrapper-layer-hashes
   go run . setup    # Input: halo2_proof.json → Output: circuit.r1cs, keys, Groth16Verifier.sol
   ```

4. **Run gnark prove**:
   ```bash
   go run . prove halo2_proof.json    # Output: groth16_proof_bytes.hex
   ```

**Key differences from deposit wrapper**:
- 13 public inputs instead of 7
- No `promise_commit` appended (the layer-hashes circuit doesn't use keccak coprocessor)
- Different proof byte layout (different witness commitment counts per phase)
- Proof format output: 256-byte Groth16 proof (same) + separate public inputs (no appended commitment)

**Decision needed**: Whether to append all 13 public inputs after the 256-byte proof (similar to how deposit appends `promise_commit`), or pass them as calldata. Recommendation: pass public inputs as calldata since the bridge contract needs them for state updates.

### Step B: Ethereum Contracts

**Goal**: Deploy contracts that verify layer-hash proofs and update bridge state.

#### B.1 Groth16Verifier (auto-generated)

The gnark setup (Step A.3) produces `Groth16Verifier.sol` with:
```solidity
function verifyProof(uint256[8] calldata proof, uint256[13] calldata input) public view
```

This is a new verifier (different from the deposit `Groth16Verifier` which takes `uint256[7]`). Deploy as a separate contract, e.g., `LayerHashGroth16Verifier.sol`.

#### B.2 LayerHashVerifier.sol (new)

Adapter contract similar to `Groth16DepositVerifier.sol`:

```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

interface ILayerHashVerifier {
    /// Verify a layer hash update proof.
    /// @param proof 256-byte Groth16 proof
    /// @param bkSetCommitment Poseidon commitment to BK set
    /// @param numLayers Number of active layers
    /// @param layerHashes Array of 10 layer hash Fr values
    /// @param prevMaxLevelLayerHash Previous top-level hash (chain anchor)
    /// @return isValid Whether the proof is valid
    function verifyLayerHashUpdate(
        bytes calldata proof,
        uint256 bkSetCommitment,
        uint256 numLayers,
        uint256[10] calldata layerHashes,
        uint256 prevMaxLevelLayerHash
    ) external view returns (bool isValid);
}
```

Implementation:
- Validate proof length = 256 bytes
- Decode 8 × uint256 Groth16 proof points
- Assemble 13 circuit public inputs: `[bkSetCommitment, numLayers, layerHashes[0..10], prevMaxLevelLayerHash]`
- Call `layerHashGroth16Verifier.verifyProof(proof, inputs)`

#### B.3 LayerHashBridge.sol (new)

Bridge contract that stores and updates layer hash state:

```solidity
contract LayerHashBridge {
    ILayerHashVerifier public verifier;

    // Current BK set commitment (updated via separate mechanism)
    uint256 public currentBkSetCommitment;

    // Current layer hashes (up to 10 layers)
    uint256[10] public currentLayerHashes;
    uint256 public currentNumLayers;

    // Events
    event LayerHashesUpdated(
        uint256 indexed blockHeight,
        uint256 numLayers,
        uint256 prevHash,
        uint256 timestamp
    );

    /// Submit a layer hash update proof.
    function updateLayerHashes(
        bytes calldata proof,
        uint256 numLayers,
        uint256[10] calldata newLayerHashes,
        uint256 prevMaxLevelLayerHash
    ) external {
        // Verify: prevMaxLevelLayerHash matches our stored top-level hash
        // (or is zero for initial state)
        require(
            prevMaxLevelLayerHash == currentLayerHashes[currentNumLayers - 1]
                || currentNumLayers == 0,
            "prev hash mismatch"
        );

        // Verify the proof
        bool valid = verifier.verifyLayerHashUpdate(
            proof,
            currentBkSetCommitment,
            numLayers,
            newLayerHashes,
            prevMaxLevelLayerHash
        );
        require(valid, "invalid proof");

        // Update state
        currentNumLayers = numLayers;
        for (uint256 i = 0; i < 10; i++) {
            currentLayerHashes[i] = newLayerHashes[i];
        }

        emit LayerHashesUpdated(0, numLayers, prevMaxLevelLayerHash, block.timestamp);
    }
}
```

**Open design questions** (see Section 6):
- How to handle BK set rotation (`currentBkSetCommitment` updates)
- Whether to store layer hash checkpoint history
- Access control on `updateLayerHashes` (permissionless? relayer-only?)

### Step C: Proof Export Pipeline (Rust)

**Goal**: Create a Rust binary that takes circuit test data and exports the Halo2 proof in the JSON format expected by gnark.

**Location**: New binary in `deposit-prover/examples/` or a new crate.

**Workflow**:
1. Load circuit test data (from fixture JSON or from a live node exporter)
2. Build `LayerHashesUpdateCircuit` with the data
3. Generate Halo2 proof using `gosh-zk-snark-halo2-utils::Proof::create_for_circuit`
4. Extract protocol metadata (k, witness counts per phase, preprocessed commitments)
5. Serialize to the JSON format matching `deposit-prover/gnark-wrapper/types.go::Halo2ProofData`
6. Write to file for gnark consumption

**Challenge**: The layer-hashes circuit uses a different Halo2 stack (halo2-base 0.4.0 from Gosh fork) than our deposit-prover (axiom-eth's halo2). These are separate workspaces with incompatible dependencies. The export tool should live in the layer-hashes workspace or a new standalone workspace.

**Recommended approach**: Create a `layer-hashes-prover/` directory at the project root, similar to `deposit-prover/`, as a standalone workspace:

```
layer-hashes-prover/
├── Cargo.toml        # Depends on layer-hashes-update-halo2-circuit + gosh-zk-snark-halo2-utils
├── src/
│   └── lib.rs
├── examples/
│   ├── export_proof_for_gnark.rs
│   └── test_with_fixture.rs
└── gnark-wrapper/    # Go module for Groth16 wrapping
    ├── main.go
    ├── circuit.go
    ├── go.mod
    └── ...
```

### Step D: End-to-End Testing

**Phase 1: Fixture-based testing (no live node)**

1. Use the 4 test fixtures from `layer-hashes-update-halo2-circuit/circuit/tests/fixtures/`
2. For each fixture:
   a. Generate Halo2 proof (Rust, using `gosh-zk-snark-halo2-utils`)
   b. Export to JSON
   c. Run gnark prove → 256-byte Groth16 proof
   d. Deploy `LayerHashGroth16Verifier` + `LayerHashVerifier` + `LayerHashBridge` to Anvil
   e. Submit proof + public inputs to `updateLayerHashes()`
   f. Verify state updates match expected layer hashes

**Phase 2: Sequential updates**

Test the chain of updates using fixtures in order:
1. Initialize bridge (empty state)
2. Submit L2_H16_prevH0 → bridge stores layer hashes from height 16
3. Submit L2_H32_prevH16 → bridge updates; verify `prevMaxLevelLayerHash` matches previous state

**Phase 3: Negative tests**

- Submit proof with wrong `bkSetCommitment` → expect rejection
- Submit proof with wrong `prevMaxLevelLayerHash` → expect rejection
- Submit proof with corrupted proof bytes → expect rejection
- Submit proof with wrong `numLayers` → expect rejection

**Phase 4: Live node testing**

1. Run a local Acki Nacki node with `history_proofs` feature and small window (size 4)
2. Use `circuit-data-exporter` to capture blocks
3. Generate proofs for each captured block
4. Submit to Sepolia bridge contract
5. Verify layer hash updates track the live chain

### Step E: Relayer Service

**Goal**: Automated service that keeps Ethereum's layer hashes in sync with Acki Nacki.

**Architecture**:
```
┌─────────────────┐     ┌──────────────┐     ┌───────────────┐
│ AN Node (BM API) │────▶│ Relayer      │────▶│ Ethereum      │
│ /v2/bk_set      │     │ - Watch      │     │ - Verify      │
│ GraphQL blocks   │     │ - Prove      │     │ - Update      │
│ Block stream     │     │ - Wrap       │     │ - Store       │
└─────────────────┘     │ - Submit     │     └───────────────┘
                        └──────────────┘
```

**Components**:
1. **Block watcher**: Connects to BM's QUIC block stream (`:12000`) or polls GraphQL for new finalized blocks
2. **Data extractor**: Extracts block envelope, attestation, BK set, and layer hash data
3. **Prover**: Runs `LayerHashesUpdateCircuit` to generate Halo2 proof
4. **Wrapper**: Calls gnark to wrap in Groth16
5. **Submitter**: Sends Ethereum transaction with proof + public inputs

**Implementation considerations**:
- Proving is expensive (~minutes per block for K=19); relayer should queue and batch if needed
- Keys (~5 GB PK) must be pre-generated and loaded at startup
- Error handling: retry on Ethereum tx failures, skip blocks where proof generation fails
- Monitoring: track proof generation time, submission success rate, gas costs

---

## 5. Timeline and Milestones

| Milestone | Description | Dependencies | Est. Effort |
|-----------|-------------|--------------|-------------|
| **M0** | Get `gosh-halo2-crypto-lib` access; build + test partner circuit | Alina grants access | 1 day |
| **M1** | Proof export: Rust binary exporting Halo2 proof as JSON | M0 | 2-3 days |
| **M2** | gnark-wrapper adaptation for 13 public inputs | M1 (needs sample JSON) | 2-3 days |
| **M3** | Groth16Verifier.sol generation + LayerHashVerifier.sol | M2 (needs gnark setup) | 2-3 days |
| **M4** | LayerHashBridge.sol + Foundry tests | M3 | 2-3 days |
| **M5** | Fixture-based E2E test (all 4 fixtures on Anvil) | M1-M4 | 2-3 days |
| **M6** | Sequential update test + negative tests | M5 | 1-2 days |
| **M7** | Live node testing (local AN node + Sepolia) | M6 + local AN node setup | 3-5 days |
| **M8** | Relayer service (basic, manual trigger) | M7 | 3-5 days |
| **M9** | Relayer service (automated, monitoring) | M8 | 3-5 days |

**Critical path**: M0 → M1 → M2 → M3 → M5

**Total estimated effort**: 3-5 weeks for M0-M6 (core integration); 2-3 additional weeks for M7-M9 (live testing + relayer).

---

## 6. Open Questions for Partner Discussion

### 6.1 BK Set Rotation

**Question**: How does the bridge learn about new BK sets?

The circuit proves a block attestation under a specific `bk_set_commitment`. If the BK set changes (epoch transition), the bridge's stored `currentBkSetCommitment` becomes stale and new proofs will fail.

**Options**:
1. **Separate BK set update proof**: A second circuit that proves the BK set transition is valid (signed by the old BK set)
2. **Embedded in layer hash updates**: Add BK set change data to the layer-hash proof when it occurs
3. **Trusted relayer**: Allow a trusted party to update the BK set commitment (weakens trust model)
4. **Genesis committee**: Start with a trusted initial BK set and prove all transitions

**Recommendation**: Option 1 (separate proof) or Option 2 (embedded). Discuss with Alina whether the partner already has a BK set change circuit planned.

### 6.2 Bridge Contract State Design

**Question**: Store only the latest layer hashes, or maintain checkpoint history?

Alina mentioned "checkpoints every 10" as a possible production design.

**Options**:
1. **Latest only**: Store `currentLayerHashes[10]` — simplest, cheapest gas
2. **Checkpoints**: Store layer hashes at regular intervals (e.g., every 10 blocks) in a ring buffer or mapping — enables proving against historical state
3. **Merkle accumulator**: Maintain an on-chain Merkle tree of all historical layer hash snapshots

**Recommendation**: Start with Option 1 for the initial integration; design the contract to be upgradeable so checkpoints can be added later.

### 6.3 Production Circuit Parameters

**Question**: What parameters will be used in production?

| Parameter | Test | Production | Impact |
|-----------|------|------------|--------|
| `LAYER_TREE_DEPTH` | 2 (window=4) | 8 (window=128) | Different VK; gnark setup must match |
| `MAX_BLOCK_DATA_BYTES` | 4096 | 4096? | Verify sufficient for production blocks |
| `MAX_SIGNERS` | 300 | 300? | Verify against production BK set size |
| K | 19 | 19? | Affects proof size, verification cost |

**Recommendation**: Confirm these with the partner before running production gnark setup (keys are circuit-specific).

### 6.4 Proof Verification Gas Cost

**Question**: What is the expected gas cost for on-chain Groth16 verification with 13 public inputs?

Our deposit verification (7 inputs) costs ~280k gas. With 13 inputs, expect ~320-350k gas (public input processing adds ~5k per input). This is well within Ethereum's block gas limit.

### 6.5 Halo2-to-Gnark Proof Export Format

**Question**: The current gnark wrapper's `circuit.go` has a stub `Define` (identity constraints only). Full in-circuit Halo2 verification is not yet implemented. What is the security model?

**Current state**: The gnark wrapper compiles a circuit that accepts public inputs and a domain size check, but does not actually verify the Halo2 proof inside Groth16. This means the Groth16 proof currently only commits to the public inputs, not to the Halo2 proof's validity.

**For production**: Either (a) implement full Halo2 SHPLONK verification inside the gnark circuit (complex, may require BN254 pairing + transcript in R1CS), or (b) use a different wrapping strategy (e.g., recursion via an intermediate IVC step, or a direct Halo2 verifier on Ethereum using EIP-7212 / precompile proposals).

**Recommendation**: This is the same situation as the deposit wrapper. For the initial integration, the gnark wrapper provides a "compressed public input commitment" rather than full proof verification. Discuss with the team whether this security model is acceptable for the initial deployment, with full verification as a future improvement.

---

## 7. Risk Register

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| G2 subgroup check gap in BLS path | Medium | Potential soundness concern | Add explicit subgroup check or prove pairing equation suffices (see audit FORK-2, BLS-1) |
| Bincode layout changes in AN node | Low | Circuit produces invalid proofs | Integration tests against node types; pin node version |
| Halo2 proof format incompatible with gnark parser | Medium | Blocks M2 | Export a real proof early; iterate on parser |
| Production block size > 4096 bytes | Low | Circuit cannot prove large blocks | Verify with partner; MAX_BLOCK_DATA_BYTES is configurable |
| Gas cost too high for frequent updates | Low | Economics unsustainable | Batch updates; use L2 for frequent state; L1 for checkpoints |
| BK set rotation not handled | High | Bridge goes stale after first epoch | Design rotation mechanism early (Question 6.1) |
| gnark wrapper stub `Define` | High | Groth16 proof doesn't verify Halo2 | Document as known limitation; plan full verification |

---

## 8. Deliverables Checklist

- [x] Clone `gosh-halo2-crypto-lib` and review source
- [x] Clone `halo2-lib-zkevm-sha256-and-bls12-381` fork; review and audit BLS12-381 additions
- [x] Build partner circuit (requires `state_to_bytes` pub fix in sha256-chip)
- [x] Run mock prover tests (all pass: k0, k1, fixture L5/S11, fixture L6/S11)
- [x] Run real prover test (keygen + prove + verify, consistent VK, ~26 min)
- [x] Rust binary: `layer-hashes-prover/src/export_proof.rs` — exports Halo2 proof as JSON for gnark
- [x] Go module: `layer-hashes-prover/gnark-wrapper/` — Groth16 wrapper for 13 public inputs
- [x] gnark setup: `LayerHashGroth16VerifierGenerated.sol` generated (14 constraints, 13 public inputs)
- [x] Solidity: `LayerHashVerifier.sol` (adapter) + `ILayerHashVerifier.sol` + `LayerHashGroth16Verifier.sol` (interface)
- [x] Solidity: `LayerHashBridge.sol` (state storage + update with chain anchoring)
- [x] Foundry tests: 17 unit tests for LayerHashVerifier + LayerHashBridge (all pass)
- [x] E2E test: fixture-based proof on Foundry (real Groth16 proof verified on-chain, ~287k gas)
- [x] E2E test: bridge state update (proof → updateLayerHashes → verify stored state)
- [x] E2E test: negative test suite (wrong commitment, wrong layers, wrong hash, wrong prevHash, corrupted proof — all rejected)
- [x] Real keygen via `gosh-zk-snark-halo2-utils` (PK 5.3GB, VK 11KB, ~11 min)
- [x] Real proof generation for all 4 fixtures via `test_layer_hashes_prove_and_verify_all_fixtures_d3` (~22 min)
- [x] Groth16 wrapping for all 4 fixtures (convert-proof → gnark setup → gnark prove)
- [x] E2E Foundry: all 4 real Groth16 proofs verified on-chain (14 E2E tests, ~287k gas verify, ~457k gas bridge update)
- [x] Sequential bridge update: L2_H16 → L2_H32 with BK set rotation and chain anchoring
- [x] AAVE V3 yield integration: `AckiNackiBridge` extended with `supplyToAave` / `withdrawFromAave` / `emergencyWithdrawAll` / `harvestYield`; principal-segregated yield accounting; 23 unit + fuzz tests with mock AAVE; `USE_AAVE=true` opt-in deployment flag (see `docs/aave_integration.md`)
- [ ] AAVE: mainnet fork tests against real `WrappedTokenGatewayV3` + `Pool`
- [ ] E2E test: live node + Sepolia (testnet not ready; circuit-data-exporter on `bridge_halo2_tests` branch)
- [ ] Relayer service: basic implementation
- [ ] Documentation: update main README with layer-hash pipeline
