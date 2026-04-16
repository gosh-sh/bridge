# Acki Nacki Bridge — Agent Context

## Project Overview

Cross-chain bridge between Ethereum and [Acki Nacki](https://docs.ackinacki.com/) (TVM-based, multi-threaded blockchain). The bridge enables deposits on Ethereum to be proven on Acki Nacki, and Acki Nacki state (layer hashes) to be verified on Ethereum — both via ZK proofs.

## Repository Layout

```
acki-nacki-bridge/          ← this repo (Ethereum side + integration)
├── contracts/ethereum/     ← Solidity (Foundry): bridge contract, verifiers, oracles
├── crates/
│   ├── acki-nacki-interface/  ← Rust traits + mock for AN node communication
│   └── eth-frontend/          ← Rust Ethereum client (ethers-rs)
├── deposit-prover/         ← Rust Halo2 circuit: proves Ethereum deposit events
│   └── gnark-wrapper/      ← Go: wraps Halo2 SHPLONK proof into Groth16 for on-chain verification
├── layer-hashes-prover/    ← Rust: proof export + Go gnark-wrapper for layer-hash circuit
│   ├── src/                ← export_proof.rs (fixture→Halo2 proof→JSON), proof_export.rs (types)
│   └── gnark-wrapper/      ← Go: wraps layer-hash Halo2 proof into Groth16 (13 public inputs)
├── bk-set-rotation-prover/ ← ZK-proven BK set rotation (circuit spec + gnark-wrapper)
│   ├── CIRCUIT_SPEC.md     ← Circuit specification: what the rotation proof proves
│   └── gnark-wrapper/      ← Go: wraps rotation Halo2 proof into Groth16 (2 public inputs)
├── poseidon-proof/         ← Rust Halo2 circuit with Blake2b transcript (Poseidon commitments)
├── frontend/               ← WASM frontend (excluded from workspace)
├── scripts/                ← Shell scripts for verifier generation, deployment
├── docs/                   ← Architecture docs, audit reports, integration plan
├── e2e_test_data/          ← Test fixtures for end-to-end tests
├── e2e_attack_test_data/   ← Negative test fixtures
├── params/                 ← SRS parameters (KZG trusted setup)
├── Makefile                ← Entry point: make setup/build/test/deploy
├── setup.sh                ← One-time dependency install (Foundry, Go, Rust)
├── test.sh / test_e2e.sh   ← Test runners
└── .gitlab-ci.yml          ← CI pipeline
```

## Sibling Repositories (under ../  relative to this repo)

| Repo | Description |
|------|-------------|
| `layer-hashes-update-halo2-circuit` | Partner's Halo2 circuit proving AN block attestation + layer hash updates |
| `gosh-zk-snark-halo2-utils` | Generic keygen/prove/verify utilities with Blake2b transcript (SHPLONK) |
| `gosh-halo2-crypto-lib` | Crypto chips: SHA-256, BLS12-381 verification, dense balanced Merkle tree |
| `halo2-lib-zkevm-sha256-and-bls12-381` | Gosh fork of axiom-crypto/halo2-lib adding BLS12-381 support to halo2-ecc |
| `acki-nacki` | Acki Nacki node implementation |
| `tvm-sdk` | TVM SDK (account queries, message sending) |
| `bk-set-stub` | Minimal stub for `bk-set-change-verifier-halo2-circuit-with-better-sha256` (enables halo2-utils build without access to that private repo) |

### `circuit-data-exporter` (on `bridge_halo2_tests` branch of `acki-nacki`)

Lives at `acki-nacki/helpers/circuit_data_exporter` on the **`bridge_halo2_tests`** branch (not on `main`).
Queries a live AN node via GraphQL, fetches a key block, extracts layer hashes, generates a synthetic BLS attestation, and writes a JSON fixture for the circuit tests.

```bash
# Fetch the branch locally
cd ../acki-nacki && git fetch origin bridge_halo2_tests

# Build (needs to be in workspace or built standalone with node deps)
cargo build -p circuit-data-exporter

# Usage
cargo run -p circuit-data-exporter -- \
    --network http://127.0.0.1:8080 \
    --height 32 \
    --bk-set-size 5 \
    --num-prev-chain-steps 1 \
    --output circuit_test_data.json
```

Output: `circuit_test_data_L{layers}_H{height}_prevH{prev}_S{steps}.json` — the same format consumed by `test_real_data.rs` and `test_layer_hashes_d3.rs`.

**Requirements**: `--height` must be a layer-N key block (H % W^N == 0 for N≥1). For `small-window` (W=2): heights 2, 4, 8, 16, 32, …

### Acki Nacki Testnet

- **Node API**: `http://94.156.178.19:8600` (port 8600; HTTPS/443 is firewalled)
- **Working endpoints**: `/v2/bk_set`, `/v2/bk_set_update` (no auth required)
- **GraphQL**: NOT publicly exposed (gql-server is a separate binary; would need local setup)
- **Testnet status**: Not ready for E2E testing (as of Apr 2026)
- **Local 5-node cluster**: `cd ../acki-nacki/nock && docker-compose build && docker-compose up -d`
  - Node0 API: `http://127.0.0.1:11000`
  - Requires building with `history_proofs` feature for layer hash data

## ZK Proof Pipeline

### Deposit Flow (Ethereum → Acki Nacki)

1. User calls `AckiNackiBridge.deposit()` on Ethereum → emits `Deposit` event
2. `deposit-prover` (Halo2, K=20) proves the event was emitted: Receipt RLP, MPT inclusion, log matching, block hash binding
3. Keccak coprocessor handles SHA3/keccak256 via Poseidon promise commitments (~500x savings)
4. `gnark-wrapper` (Go) wraps Halo2 SHPLONK proof → Groth16 (BN254) for cheap on-chain verification
5. `Groth16DepositVerifier.sol` verifies on-chain (7 public inputs: depositId, sender, amount, contractAddress, blockHashHigh, blockHashLow, promiseCommit)

### Layer Hash Flow (Acki Nacki → Ethereum)

1. AN blocks contain layer hashes in BTreeMap data structure (bincode-serialized)
2. Block Keepers (BK) sign attestations using BLS12-381 aggregate signatures
3. `layer-hashes-update-halo2-circuit` (Halo2, K=19, BN254 Fr) proves:
   - SHA-256 binding of block data to attestation envelope hash
   - Primary target type constraint
   - Layer hash extraction at correct BTreeMap offsets
   - Poseidon dense balanced Merkle tree chain continuity
   - BLS12-381 committee signature verification (≥2/3 threshold)
   - BK set Poseidon commitment
4. Public inputs (13 field elements): old_layer_hashes_root, new_layer_hashes_root, bk_set_commitment, layer indices, block heights, chain lengths
5. Needs gnark-wrapper adaptation (13 inputs vs 7 for deposits) → `Groth16Verifier.sol` on Ethereum

## Solidity Contracts (Foundry)

| Contract | Purpose |
|----------|---------|
| `AckiNackiBridge.sol` | Main bridge: deposits, withdrawals, layer hash state |
| `Groth16Verifier.sol` | Auto-generated Groth16 verifier (gnark output) |
| `Groth16DepositVerifier.sol` | Adapter: decodes deposit proof public inputs, calls Groth16Verifier |
| `IAckiNackiVerifier.sol` | Interface for AN-side proof verification |
| `IBlockHeaderOracle.sol` | Interface for Ethereum block hash oracle |
| `AxiomBlockHeaderOracle.sol` | Axiom-based block hash oracle implementation |
| `Halo2Verifier.sol` | Direct Halo2 SHPLONK verifier (Yul-based, for testing) |
| `Blake2bHalo2Verifier.sol` | Blake2b-transcript Halo2 verifier |
| `Blake2bTranscript.sol` / `Blake2bChallengeComputer.sol` | On-chain Blake2b transcript replay |
| `DummyVerifier.sol` | Always-true verifier for testing |
| `MockBlockHeaderOracle.sol` | Mock oracle for testing |
| `LayerHashBridge.sol` | Stores AN layer hashes + BK set commitment; verifies ZK proofs for layer updates and BK rotation; timelock for emergency BK set changes |
| `LayerHashVerifier.sol` | Adapter: assembles 13 public inputs, calls Groth16 verifier |
| `ILayerHashVerifier.sol` | Interface for layer hash verification |
| `LayerHashGroth16Verifier.sol` | Interface for gnark-generated 13-input Groth16 verifier |
| `LayerHashGroth16VerifierGenerated.sol` | Auto-generated Groth16 verifier from gnark (13 inputs) |
| `IBkSetRotationVerifier.sol` | Interface for ZK-proven BK set rotation verification |
| `BkSetRotationVerifier.sol` | Adapter: assembles 2 public inputs (old/new commitment), calls Groth16 verifier |
| `BkSetRotationGroth16Verifier.sol` | Interface for gnark-generated 2-input Groth16 verifier |

Build: `cd contracts/ethereum && forge build`
Test: `cd contracts/ethereum && forge test`

## Rust Workspace

**Workspace members** (in `Cargo.toml`): `crates/eth-frontend`, `crates/acki-nacki-interface`
**Excluded** (separate dependency trees): `deposit-prover`, `frontend`, `poseidon-proof`

- `acki-nacki-interface`: Async traits (`AckiNackiClient`, `BlockProvider`, etc.) + mock implementations. No real AN node integration yet.
- `eth-frontend`: Ethereum client using ethers-rs. Interacts with bridge contracts.
- `deposit-prover`: Standalone Halo2 circuit crate. Uses axiom-crypto's halo2-lib (different from partner's gosh fork).
- `poseidon-proof`: Halo2 circuit with Blake2b transcript for Poseidon commitment proofs.

## Partner's Circuit Details

### `layer-hashes-update-halo2-circuit`

**Build**: `cargo build --features small-window` (uses LAYER_TREE_DEPTH=2 for testing)
**Test**: `cargo test --features small-window -- "mock"` for mock prover; real prover takes ~26 min

**Key feature flags**: `small-window` (depth=2, window=4 for testing), default is production depth=8

**Known build issue**: `state_to_bytes` in `gosh-sha256-chip` is private but called by the main circuit. Requires `pub` fix.

**Circuit parameters** (small-window): K=19, 80 advice columns, 2 lookup advice columns, lookup_bits=18

**Test fixtures**: `circuit/tests/fixtures/circuit_test_data_*.json` — real AN node data

### `gosh-halo2-crypto-lib`

Three crates, all using the Gosh fork of halo2-lib:
- **`sha256-chip`**: Standard SHA-256 with range-checked mod-2^32 arithmetic. Correct.
- **`bls-verification`**: BLS12-381 aggregate signature verification. Threshold: Primary (3s≥2n), Fallback (2s>n). Uses `load_private_g*_unchecked` + manual `check_is_on_curve`. No G2 subgroup check.
- **`dense-balanced-tree`**: Poseidon Merkle chain verification. MAX_CHAIN_LEN=11. Uses 3-chunk encoding (c0, c1, c2) with range checks.

### `halo2-lib-zkevm-sha256-and-bls12-381`

Gosh fork of axiom-crypto/halo2-lib. Single commit. Adds `bls12_381` module to halo2-ecc. Core halo2-base is unchanged. Pairing equation: `e(-g1, sig) · e(pk, H(m)) = 1`. Hash-to-curve follows RFC 9380.

### `gosh-zk-snark-halo2-utils`

- `keygen.rs`: `generate_and_save_keys` — runs keygen_vk/keygen_pk, saves VK + PK + config
- `proof.rs`: `Proof::create_for_circuit` / `verify_with_vk` — Blake2bWrite + ProverSHPLONK
- `io.rs`: Universal PK/VK deserialization using BaseCircuitBuilder

**Test flow for layer-hashes (depth=3, small-window)**:
1. `test_layer_hashes_keygen_d3` — generates PK (~5.3GB), VK (11KB), config in `keys/` (~11 min)
2. `test_layer_hashes_prove_and_verify_all_fixtures_d3` — proves all 4 fixtures with same key, verifies each (~22 min total)
3. Keys are reusable for any fixture from a W=4 node with blocks ≤ 4096 bytes

**Patching**: `Cargo.toml` needs `[patch]` sections pointing to local sibling repos (halo2-lib, layer-hashes-circuit, crypto-lib, bk-set-stub) to build without network access to private GitHub repos.

## Key Technical Concepts

### Proof Systems
- **Halo2** (KZG + SHPLONK): Used by both deposit-prover and layer-hashes circuit. BN254 scalar field.
- **Groth16** (BN254): Final on-chain verifier format. Wrapped from Halo2 via gnark.
- **Blake2b transcript**: Fiat-Shamir transcript used by partner's circuits (vs Poseidon/Keccak in axiom's).

### Crypto Primitives
- **Keccak256**: Ethereum hashing. Offloaded to coprocessor circuit in deposit-prover.
- **SHA-256**: Used in partner's circuit for block data hashing.
- **Poseidon**: In-circuit-friendly hash. Used for BK set commitment, dense Merkle tree, keccak coprocessor promises.
- **BLS12-381**: Acki Nacki Block Keeper signatures. Aggregate signature scheme.
- **Dense Balanced Tree**: Acki Nacki's Merkle tree variant using 3-chunk Poseidon hashing.

### Acki Nacki Blockchain
- TVM-based, multi-threaded (up to 512 threads per workchain)
- **Block Keepers (BK)**: Validate and attest blocks with BLS12-381 signatures
- **Block Managers (BM)**: Propose blocks
- **Epochs**: BK set rotates; each epoch has a new committee
- **Layer hashes**: Per-layer state commitments stored in dense balanced Merkle trees
- **Bincode serialization**: AN uses bincode for on-chain data structures. The circuit has hardcoded offsets for parsing.

### Bincode Layout Constants (in the circuit)
These are tightly coupled to the AN node's serialization and will break if the node changes format:
- `ENVELOPE_HASH_REL_OFFSET`, `TARGET_TYPE_REL_OFFSET`
- `BTREE_ENTRY_SIZE`, `BTREE_KEY_SIZE`, `BTREE_VALUE_SIZE`
- `MAX_BLOCK_DATA_BYTES = 4096`, `MAX_PADDED_BYTES = 4160`

## Open Audit Findings (Medium Severity)

1. **BLS-1 / FORK-2**: No G2 subgroup check in BLS12-381 path. `load_private_g2_unchecked` skips both on-curve and subgroup checks. Calling code adds on-curve but not subgroup. G2 cofactor ≠ 1.
2. **D-1**: No semantic BTreeMap validation in circuit. Layer hash extraction trusts that BLS-attested data is correctly formatted. Protocol-level trust assumption.
3. **L-1**: Bincode layout constants are fragile across AN node releases. Need CI integration tests.

## Build & Test Commands

```bash
# This repo — main workspace
make setup          # One-time: install Foundry, Go, Rust toolchain
make build          # Build all (Rust workspace + Solidity)
make test           # Run all tests
make build-solidity # Solidity only

# Solidity contracts (31 tests: 17 unit + 14 E2E with real proofs)
cd contracts/ethereum && forge build
cd contracts/ethereum && forge test --match-contract "LayerHash" -vv

# Layer-hashes prover (standalone workspace, excluded from main)
cd layer-hashes-prover
CARGO_NET_GIT_FETCH_WITH_CLI=true cargo build
# Convert binary proof+instances to gnark JSON:
cargo run --bin convert-proof -- \
    --proof <path/to/proof.bin> --instances <path/to/instances.bin> \
    --output halo2_proof.json --k 19

# Gnark wrapper for layer hashes
cd layer-hashes-prover/gnark-wrapper
go build .
./gnark-wrapper setup halo2_proof.json   # Generates Groth16Verifier.sol + keys (once)
./gnark-wrapper prove halo2_proof.json   # Generates groth16_proof.hex + groth16_output.json

# Full pipeline: keygen → prove all fixtures → gnark → Solidity
cd ../gosh-zk-snark-halo2-utils
cargo test --test test_layer_hashes_d3 -- test_layer_hashes_keygen_d3 --exact --nocapture          # ~11 min
cargo test --test test_layer_hashes_d3 -- test_layer_hashes_prove_and_verify_all_fixtures_d3 --exact --nocapture  # ~22 min
# Then convert each proof:
for f in L2_H16_prevH0_S1 L2_H32_prevH16_S1 L5_H12288_prevH1024_S11 L6_H45056_prevH0_S11; do
  cd ../acki-nacki-bridge/layer-hashes-prover
  cargo run --bin convert-proof -- \
    --proof ../../gosh-zk-snark-halo2-utils/keys/layer_hashes_all_circuit_test_data_${f}_proof.bin \
    --instances ../../gosh-zk-snark-halo2-utils/keys/layer_hashes_all_circuit_test_data_${f}_instances.bin \
    --output proofs/halo2_proof_${f}.json
done
# Gnark wrap:
cd gnark-wrapper && ./gnark-wrapper setup ../proofs/halo2_proof_L2_H32_prevH16_S1.json
for f in L2_H16_prevH0_S1 L2_H32_prevH16_S1 L5_H12288_prevH1024_S11 L6_H45056_prevH0_S11; do
  ./gnark-wrapper prove ../proofs/halo2_proof_${f}.json
  mv groth16_output.json ../proofs/groth16/groth16_output_${f}.json
done
# Update Solidity verifier:
cp Groth16Verifier.sol ../../contracts/ethereum/src/LayerHashGroth16VerifierGenerated.sol

# Partner's circuit (from ../layer-hashes-update-halo2-circuit)
cargo build --features small-window
cargo test --features small-window -- "test_prev_chain_k0_mock"  # Quick (~2 min)
cargo test --features small-window -- "test_fixture_mock_prover"  # Real data (~5 min)
```

## Test Fixtures (4 real-data fixtures from AN node)

| Fixture | num_layers | prev_chain_steps | prev_hash | block_data |
|---------|-----------|------------------|-----------|------------|
| `L2_H16_prevH0_S1` | 2 | 1 | zero (genesis) | 2012 B |
| `L2_H32_prevH16_S1` | 2 | 1 | from H16 block | 2012 B |
| `L5_H12288_prevH1024_S11` | 5 | 11 | from H1024 | 2512 B |
| `L6_H45056_prevH0_S11` | 6 | 11 | zero (genesis) | 2504 B |

All 4 proven + Groth16 wrapped + verified on Ethereum (Foundry). Proof files in `layer-hashes-prover/proofs/`.

**Public inputs per fixture** (13 Fr elements):
`[bk_set_commitment, num_layers, layer_hash[0..9], prev_max_level_layer_hash]`

## Integration Status

**Completed (M0–M6 + full fixture E2E)**:
- Audit of all partner code and dependencies
- `layer-hashes-prover/` Rust crate: exports Halo2 proof as JSON for gnark
- `layer-hashes-prover/gnark-wrapper/` Go module: Groth16 wrapper for 13 public inputs
- Gnark setup + prove pipeline verified end-to-end
- `LayerHashVerifier.sol`, `LayerHashBridge.sol`, `LayerHashGroth16VerifierGenerated.sol`
- Real keygen (PK 5.3GB, VK 11KB, ~11 min) via `gosh-zk-snark-halo2-utils`
- Real proof generation for all 4 fixtures (~22 min total)
- Groth16 wrapping for all 4 fixtures (instant)
- **31 Foundry tests**: 17 unit + 14 E2E with real proofs (~287k gas verify, ~457k gas bridge update)
- Sequential bridge update tested (L2_H16 → L2_H32 with chain anchoring)
- Negative tests: wrong commitment, layers, hash, prev_hash, corrupted proof — all rejected

**BK Set Rotation (completed: Ethereum side)**:
- `IBkSetRotationVerifier.sol`, `BkSetRotationVerifier.sol`, `BkSetRotationGroth16Verifier.sol`
- `LayerHashBridge.rotateBkSet()` — permissionless ZK-proven BK set rotation
- `LayerHashBridge.proposeBkSetCommitment()` / `executeBkSetCommitment()` / `cancelBkSetCommitment()` — 7-day timelocked emergency fallback
- `bk-set-rotation-prover/gnark-wrapper/` — Go gnark wrapper for 2 public inputs (builds, ready for circuit output)
- `bk-set-rotation-prover/CIRCUIT_SPEC.md` — full specification for the Halo2 rotation circuit
- 49 Foundry tests pass (27 unit incl. timelock + ZK rotation, 4 rotation verifier, 4 layer hash verifier, 14 E2E)

**Remaining (M7–M9)**:
- BK set rotation Halo2 circuit implementation (spec written; partner has stub `bk-set-change-verifier-halo2-circuit`)
- Live node testing (testnet not ready; `circuit-data-exporter` on `bridge_halo2_tests` branch)
- Relayer service: watch AN node → prove → wrap → submit to Ethereum
- Real `acki-nacki-interface` implementation (currently mock only)
- Production LAYER_TREE_DEPTH=8 testing

See `docs/integration_plan.md` for the full plan with milestones.
See `docs/layer_hashes_circuit_audit.md` for the complete audit report.
See `docs/integration_analysis.md` for the architecture analysis.
