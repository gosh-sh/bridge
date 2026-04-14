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
| `LayerHashBridge.sol` | **NEW** — Stores AN layer hashes, verifies ZK proofs for updates |
| `LayerHashVerifier.sol` | **NEW** — Adapter: assembles 13 public inputs, calls Groth16 verifier |
| `ILayerHashVerifier.sol` | **NEW** — Interface for layer hash verification |
| `LayerHashGroth16Verifier.sol` | **NEW** — Interface for gnark-generated 13-input Groth16 verifier |
| `LayerHashGroth16VerifierGenerated.sol` | **NEW** — Auto-generated Groth16 verifier from gnark (13 inputs) |

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
make test-rust      # Rust workspace only

# Solidity contracts
cd contracts/ethereum && forge build
cd contracts/ethereum && forge test --match-contract "LayerHash" -vv  # Layer hash tests (24 tests)

# Layer-hashes prover (standalone workspace, excluded from main)
cd layer-hashes-prover
CARGO_NET_GIT_FETCH_WITH_CLI=true cargo build  # Needs SSH for GitHub
cargo run --bin export-proof -- --fixture <path/to/fixture.json> --output halo2_proof.json

# Gnark wrapper for layer hashes
cd layer-hashes-prover/gnark-wrapper
go build .
./gnark-wrapper setup halo2_proof.json  # Generates Groth16Verifier.sol + keys
./gnark-wrapper prove halo2_proof.json  # Generates groth16_proof.hex + groth16_output.json

# Partner's circuit (from ../layer-hashes-update-halo2-circuit)
cargo build --features small-window
cargo test --features small-window -- "test_prev_chain_k0_mock"  # Quick (~2 min)
cargo test --features small-window -- "test_fixture_mock_prover"  # Real data (~5 min)
cargo test --features small-window -- "test_prev_chain_real_prover"  # Full prove (~26 min)
```

## Integration Status

**Completed (M0–M6)**:
- Audit of all partner code and dependencies
- `layer-hashes-prover/` Rust crate: exports Halo2 proof as JSON for gnark
- `layer-hashes-prover/gnark-wrapper/` Go module: Groth16 wrapper for 13 public inputs
- Gnark setup + prove pipeline verified end-to-end
- `LayerHashVerifier.sol`, `LayerHashBridge.sol`, `LayerHashGroth16VerifierGenerated.sol`
- 17 unit tests + 7 E2E tests (real proof verified on-chain, ~287k gas)
- Negative tests: wrong commitment, wrong layers, wrong hash, corrupted proof — all rejected

**Remaining (M7–M9)**:
- Live node testing (local AN node + Sepolia)
- Relayer service: watch AN node → prove → wrap → submit to Ethereum
- BK set rotation mechanism on Ethereum side
- Real `acki-nacki-interface` implementation (currently mock only)
- Production LAYER_TREE_DEPTH=8 testing

See `docs/integration_plan.md` for the full plan with milestones.
See `docs/layer_hashes_circuit_audit.md` for the complete audit report.
See `docs/integration_analysis.md` for the architecture analysis.
