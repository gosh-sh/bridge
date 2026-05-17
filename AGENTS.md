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
├── deposit-prover/         ← Rust Halo2 circuit: proves Ethereum deposit events. Halo2 SHPLONK proof is consumed natively on the AN side (no gnark wrapper — retired in Phase 4.3 2026-05-17).
├── crates/bridge-prover-orchestrator/  ← Wraps the partner's 4-circuit pipeline (Halo2 1A/1B/2[/3]) for prover/relayer use
│   └── gnark-wrappers/     ← Go modules per circuit (circuit-1a, circuit-1b, circuit-2[, circuit-3]) producing 256-byte Groth16 proofs (AN→ETH side only; EIP-170 forces gnark wrap on this direction)
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

1. User calls `AckiNackiBridge.deposit()` on Ethereum → emits `Deposit` event.
2. `deposit-prover` (Halo2, K=20) proves the event was emitted: Receipt RLP, MPT inclusion, log matching, block hash binding.
3. Keccak coprocessor handles SHA3/keccak256 via Poseidon promise commitments (~500× savings).
4. **AN side verifies the Halo2 SHPLONK proof natively** via the proposed `ZKHALO2VERIFYWITHVK` TVM opcode (work-in-progress in `tvm-sdk`; bridge-side design memo: `docs/zk_halo2_an_side_design.md`; tvm-sdk skeleton: branch `serhii/verhalo2shplonk-skeleton`, dispatch byte `0xC7 0x4A`). Partner's parallel `ZKHALO2VERIFY` (hard-coded DarkDex VK, dispatch byte `0xC7 0x49`) lives on `tvm-sdk` branch `serhii/node-3406-vergrth16-with-vk`; our WithVK sibling adds caller-supplied VK so per-deployment bridge circuits can be verified. 7 public inputs: `[depositId, sender, amount, contractAddress, blockHashHigh, blockHashLow, promiseCommit]`. **No on-chain ETH-side verifier**: the legacy `IAckiNackiVerifier` / `Groth16{Verifier,DepositVerifier}` chain + the deposit-prover's `gnark-wrapper/` were retired in Phase 4.3 (2026-05-17) — see Decision Log.

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
4. The legacy single-circuit 13-input pipeline (`layer-hashes-update-halo2-circuit` with `gnark-wrapper/`) was retired by Phase 4.2 (2026-05-10) in favour of the four-circuit architecture below.
5. **v2 (current)**: per-circuit Halo2 + gnark Groth16 wrappers (`crates/bridge-prover-orchestrator/gnark-wrappers/circuit-{1a,1b,2}/`) — public inputs split into Circuit 1A/1B (4 inputs: `[block_id, bk_set_poseidon, block_seq_no, last_seen]`) and Circuit 2 (14 inputs: `[block_id, bk_set_poseidon, num_layers, layer_hash[0..10], prev_max_level_layer_hash]`). Consumed on-chain by `PrimaryVerifier.sol`/`FallbackVerifier.sol`/`LayerHashesMovementVerifier.sol` → `*Groth16VerifierGenerated.sol`.

## Solidity Contracts (Foundry)

| Contract | Purpose |
|----------|---------|
| `AckiNackiBridge.sol` | Main bridge: `deposit()`, **AAVE V3 yield integration**, AN→ETH state via `verifyBlock(finType, 1A-or-1B proof, Circuit-2 proof, …)` enforcing cross-circuit `block_id`/`bk_set_poseidon` agreement + monotonic `block_seq_no` + Poseidon chain anchor (Phase 4.1). AN→ETH event attestation via `verifyEvent(proof, tokenId)` + rolling `_layerWindow[100]` ring buffer (Phase A Circuit 4 scaffold, 2026-05-17 — see `docs/circuit_4_open_questions.md`). Legacy refund-style `withdraw()` + `IAckiNackiVerifier` chain retired in Phase 4.3 (2026-05-17). |
| `IBlockHeaderOracle.sol` | Interface for Ethereum block hash oracle |
| `AxiomBlockHeaderOracle.sol` | Axiom-based block hash oracle implementation |
| `Halo2Verifier.sol` | Direct Halo2 SHPLONK verifier (Yul-based, for testing) |
| `Blake2bHalo2Verifier.sol` | Blake2b-transcript Halo2 verifier |
| `Blake2bTranscript.sol` / `Blake2bChallengeComputer.sol` | On-chain Blake2b transcript replay |
| `MockBlockHeaderOracle.sol` | Mock oracle for testing |
| `IPrimaryVerifier.sol` / `PrimaryVerifier.sol` | Bridge-side adapter for Circuit 1A (Primary attestation) — 4 public inputs `[blockId, bkSetCommitment, blockSeqNo, lastSeenBlockSeqNo]` (renamed from `envelopeHash → blockId` 2026-05-10 per partner offset shift) |
| `IPrimaryGroth16Verifier.sol` / `PrimaryGroth16VerifierGenerated.sol` | Gnark-generated 4-input Groth16 verifier interface + impl |
| `IFallbackVerifier.sol` / `FallbackVerifier.sol` | Bridge-side adapter for Circuit 1B (Fallback attestation) — same 4-input shape as 1A |
| `IFallbackGroth16Verifier.sol` / `FallbackGroth16VerifierGenerated.sol` | Gnark-generated 4-input Groth16 verifier for Fallback (separate VK from 1A) |
| `ILayerHashesMovementVerifier.sol` / `LayerHashesMovementVerifier.sol` | Bridge-side adapter for Circuit 2 (Layer Hashes Movement) — 14 public inputs `[blockId, bkSetCommitment, numLayers, layerHashes[0..10], prevMaxLevelLayerHash]` |
| `ILayerHashesGroth16Verifier.sol` / `LayerHashesGroth16VerifierGenerated.sol` | Gnark-generated 14-input Groth16 verifier for Circuit 2 |
| `IBridgeEventVerifier.sol` / `BridgeEventVerifier.sol` | Bridge-side adapter for Circuit 4 (Bridge Event Prove, Phase A scaffold) — 103 public inputs `[tokenId, dappFr, accFr, layerHashes[0..100]]`. Mock-tested only; real `BridgeEventGroth16VerifierGenerated.sol` pending Phase B (see `docs/circuit_4_open_questions.md`). |
| `IBridgeEventGroth16Verifier.sol` | Interface for the future gnark-generated 103-input Groth16 verifier (Circuit 4) |
| `IAavePool.sol` | Minimal AAVE V3 Pool interface (`supply` / `withdraw` / `getReserveData`) |
| `IWrappedTokenGatewayV3.sol` | AAVE V3 ETH⇄WETH gateway interface (`depositETH` / `withdrawETH`) |
| `IERC20.sol` | Trimmed ERC-20 interface for aWETH custody |

Test mocks (under `test/mocks/`): `MockAave.sol` (`MockAWETH`, `MockAavePool`, `MockWETHGateway`) for the AAVE path without forking mainnet; `MockPrimaryVerifier.sol` / `MockFallbackVerifier.sol` / `MockLayerHashesMovementVerifier.sol` for driving `AckiNackiBridge.verifyBlock` through many synthetic blocks without re-running ZK proof generation (real verifiers covered end-to-end by `AckiNackiBridgeVerifyBlock.t.sol`); `MockBridgeEventVerifier.sol` with optional **strict mode** (pin expected layerHashes window + identity triple) for Phase A Circuit 4 tests.

Build: `cd contracts/ethereum && forge build`
Test: `cd contracts/ethereum && forge test`

## Rust Workspace

**Workspace members** (in `Cargo.toml`): `crates/eth-frontend`, `crates/acki-nacki-interface`
**Excluded** (separate dependency trees): `deposit-prover`, `frontend`, `poseidon-proof`, `layer-hashes-prover`, `crates/bridge-prover-orchestrator`, `crates/bridge-relayer-daemon`

- `acki-nacki-interface`: Async traits (`IAckiNacki`, `TransactionSender`) + mock implementations, plus a **live REST client** `BkSetClient` against the AN node's `/v2/bk_set` and `/v2/bk_set_update` endpoints (probed working against `http://94.156.178.19:8600` on 2026-05-18). Returns typed `BkSetResponse` / `BkSetUpdateResponse` and a `signer_index → 48-byte BLS pubkey` map ready for `bridge-prover-orchestrator::generate_fallback_proof`. Live tests are `#[ignore]`-gated (`cargo test -p acki-nacki-interface --test live_bk_set -- --ignored`).
- `eth-frontend`: Ethereum client using alloy-rs (migrated 2026-05-17 from ethers-rs). Interacts with bridge contracts.
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

## Partner Prover Bug Found (2026-05-10, GOSH stand)

While running Alina's two-circuit live-stand experiment (`acki-nacki/latest_an_to_eth_bridge_test_lightweight` + `acki-nacki-to-eth-bridge-halo2-prover/test_both_circuits_lightweight`) on the GOSH server, Circuit 1a verified consistently but Circuit 2 was rejected for every live key block (`primary=true, layer=false`).

**Root cause**: `bridge-prover-lib/src/keys.rs::ensure_layer_keys` builds the reference circuit used for VK/PK keygen with tree depth 3:

```rust
let chain_data = bridge_test_data_gen::layer_hashes::generate_layer_hash_chain_with_depth(3, 2, 3);
```

The accompanying comment said `WINDOW_SIZE=4 → 6 leaves → pad to 8 → depth=3`, which was correct for the now-superseded `HISTORY_PROOF_WINDOW_SIZE=4`. The lightweight branch uses `HISTORY_PROOF_WINDOW_SIZE=8`, so real layer trees have `2 + 8 = 10` leaves padded to 16, **depth 4**. As a result, every `DenseChainLink` produced by `real_chain_builder` carries `siblings.len() == 4`, while the VK/PK were committed for `siblings.len() == 3`. The witness still satisfies the constraint system (MockProver passes), but `halo2::verify_proof` rejects every proof because the polynomial shape no longer matches the VK.

**Diagnostic isolation** (in `bridge-prover-lib/tests/`):
- `diagnostic_l1_block16.rs` — rebuilds the layer-1 Poseidon tree for live block 16 from blocks 8..15 leaves and confirms the prover's tree reconstruction matches the node's `history_proofs[1].root_hash` byte-for-byte (`fb3461c9...d175252c`). Tree math is fine.
- `diagnostic_mockprover_block16.rs` — runs `MockProver` with the EXACT witness the live prover constructs (real preimage, real siblings, real chain). All constraints satisfied.
- `diagnostic_chain_depth.rs` — proves the discrepancy: real chain links have `siblings.len()=4`, keygen reference has `siblings.len()=3`.

**Fix**: change the keygen reference depth to 4 (and update the stale comment):

```diff
- // Tree depth must match real trees: WINDOW_SIZE=4 → 6 leaves → pad to 8 → depth=3.
- let chain_data = bridge_test_data_gen::layer_hashes::generate_layer_hash_chain_with_depth(3, 2, 3);
+ // Tree depth must match real trees: WINDOW_SIZE=8 → 10 leaves → pad to 16 → depth=4.
+ let chain_data = bridge_test_data_gen::layer_hashes::generate_layer_hash_chain_with_depth(3, 2, 4);
```

**Verification (live)**: with the patch, after deleting cached `params/layer_*.bin` + `params/layer_config_params.json` and `state/*.json`, the prover regenerated keys (`base_circuit_params: k=17, num_advice_per_phase=[18], lookup_bits=Some(16)`) and successfully proved + verified Circuit 2 for blocks 16 and 24:

```
block 16: Circuit 1a VERIFIED (11.3ms), Circuit 2 VERIFIED (9.1ms)  → BOTH VERIFIED OK
block 24: Circuit 1a VERIFIED (17.5ms), Circuit 2 VERIFIED (11.0ms) → BOTH VERIFIED OK
```

Three diagnostic tests are checked into the prover repo so this regression is catchable in CI without needing a live node only for the depth check (`diagnostic_chain_depth` is the smallest reproducer).

**Long-term remediation suggestion (for the partner)**: derive the keygen depth from `HISTORY_WINDOW_SIZE` rather than hardcoding it, so the reference circuit shape automatically tracks the runtime constant.

### Resolution upstream (2026-05-11, Alina)

Alina confirmed the diagnosis and pushed the long-term remediation across all three repos. The actual story turned out to be a *duplicate-constant* drift, not a Window-= 8 production setting: `HISTORY_PROOF_WINDOW_SIZE` was defined in two places inside `acki-nacki` (`node/src/types/history_proof.rs` = 8, `node/libs/node-block-client/src/history_proof.rs` = 4). Her local node had been built from a cached Docker image where `node/src/types/history_proof.rs` = 8 while the prover daemon used the lightweight crate's `= 4` definition — masking the desync for several full E2E runs.

The chain of three coordinated commits:

- `acki-nacki@8c54dd7c` — single source of truth lives in `node/libs/node-block-client/src/history_proof.rs` (value reverted to `= 4` for fast E2E coverage); `node/src/types/history_proof.rs` now does `pub use node_block_client::history_proof::HISTORY_PROOF_WINDOW_SIZE`. Old `8` and production `128` preserved as comments.
- `acki-nacki-to-eth-bridge-halo2-prover@4cdc932 + c8495f9 + 24dffa1` — `bridge-prover-daemon` no longer carries its own `HISTORY_WINDOW_SIZE = …` literal; it imports `node_block_client::history_proof::HISTORY_PROOF_WINDOW_SIZE` at compile time. `ensure_layer_keys` computes `REF_TREE_DEPTH = (W + 2).next_power_of_two().trailing_zeros() as usize` — exactly the recommendation above. For the current `W = 4` this is depth `3`, bit-identical to the pre-fix literal, so cached VK/PK survive the patch with no keygen rerun.
- `acki-nacki-to-eth-bridge-halo2-circuits@55a22b9` — `historical-layer-hashes-movement-checker-circuit/tests/real_prover.rs` aligned to `TREE_DEPTH = 3`, README table now lists all three rows (W=4 → depth 3, W=8 → 4, W=128 → 8), `.gitignore` covers `test_cache_real_prover/`.

She independently re-verified end-to-end on her lightweight stand: 16/16 key blocks (heights 8..68) BOTH VERIFIED OK at `~3:20`/block steady state. Her note for future testing: stay on `W = 4` to maximise configuration coverage per unit time; bump to `8` only for the mid-size sanity sweep, and to `128` for production. The class of bug ("daemon literal drifts from node constant") is now eliminated at the type-system level — the prover crate physically depends on `node-block-client`, and the node crate physically re-exports the same constant.

### Independent re-verification on our stand (2026-05-11)

To close the loop we rebuilt the lightweight stand from scratch against Alina's unified `W = 4` ribosome (no Docker cache for `ackinacki-bridge-lite:latest`, fresh `params/layer_*.bin`, empty `state/`) and ran the prover+verifier pair against it. Six consecutive key blocks from the freshly bootstrapped chain all came through clean:

```
block  8: Circuit 1a VERIFIED (9.8 ms),  Circuit 2 VERIFIED (10.1 ms)  → BOTH VERIFIED OK  (3:32 e2e)
block 12: Circuit 1a VERIFIED (11.3 ms), Circuit 2 VERIFIED (9.7 ms)   → BOTH VERIFIED OK  (3:27 e2e)
block 16: Circuit 1a VERIFIED (8.8 ms),  Circuit 2 VERIFIED (10.0 ms)  → BOTH VERIFIED OK  (3:23 e2e)
block 20: Circuit 1a VERIFIED (15.2 ms), Circuit 2 VERIFIED (12.4 ms)  → BOTH VERIFIED OK  (3:28 e2e)
block 24: Circuit 1a VERIFIED (9.7 ms),  Circuit 2 VERIFIED (10.6 ms)  → BOTH VERIFIED OK  (3:25 e2e)
block 28: Circuit 1a VERIFIED (13.0 ms), Circuit 2 VERIFIED (8.1 ms)   → BOTH VERIFIED OK  (3:24 e2e)
```

Steady-state ~3:25/block end-to-end (prove+verify), matching Alina's reported ~3:20/block within noise. Halo2 verify time stays at ~10 ms/circuit. The class of bug is empirically closed on our stand too.

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
cd contracts/ethereum && forge test --match-contract "AckiNackiBridgeAaveTest" -vv       # AAVE mock subset (20 tests)

# AAVE V3 mainnet-fork tests (opt-in, requires RPC + `fork` profile for evm_version=shanghai
# because AAVE V3 aWETH bytecode uses PUSH0). Without FORK_URL the suite skips cleanly.
FOUNDRY_PROFILE=fork FORK_URL=https://ethereum-rpc.publicnode.com \
    forge test --match-contract AckiNackiBridgeAaveForkTest -vv         # 4 fork tests against real V3 Pool/Gateway/aWETH
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
- Lingering doc references in `docs/integration_plan.md` / `docs/verifying_an_proof.md` are intentionally left unchanged — they describe the legacy architecture that's now superseded by `docs/an_partner_integration_plan.md`. A pointer in `docs/integration_plan.md` already marks M7–M9 as superseded.

**Phase 4.3 — Legacy deposit-verifier demolition (2026-05-17)**:
- Removed the legacy v1 ETH-side deposit-verifier chain: deleted `IAckiNackiVerifier.sol`, `Groth16Verifier.sol` (gnark-generated for deposit-prover), `Groth16DepositVerifier.sol`, `DummyVerifier.sol`, `AckiNackiBridgeV2.t.sol`, the `FuzzGroth16{Verifier,DepositVerifier}Test` contracts, and the withdraw-flow fuzz tests.
- Surgically removed from `AckiNackiBridge.sol`: `verifier` storage slot, `processedDeposits` mapping, `event Withdrawal`, four errors (`InvalidProof`, `DepositAlreadyProcessed`, `InvalidVerifier`, `InvalidBlockHash`), the `withdraw(...)` function, the `isDepositProcessed(...)` view, and the `_verifier` ctor parameter. Ctor is now 5-args (was 6).
- Deleted the deposit-prover gnark-wrapper toolchain: `deposit-prover/gnark-wrapper/` (Go module + R1CS + keys + generated `Groth16Verifier.sol`), `deposit-prover/src/groth16_wrapper/` (Rust JSON adapter), and 11 deposit-prover/top-level shell scripts that wired the Sepolia E2E.
- Trimmed `crates/eth-frontend/src/contract.rs` ABI to deposit + read-only views; deleted `crates/eth-frontend/src/withdrawal.rs` stub.
- Net Foundry suite: 135 → **109 tests across 12 suites** (−26 tests, −3 suites); `forge build` clean, `forge test` all green; `cargo check --workspace --all-targets` clean.
- Rationale: AN-side will verify the deposit-prover's Halo2 SHPLONK proof natively via the future `VERHALO2SHPLONK` TVM opcode (no EIP-170 on AN side, so the gnark wrapper is unnecessary). The AN→ETH gnark wrappers under `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-{1a,1b,2}/` are **not affected** — EIP-170 still forces a wrapper on that side. See Decision Log 2026-05-17 in `docs/an_partner_integration_plan.md`.

**AAVE V3 Yield Integration (completed)**:
- `AckiNackiBridge` extended with optional AAVE V3 wiring (pool + WETH gateway + aWETH).
- New owner role manages routing only — **cannot touch user principal**.
- `supplyToAave(amount)` routes idle ETH to AAVE; `withdrawFromAave` and `emergencyWithdrawAll` pull funds back (owner-only).
- Configurable `liquidReserveBps` (default 10%, capped at 50%) governs how much of treasury stays liquid as ETH.
- Yield isolated from principal: `accruedYield() = aWETH.balanceOf(bridge) - suppliedPrincipal`. `harvestYield(amount)` forwards to a separate `yieldRecipient`.
- Reentrancy guard on all mutating functions; CEI preserved throughout.
- Mainnet addresses hardcoded in `script/DeployRealBridge.s.sol`; opt-in via `USE_AAVE=true`.
- See `docs/aave_integration.md` for design + correctness verification protocol. (Note: doc may still mention the legacy user-facing `withdraw()` auto-pull behaviour, which was retired in Phase 4.3 along with the rest of the legacy withdraw flow; the owner-only `withdrawFromAave` / `emergencyWithdrawAll` paths are unaffected.)

**Test counts (Foundry, 13 suites, all green)**:

| Suite | Count |
|------|------|
| `AckiNackiBridgeAaveTest` (AAVE; owner-only top-up + yield) | 20 |
| `AckiNackiBridgeVerifyBlockTest` (Phase 4 AN→ETH, real bound 1A+2 proofs + invariants) | 17 |
| `AckiNackiBridgeRelayerLoopTest` (Phase 5.1 — 10-block loop with mock verifiers) | 6 |
| `AckiNackiBridgeVerifyEventTest` (Phase A Circuit 4 scaffolding — layerWindow + verifyEvent) | 16 |
| `AxiomBlockHeaderOracleTest` | 16 |
| `Blake2bHalo2VerifierTest` | 7 |
| `KeccakHalo2VerifierTest` | 1 |
| `Halo2PoseidonVerifierTest` | 7 |
| `FuzzAckiNackiBridgeDepositTest` (deposit fuzz only) | 3 |
| `FuzzHalo2VerifierTest` | 6 |
| `FallbackVerifierTest` (Circuit 1B, real gnark proof) | 8 |
| `PrimaryVerifierTest` (Circuit 1A, real gnark proof) | 8 |
| `LayerHashesMovementVerifierTest` (Circuit 2, real gnark proof) | 10 |
| **Total Foundry** | **125** |

**Rust tests** (excluded crates, run with `cargo test` per crate):

| Crate | Count | Notes |
|------|------|------|
| `bridge-relayer-daemon` | 13 | state persistence (2), `BlockSource` (2), `MockBridgeClient` (4), `Relayer` loop end-to-end (5) |

**Remaining (Phase 5.2/5.3, 6, 7)**:
- Phase 5.2: `LiveBlockSource` impl over partner's `gql_client` + `boc_parser` + relayer-side halo2+gnark (blocked on Q1 + Q2).
- Phase 5.3: 10-block shellnet acceptance (Anvil + live AN node) — blocked on Q1. Re-introduces real-proof multi-block coverage that retired with the legacy E2E suite in Phase 4.2.
- Phase 1.C / Phase 3.4: BK-set update circuit (partner stub `bk-set-change-verifier-halo2-circuit`); on landing, `verifyBlock` grows an optional fourth proof argument.
- Phase 6: real `acki-nacki-interface` implementation (currently mock only).
- AAVE: mainnet fork tests landed 2026-05-17 in `AckiNackiBridgeAaveFork.t.sol` (4 tests: constructor wiring, supply+withdraw round-trip, 1-year yield accrual + harvest, emergency exit). Opt-in via `FOUNDRY_PROFILE=fork FORK_URL=...`. Default `forge test` skips them. Validates real V3 `Pool` + `WrappedTokenGatewayV3` + `aWETH` ABI assumptions against the mock-based `AckiNackiBridgeAaveTest`.
- Production LAYER_TREE_DEPTH=8 testing.

### Canonical v2 doc set (Phase 7 done, 2026-05-10)

- `docs/four_circuit_architecture.md` — **canonical v2 entry point**: envelope-hash leaf table, per-circuit public-input layouts, cross-circuit binding (CC-#), state machine, gas table.
- `docs/audit_trail_v2.md` — **v1 → v2 trust delta**: 7 reduced assumptions, 12 retained, the cross-circuit soundness argument, the pre-tag sign-off checklist.
- `docs/an_partner_integration_plan.md` — **active integration plan** against the partner's four-circuit architecture (`acki-nacki-to-eth-bridge-halo2-circuits` + `acki-nacki-to-eth-bridge-halo2-prover` sibling repos).
- `docs/integration_analysis.md` — architecture analysis with §3 / §5 rewritten for v2 (deposit/AN-platform sections still original).
- `docs/bridge_verification.md` — invariant labels (DEP-#, **LH-#** v2, **BK-#** Phase 1.C placeholder, OR-#, AC-#, FORK-#, **CC-#** new in v2, ZK-#) + reproducible run recipe.
- `docs/manual_verification_runbook.md` — hands-on review (Phase D bound proof, Phase F `verifyBlock` walk, Phase G Phase-1.C placeholder, Phase J ≥ 30 attack scenarios incl. CC-1..CC-7).
- `docs/verifying_an_proof.md` — per-circuit (1A/1B/2) verification flow, V1–V5 stages.
- `docs/verifying_eth_proof_on_an.md` — deposit-side flow. **Rewritten 2026-05-17 after Phase 4.3 demolition**: the ETH-side Groth16 path was retired and the verification is now described as a pure AN-side native Halo2 SHPLONK check via the proposed `ZKHALO2VERIFYWITHVK` TVM opcode.
- `docs/zk_halo2_an_side_design.md` — design memo for the AN-side `ZKHALO2VERIFYWITHVK` opcode: gap analysis vs. partner's `ZKHALO2VERIFY` (hard-coded DarkDex VK), proposed stack ABI / gas model / per-VK cache, five open Q-WIRE-# questions, six-phase roadmap. Companion skeleton lives in `tvm-sdk` branch `serhii/verhalo2shplonk-skeleton`. New 2026-05-17.
- `docs/circuit_4_open_questions.md` — Phase A vs Phase B split for Circuit 4 (`bridge-event-prove-circuit`). Phase A landed in this commit (`AckiNackiBridge.verifyEvent`, `_layerWindow`, `BridgeEventVerifier`, 16 mock-based Foundry tests, gnark wrapper skeleton at `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-4/`). Phase B (real `withdraw()`) gated on five circuit-side design questions Q-CIRC4-1..5. New 2026-05-17.
- `docs/aave_integration.md` — AAVE yield integration (orthogonal to four-circuit surface).
- `docs/layer_hashes_circuit_audit.md` — Phase 0 partner-circuit audit (still applies — chips reused by Circuit 1A/1B/2).
- `docs/legacy/verifying_an_proof_v1.md` — the retired single-circuit walkthrough, kept for reproducibility of legacy proofs.
- `docs/integration_plan.md` — historical M0–M9; M7–M9 banner-marked as superseded by `an_partner_integration_plan.md`.
