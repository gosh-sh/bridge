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
├── crates/bridge-prover-orchestrator/  ← Wraps the partner's 4-circuit pipeline (Halo2 1A/1B/2[/3]) for prover/relayer use
│   └── gnark-wrappers/     ← Go modules per circuit (circuit-1a, circuit-1b, circuit-2[, circuit-3]) producing 256-byte Groth16 proofs
├── crates/bridge-relayer-daemon/       ← Phase 5.1 relayer skeleton: Relayer::tick() / run_loop() + BlockSource/BridgeClient traits + abigen!-generated AckiNackiBridge bindings + state.json persistence + CLI
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
| `AckiNackiBridge.sol` | Main bridge: deposits, withdrawals, **AAVE V3 yield integration**, AN→ETH state with `verifyBlock(finType, 1A-or-1B proof, Circuit-2 proof, …)` enforcing cross-circuit `block_id`/`bk_set_poseidon` agreement + monotonic `block_seq_no` + Poseidon chain anchor (Phase 4.1) |
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
| `IPrimaryVerifier.sol` / `PrimaryVerifier.sol` | Bridge-side adapter for Circuit 1A (Primary attestation) — 4 public inputs `[blockId, bkSetCommitment, blockSeqNo, lastSeenBlockSeqNo]` (renamed from `envelopeHash → blockId` 2026-05-10 per partner offset shift) |
| `IPrimaryGroth16Verifier.sol` / `PrimaryGroth16VerifierGenerated.sol` | Gnark-generated 4-input Groth16 verifier interface + impl |
| `IFallbackVerifier.sol` / `FallbackVerifier.sol` | Bridge-side adapter for Circuit 1B (Fallback attestation) — same 4-input shape as 1A |
| `IFallbackGroth16Verifier.sol` / `FallbackGroth16VerifierGenerated.sol` | Gnark-generated 4-input Groth16 verifier for Fallback (separate VK from 1A) |
| `ILayerHashesMovementVerifier.sol` / `LayerHashesMovementVerifier.sol` | Bridge-side adapter for Circuit 2 (Layer Hashes Movement) — 14 public inputs `[blockId, bkSetCommitment, numLayers, layerHashes[0..10], prevMaxLevelLayerHash]` |
| `ILayerHashesGroth16Verifier.sol` / `LayerHashesGroth16VerifierGenerated.sol` | Gnark-generated 14-input Groth16 verifier for Circuit 2 |
| `IAavePool.sol` | Minimal AAVE V3 Pool interface (`supply` / `withdraw` / `getReserveData`) |
| `IWrappedTokenGatewayV3.sol` | AAVE V3 ETH⇄WETH gateway interface (`depositETH` / `withdrawETH`) |
| `IERC20.sol` | Trimmed ERC-20 interface for aWETH custody |

Test mocks (under `test/mocks/`): `MockAave.sol` (`MockAWETH`, `MockAavePool`, `MockWETHGateway`) for the AAVE path without forking mainnet; `MockPrimaryVerifier.sol` / `MockFallbackVerifier.sol` / `MockLayerHashesMovementVerifier.sol` for driving `AckiNackiBridge.verifyBlock` through many synthetic blocks without re-running ZK proof generation (real verifiers covered end-to-end by `AckiNackiBridgeVerifyBlock.t.sol`).

Build: `cd contracts/ethereum && forge build`
Test: `cd contracts/ethereum && forge test`

## Rust Workspace

**Workspace members** (in `Cargo.toml`): `crates/eth-frontend`, `crates/acki-nacki-interface`
**Excluded** (separate dependency trees): `deposit-prover`, `frontend`, `poseidon-proof`, `layer-hashes-prover`, `crates/bridge-prover-orchestrator`, `crates/bridge-relayer-daemon`

- `acki-nacki-interface`: Async traits (`AckiNackiClient`, `BlockProvider`, etc.) + mock implementations. No real AN node integration yet.
- `eth-frontend`: Ethereum client using ethers-rs. Interacts with bridge contracts.
- `bridge-prover-orchestrator`: Phase 1.A/1.B prover wiring — wraps the partner's halo2 Circuit 1A/1B/2 with `KeyManager`/`generate_*_proof`/`verify_*_proof` helpers, plus `bound_test_data` for cross-circuit-bound test scenarios and `export-bound-block-proofs` binary used by Phase 4 fixtures.
- `bridge-relayer-daemon`: Phase 5.1 relayer skeleton — `Relayer::tick()`/`run_loop()` with `BlockSource` + `BridgeClient` traits (`EthBridgeClient` over `abigen!`-bindings; `MockBridgeClient`/`InMemoryBlockSource`/`FixturesBlockSource` for tests), atomic `state.json` persistence, `relayer` CLI binary. 13 unit tests cover the loop, state machine, restart-from-anchor recovery.
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

# Solidity contracts (135 tests across 15 suites, all green)
cd contracts/ethereum && forge build
cd contracts/ethereum && forge test                                                      # full suite
cd contracts/ethereum && forge test --match-contract "AckiNackiBridgeAave" -vv           # AAVE subset (23 tests)
cd contracts/ethereum && forge test --match-contract "AckiNackiBridgeVerifyBlock" -vv    # Phase 4 verifyBlock (17 tests)
cd contracts/ethereum && forge test --match-contract "AckiNackiBridgeRelayerLoop" -vv    # Phase 5.1 relayer loop (6 tests)
cd contracts/ethereum && forge test --match-contract "(Primary|Fallback|LayerHashesMovement)Verifier" -vv  # Per-circuit Groth16 adapters

# Relayer skeleton (Phase 5.1, standalone)
cd crates/bridge-relayer-daemon && cargo test                                            # 13 unit tests
cd crates/bridge-relayer-daemon && cargo run --bin relayer -- --help                     # CLI surface

# Cross-circuit-bound proof generation (Phase 4.1 fixture builder)
cd crates/bridge-prover-orchestrator
cargo run --bin export-bound-block-proofs --release        # writes proofs/bound/{primary,layer-hashes}/*
cd gnark-wrappers/circuit-1a && ./circuit-1a prove ../../proofs/bound/primary/halo2_proof.json
cd ../circuit-2                && ./circuit-2 prove ../../proofs/bound/layer-hashes/halo2_proof.json
```

## Integration Status

**Architecture v2 (4-circuit, since Phase 1.A 2026-05-06 onward)**: AN→ETH state lives directly on `AckiNackiBridge.sol` (`verifyBlock`); the legacy single-circuit pipeline (`LayerHashBridge.sol`, `LayerHashVerifier.sol`, the `bk-set-rotation-prover/` design, and the `layer-hashes-prover/` crate with its 13-input Groth16 wrapper) was retired by Phase 4.2 on 2026-05-10. Per-circuit Groth16 adapters (`PrimaryVerifier.sol`, `FallbackVerifier.sol`, `LayerHashesMovementVerifier.sol`) and the bound proof toolchain (`crates/bridge-prover-orchestrator/`) now own the surface that used to be split across the legacy crate + `LayerHashBridge`.

**Phase 4.1 (`AckiNackiBridge.verifyBlock`, completed 2026-05-10, additive)**:
- `AckiNackiBridge.sol` extended with AN→ETH state (`storedBkSetCommitment`, `storedLastSeenBlockSeqNo`, `storedNumLayers`, `storedLayerHashes[10]`, `storedPrevMaxLevelLayerHash`) + 3 immutable verifier slots + `verifyBlock(finType, attestationProof, layerHashesProof, blockId, bkSetCommitment, blockSeqNo, numLayers, layerHashes[10], prevMaxLevelLayerHash)` permissionless entry point.
- Cross-circuit + monotonicity + chain-anchor invariants enforced (`BkSetCommitmentMismatch`, `BlockSeqNoNotMonotonic`, `PrevAnchorMismatch`, `InvalidNumLayers`, `LayerHashTailNonZero`, `AttestationProofRejected`, `LayerHashesProofRejected`).
- Constructor takes a `VerifyBlockConfig` struct as 6th argument; passing `VerifyBlockConfigLib.disabled()` disables `verifyBlock` (legacy deployments).
- Cross-circuit-bound test data: `crates/bridge-prover-orchestrator/src/bound_test_data.rs` wraps the partner's `generate_bridge_test_data` so Circuit 1A's primary attestation BLS-bytes and Circuit 2's layer-hashes preimage + Merkle siblings hash to the **same** `block_id` (the 8-leaf envelope tree root) and share `bk_set_poseidon`.
- New binary `cargo run -p bridge-prover-orchestrator --bin export-bound-block-proofs --release` produces both proofs from one scenario in ~3.5 min wall time (uses cached 1A K=20 + Circuit 2 K=17 keys; gnark setup also cached).
- 17 new Foundry tests in `AckiNackiBridgeVerifyBlock.t.sol` — real bound 1A + 2 Groth16 proofs go end-to-end through both adapters → on-chain `*Groth16VerifierGenerated.sol`. Per-block verify cost: ~700 k gas (well under the 1 M target). Mock fallback path covered by `test/mocks/MockFallbackVerifier.sol`.
- 178/178 Foundry tests green at landing.

**Phase 5.1 (relayer skeleton, completed 2026-05-10)**:
- New standalone crate `crates/bridge-relayer-daemon/` (excluded from workspace, like `bridge-prover-orchestrator`). Modules: `types` (`AnBlockData`, `FinalizationType`, `MAX_LAYER_HASHES = 10`, structural validation), `bridge` (`BridgeClient` async trait + `EthBridgeClient` over `abigen!`-bindings + `MockBridgeClient` mirroring the on-chain state machine byte-for-byte for unit tests), `source` (`BlockSource` async trait + `InMemoryBlockSource` + `FixturesBlockSource` reading Phase 4.1 bound proof artefacts), `state` (atomic `state.json` persistence with write-temp-then-rename), `relayer` (`Relayer::tick()` + `Relayer::run_loop(max_ticks, should_stop)`), CLI binary `relayer` with a `smoke-fixture` subcommand.
- 13 Rust unit tests cover the loop end-to-end against `MockBridgeClient` + `InMemoryBlockSource`: 5 sequential blocks (mixed Primary/Fallback), `NotYetAvailable` recovery, verifier-rejection path, restart-from-persisted-state, `run_loop`'s `should_stop` semantics.
- 6 new Foundry tests in `AckiNackiBridgeRelayerLoop.t.sol` drive `verifyBlock` through 10 sequential synthetic blocks with the new `MockPrimaryVerifier`/`MockFallbackVerifier`/`MockLayerHashesMovementVerifier` mocks: asserts `storedLastSeenBlockSeqNo`/`storedNumLayers`/`storedLayerHashes[..]`/`storedPrevMaxLevelLayerHash` after each step, plus restart-reads-anchor, replay-reverts, fast-forward-permitted, attestation-rejection-state-untouched (CEI), anchor-mismatch-reverts negatives.
- 184/184 Foundry tests green at landing (178 baseline + 6 new). Phase 5.2 (`LiveBlockSource` over partner GraphQL/BOC + halo2+gnark inside the relayer) blocked on Q1 + Q2; Phase 5.3 (10-block shellnet acceptance) blocked on Q1.

**Phase 4.2 (legacy demolition, completed 2026-05-10)**:
- Deleted 8 legacy Solidity sources: `LayerHashBridge.sol`, `LayerHashVerifier.sol`, `LayerHashGroth16Verifier.sol`, `LayerHashGroth16VerifierGenerated.sol`, `ILayerHashVerifier.sol`, `BkSetRotationVerifier.sol`, `BkSetRotationGroth16Verifier.sol`, `IBkSetRotationVerifier.sol`. The single-circuit 13-input flow plus its old BK-rotation surface are gone — `AckiNackiBridge.verifyBlock` (Phase 4.1) is the only AN→ETH path; the future BK-set update will plug into it as an optional fourth proof argument once Phase 1.C / 3.4 land.
- Deleted 2 legacy Foundry test files: `LayerHashBridge.t.sol` (35 tests across `LayerHashBridgeTest` + `LayerHashVerifierTest` + `BkSetRotationVerifierTest`) and `LayerHashE2E.t.sol` (14 real-proof tests across 4 fixtures). Net Foundry suite: 184 → **135 tests** across 15 suites; coverage of every surviving invariant is preserved by `AckiNackiBridgeVerifyBlock.t.sol` (17), `AckiNackiBridgeRelayerLoop.t.sol` (6), `LayerHashesMovementVerifier.t.sol` (10), `PrimaryVerifier.t.sol` (8), `FallbackVerifier.t.sol` (8). Real-proof multi-fixture E2E coverage will be re-introduced by Phase 5.3 (relayer-driven, multi-block from shellnet).
- Deleted 2 legacy Rust trees: `layer-hashes-prover/` (the standalone single-circuit Halo2 → 13-input Groth16 wrapping pipeline) and `bk-set-rotation-prover/` (circuit spec + Go gnark wrapper for the old 2-input rotation design). Workspace `Cargo.toml` no longer excludes `layer-hashes-prover`.
- Lingering doc references in `docs/integration_plan.md` / `docs/manual_verification_runbook.md` / `docs/bridge_verification.md` / `docs/verifying_an_proof.md` / `docs/verifying_eth_proof_on_an.md` are intentionally left unchanged — they describe the legacy architecture that's now superseded by `docs/an_partner_integration_plan.md`. A pointer in `docs/integration_plan.md` already marks M7–M9 as superseded; the v2-aware doc rewrite belongs to Phase 7.

**AAVE V3 Yield Integration (completed)**:
- `AckiNackiBridge` extended with optional AAVE V3 wiring (pool + WETH gateway + aWETH).
- New owner role manages routing only — **cannot touch user principal**.
- `supplyToAave(amount)` routes idle ETH to AAVE; `withdrawFromAave` and `emergencyWithdrawAll` pull funds back.
- `withdraw()` auto-pulls shortfall from AAVE if liquid buffer is insufficient — transparent to users.
- Configurable `liquidReserveBps` (default 10%, capped at 50%) keeps a buffer for cheap small withdrawals.
- Yield isolated from principal: `accruedYield() = aWETH.balanceOf(bridge) - suppliedPrincipal`. `harvestYield(amount)` forwards to a separate `yieldRecipient`.
- Reentrancy guard on all mutating functions; CEI preserved in `withdraw()`.
- Mainnet addresses hardcoded in `script/DeployRealBridge.s.sol`; opt-in via `USE_AAVE=true`.
- See `docs/aave_integration.md` for design + correctness verification protocol.

**Test counts (Foundry, 15 suites, all green)**:

| Suite | Count |
|------|------|
| `AckiNackiBridgeAaveTest` (AAVE) | 23 |
| `AckiNackiBridgeV2Test` (deposit/withdraw) | 14 |
| `AckiNackiBridgeVerifyBlockTest` (Phase 4 AN→ETH, real bound 1A+2 proofs + invariants) | 17 |
| `AckiNackiBridgeRelayerLoopTest` (Phase 5.1 — 10-block loop with mock verifiers) | 6 |
| `AxiomBlockHeaderOracleTest` | 16 |
| `Blake2b/KeccakHalo2VerifierTest` | 8 |
| `Halo2PoseidonVerifierTest` | 7 |
| `FuzzAckiNackiBridgeTest` | 5 |
| `FuzzGroth16DepositVerifierTest` | 3 |
| `FuzzGroth16VerifierTest` | 4 |
| `FuzzHalo2VerifierTest` | 6 |
| `FallbackVerifierTest` (Circuit 1B, real gnark proof) | 8 |
| `PrimaryVerifierTest` (Circuit 1A, real gnark proof) | 8 |
| `LayerHashesMovementVerifierTest` (Circuit 2, real gnark proof) | 10 |
| **Total Foundry** | **135** |

**Rust tests** (excluded crates, run with `cargo test` per crate):

| Crate | Count | Notes |
|------|------|------|
| `bridge-relayer-daemon` | 13 | state persistence (2), `BlockSource` (2), `MockBridgeClient` (4), `Relayer` loop end-to-end (5) |

**Remaining (Phase 5.2/5.3, 6, 7)**:
- Phase 5.2: `LiveBlockSource` impl over partner's `gql_client` + `boc_parser` + relayer-side halo2+gnark (blocked on Q1 + Q2).
- Phase 5.3: 10-block shellnet acceptance (Anvil + live AN node) — blocked on Q1. Re-introduces real-proof multi-block coverage that retired with the legacy E2E suite in Phase 4.2.
- Phase 1.C / Phase 3.4: BK-set update circuit (partner stub `bk-set-change-verifier-halo2-circuit`); on landing, `verifyBlock` grows an optional fourth proof argument.
- Phase 6: real `acki-nacki-interface` implementation (currently mock only).
- AAVE: mainnet fork tests against the real `WrappedTokenGatewayV3` + `Pool` (currently mock-based).
- Production LAYER_TREE_DEPTH=8 testing.

### Canonical v2 doc set (Phase 7 done, 2026-05-10)

- `docs/four_circuit_architecture.md` — **canonical v2 entry point**: envelope-hash leaf table, per-circuit public-input layouts, cross-circuit binding (CC-#), state machine, gas table.
- `docs/audit_trail_v2.md` — **v1 → v2 trust delta**: 7 reduced assumptions, 12 retained, the cross-circuit soundness argument, the pre-tag sign-off checklist.
- `docs/an_partner_integration_plan.md` — **active integration plan** against the partner's four-circuit architecture (`acki-nacki-to-eth-bridge-halo2-circuits` + `acki-nacki-to-eth-bridge-halo2-prover` sibling repos).
- `docs/integration_analysis.md` — architecture analysis with §3 / §5 rewritten for v2 (deposit/AN-platform sections still original).
- `docs/bridge_verification.md` — invariant labels (DEP-#, **LH-#** v2, **BK-#** Phase 1.C placeholder, OR-#, AC-#, FORK-#, **CC-#** new in v2, ZK-#) + reproducible run recipe.
- `docs/manual_verification_runbook.md` — hands-on review (Phase D bound proof, Phase F `verifyBlock` walk, Phase G Phase-1.C placeholder, Phase J ≥ 30 attack scenarios incl. CC-1..CC-7).
- `docs/verifying_an_proof.md` — per-circuit (1A/1B/2) verification flow, V1–V5 stages.
- `docs/verifying_eth_proof_on_an.md` — deposit-side flow (unchanged in v2; cross-refs updated).
- `docs/aave_integration.md` — AAVE yield integration (orthogonal to four-circuit surface).
- `docs/layer_hashes_circuit_audit.md` — Phase 0 partner-circuit audit (still applies — chips reused by Circuit 1A/1B/2).
- `docs/legacy/verifying_an_proof_v1.md` — the retired single-circuit walkthrough, kept for reproducibility of legacy proofs.
- `docs/integration_plan.md` — historical M0–M9; M7–M9 banner-marked as superseded by `an_partner_integration_plan.md`.
