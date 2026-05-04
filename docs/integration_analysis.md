# Acki Nacki Bridge — Integration Analysis

This document provides a comprehensive analysis of both sides of the Acki Nacki cross-chain bridge: our Ethereum-side implementation and the partner's Acki Nacki-side ZK circuits.

## Table of Contents

1. [Our Side: Ethereum Bridge](#1-our-side-ethereum-bridge)
2. [Acki Nacki Platform](#2-acki-nacki-platform)
3. [Partner's Work: Layer Hashes Circuit](#3-partners-work-layer-hashes-circuit)
4. [Partner's Work: ZK SNARK Halo2 Utils](#4-partners-work-zk-snark-halo2-utils)
5. [Gap Analysis](#5-gap-analysis)

---

## 1. Our Side: Ethereum Bridge

### 1.1 Architecture Overview

The `acki-nacki-bridge` repository implements the **Ethereum side** of a cross-chain bridge. It consists of:

- **Solidity smart contracts** (Foundry) — bridge vault, ZK verifiers, block hash oracles
- **deposit-prover** (Rust + axiom-eth) — Halo2 circuit proving Ethereum deposit events
- **gnark-wrapper** (Go) — wraps Halo2 SNARK proofs in Groth16 for efficient on-chain verification
- **poseidon-proof** (Rust) — Halo2 circuit with Blake2b transcript for Acki Nacki → Ethereum proofs
- **acki-nacki-interface** (Rust) — trait definitions for Acki Nacki blockchain interaction (mock-only)

### 1.2 Smart Contracts

All contracts are in `contracts/ethereum/src/`, compiled with Solidity 0.8.19 via Foundry.

#### AckiNackiBridge.sol

Main bridge contract with two user-facing entry points plus an optional AAVE V3 yield path:

- **`deposit()`** — accepts ETH (max 100 ETH), increments `depositCounter`, emits `Deposit(depositId, sender, amount, timestamp)`. Funds accumulate in the contract; `deposit()` itself never calls AAVE (kept cheap).
- **`withdraw(recipient, amount, depositId, blockNumber, proof)`** — queries block hash from oracle, builds 6 public inputs, calls verifier, transfers ETH on success. If the bridge's liquid ETH balance is short of `amount`, the shortfall is transparently pulled from AAVE.

Constructor (5 args): `(verifier, blockHeaderOracle, aavePool, wethGateway, aWETH)`. Pass `address(0)` for the last three to disable AAVE; this keeps the bridge in plain-ETH custody mode.

Storage:
- Core: `processedDeposits` (double-spend prevention), `depositCounter`, `treasuryBalance`, pluggable `verifier`, `blockHeaderOracle`.
- AAVE: immutable `aavePool` / `wethGateway` / `aWETH`; `aaveEnabled`, `suppliedPrincipal` (book value of supplied ETH), `liquidReserveBps` (default 1000 = 10 %, capped at 5000).
- Access: `owner` (administers AAVE routing), `yieldRecipient` (gets harvested yield).

Public input layout assembled by the bridge for verification:

| Index | Field | Description |
|-------|-------|-------------|
| 0 | depositId | Unique deposit identifier |
| 1 | sender | Recipient address as uint256 |
| 2 | amount | Deposit amount in wei |
| 3 | contractAddress | Bridge contract address |
| 4 | blockHashHigh | Upper 128 bits of block hash |
| 5 | blockHashLow | Lower 128 bits of block hash |

User-facing entry points (`deposit`, `withdraw`) are permissionless — anyone with a valid ZK proof can withdraw. The `owner` role only governs AAVE routing (`supplyToAave`, `withdrawFromAave`, `emergencyWithdrawAll`, `setAaveEnabled`, `setLiquidReserveBps`, `harvestYield`, `setYieldRecipient`, `transferOwnership`). The owner **cannot** withdraw user principal: `harvestYield` is bounded by `aWETH.balanceOf(bridge) - suppliedPrincipal`, and there is no admin path that bypasses the ZK-verified `withdraw()`.

See `docs/aave_integration.md` for the full design, invariants, and verification protocol.

#### IAckiNackiVerifier.sol

Verifier interface:
- `verifyWithdrawalProof(bytes proof, uint256[] publicInputs) → (bool isValid, bytes32 depositId)`
- `getPublicInputsCount() → uint256`

#### Groth16DepositVerifier.sol

Production verifier adapting the bridge's 6 public inputs to the circuit's 7 inputs:

- Validates proof length = 288 bytes (256 Groth16 + 32 `promise_commit`)
- Decodes 8 × uint256 Groth16 proof points
- Extracts `promise_commit` from the last 32 bytes
- Assembles 7 circuit inputs: bridge's 6 + `promise_commit`
- Calls gnark-generated `Groth16Verifier.verifyProof(uint256[8], uint256[7])`

#### IBlockHeaderOracle.sol

Oracle interface for trusted block hash sources:
- `getBlockHash(blockNumber)`, `isBlockHashAvailable(blockNumber)`, `getLatestVerifiedBlock()`
- Implementations: `MockBlockHeaderOracle` (testing), `AxiomBlockHeaderOracle` (production via Axiom V2)

#### Blake2b Verification Path

For Acki Nacki → Ethereum proofs:
- `Blake2bHalo2Verifier.sol` — Halo2 verifier using EIP-152 Blake2b precompile
- `Blake2bTranscript.sol` — Fiat-Shamir transcript matching `halo2_proofs::Blake2bWrite`
- `Blake2bChallengeComputer.sol` — challenge computation (split out for 24KB limit)

### 1.3 ZK Proof Pipeline (Deposits)

```
Ethereum deposit event
    → deposit-prover (Rust/axiom-eth): Halo2 circuit
        - Receipt trie inclusion (MPT proof)
        - RLP decoding, event log extraction
        - Event signature verification (keccak256)
        - Contract address binding
        - Block hash binding
        → 7 public inputs: [depositId, sender, amount, contractAddress,
                            blockHashHigh, blockHashLow, promiseCommit]

    → gnark-wrapper (Go): Halo2 → Groth16
        - Loads halo2_proof.json (proof bytes + protocol metadata)
        - Wraps in Groth16 on BN254
        → 288 bytes: 256-byte Groth16 proof + 32-byte promise_commit

    → On-chain: Groth16DepositVerifier → Groth16Verifier
        - Pairing check on BN254 (~280k gas)
```

The keccak coprocessor pattern (documented in `docs/keccak_coprocessor_flowchart.mmd`) delegates expensive keccak256 computations to a separate circuit via Poseidon-based promise commitments, achieving ~500x constraint savings per hash.

### 1.4 Integration Surfaces

**`crates/acki-nacki-interface`**: Defines `IAckiNacki` and `TransactionSender` traits for Acki Nacki blockchain interaction. Currently **mock-only** — the actual implementation is expected from the Acki Nacki team. Types include `AckiNackiTransaction`, `TransactionReceipt`, `TransactionStatus`, `Log`.

**`crates/eth-frontend`**: `DepositManager` and `WithdrawalManager` are **empty stubs**. The `contract.rs` abigen does not fully match the latest `AckiNackiBridge.sol` signature.

**gnark-wrapper**: Currently hardcoded for the deposit circuit (7 public inputs, specific witness commitment counts). Adapting it for a different circuit requires updating the JSON format, circuit struct sizes, and proof parser.

### 1.5 Workspace Layout

Three separate Cargo workspaces due to dependency incompatibilities:

| Workspace | Crates | Halo2 Stack |
|-----------|--------|-------------|
| Root `Cargo.toml` | `eth-frontend`, `acki-nacki-interface` | None |
| `deposit-prover/` | `deposit-prover` | axiom-eth + halo2-pse v2023_04_20 |
| `poseidon-proof/` | `poseidon-proof` | halo2-axiom 0.5.x |

---

## 2. Acki Nacki Platform

### 2.1 Overview

Acki Nacki is a **TVM-based multi-threaded blockchain** with the following topology:

- **Block Keepers (BK)** — consensus nodes that produce and attest blocks using BLS12-381 signatures
- **Block Managers (BM)** — archive/API nodes that consume blocks and serve REST/GraphQL
- **Broadcast Proxies** — optional relay nodes to reduce cross-provider traffic
- **GraphQL Server** — read-only query interface over BM's SQLite archives

### 2.2 Block Structure

An `AckiNackiBlock` wraps a TVM `Block` plus a `CommonSection` containing:

- Block height, round, producer ID, thread ID
- **`block_attestations`** — BLS aggregated signature envelopes
- **`block_keeper_set_changes`** — epoch BK set transitions
- **`refs`** — cross-thread references and migration data
- **`history_proofs`** — `BTreeMap<LayerNumber, ProofLayerRootHash>` (feature-gated)

Block sequence numbers (`BlockSeqNo`) are u32, matching TVM block `seq_no`.

### 2.3 Consensus: BLS Attestations and Epochs

**Attestation structure** (`AttestationData`):
- `parent_block_id`, `block_id`, `block_seq_no`
- `envelope_hash` — SHA-256 hash of the serialized block envelope
- `target_type` — `Primary` or `Fallback`

Attestations use `Envelope<AttestationData>` with **`GoshBLS`**: an aggregated BLS12-381 signature plus `signature_occurrences` mapping (signer index → count) for multi-signer aggregation.

**Finalization**: quorum rules combine primary and fallback attestation targets. Primary finalization requires ~66% of the BK set. Blocks carry aggregated attestation vectors in their common section.

**Epochs**: tied to on-chain staking (BK/BM wallets, continue-stake timing). They drive BLS key rotation and BK set membership. The `/v2/bk_set_update` HTTP endpoint provides full BK set snapshots.

### 2.4 History Proofs / Layer Hashes

Enabled via the `history_proofs` Cargo feature. Key parameters:

- **`HISTORY_PROOF_WINDOW_SIZE = 128`** (production); configurable to 2 or 4 for testing
- **`ProofLayerRootHash`**: layer index (u8), Poseidon monotree root, block height, block ID
- Stored as `BTreeMap<LayerNumber, ProofLayerRootHash>` in the block's common section
- Root computation uses **Poseidon hasher** (T=3, R_F=8, R_P=57 on BN254)
- Layers form a hierarchical structure: layer N aggregates entries from layer N-1 in balanced Merkle trees

The layer hash system provides a compact, ZK-provable chain of block commitments that a bridge contract can track.

### 2.5 Available APIs

| API | Endpoint | Purpose |
|-----|----------|---------|
| BK HTTP | `/v2/bk_set`, `/v2/bk_set_update` | BK set state |
| BK HTTP | `/v2/account`, `/v2/messages` | Account queries, external messages |
| BM REST | `/v2/account`, `/v2/messages`, `/v2/readiness` | Archive queries |
| BM Stream | QUIC `:12000` | Block streaming |
| GraphQL | `/graphql` | Read-only queries: blocks, transactions, attestations, BK set updates |

### 2.6 TVM SDK

`tvm-sdk` (workspace version 2.24.16) provides:
- `tvm_client` — messages, ABI encoding, BOC parsing, GraphQL queries
- `tvm_block` — block/transaction data types
- `tvm_vm` — TVM implementation
- `tvm_executor` — contract execution

No built-in cross-chain API — bridge logic lives at the contract level plus off-chain relayers.

---

## 3. Partner's Work: Layer Hashes Circuit

### 3.1 Repository: `layer-hashes-update-halo2-circuit`

A Rust workspace implementing a Halo2 circuit (`LayerHashesUpdateCircuit`) that proves facts about a single finalized Acki Nacki block, enabling trustless layer hash updates on a bridge contract.

### 3.2 What the Circuit Proves

The circuit (`primary_circuit.rs`, function `build_primary_constraints`) enforces six constraint groups:

**A. Block Attestation (SHA-256 binding)**
- Computes SHA-256 of `block_data` (up to 4096 bytes, padded to 65 × 64 compression blocks)
- Constrains the hash output to equal the `envelope_hash` field within the attestation data
- Uses `block_selector` (witness, 7-bit range-checked) to select the correct SHA state after the actual message length

**B. Target Type Enforcement**
- Four bytes at `TARGET_TYPE_REL_OFFSET` (offset 116 within `AttestationData`) constrained to zero
- This enforces the attestation is for a **Primary** block (bincode u32 LE discriminant = 0)

**C. Layer Count Validation**
- `num_layers` is a witness, range-checked to `[1, MAX_LAYERS]` (i.e., 1..10)
- Enforced via three 4-bit range checks: `num_layers`, `num_layers - 1`, `MAX_LAYERS - num_layers`

**D. Layer Hash Extraction from Block Data**
- Extracts 10 × 32-byte root hashes from `block_data` at bincode-predictable offsets
- Uses `base_offset_cell` (12-bit range) as the start of the `history_proofs` BTreeMap
- BTreeMap entry stride: `BTREE_ENTRY_SIZE = 124` bytes per layer
- First root hash offset: `FIRST_ROOT_HASH_OFFSET = 10` (8-byte BTree header + 1-byte key + 1-byte root_hash_offset)
- For each layer i, byte j: rotates `block_data_cells` by constant stride, uses inner product with one-hot `offset_indicator` (length 4096) to extract the byte
- Packs 32 bytes to Fr via inner product with powers of 256
- Inactive layers (i >= num_layers) are masked to zero via `active_mask`

**E. Prev-Chain Merkle Verification**
- Verifies a Poseidon Merkle chain from `prev_max_level_layer_hash` to `layer_hash_frs[num_layers - 1]`
- `num_prev_chain_steps` range-checked to `[1, MAX_CHAIN_LEN]`
- Uses `verify_chain_of_dense_proofs` from `gosh-dense-balanced-tree`
- Poseidon parameters: T=3, RATE=2, R_F=8, R_P=57

**F. BLS Signature Verification**
- Hash-to-curve: `ExpandMsgXmd` on full attestation message bytes
- Aggregate BLS12-381 signature verification with Primary threshold
- Threshold mode enforces >= 2n/3 signers from the BK set
- Cross-field arithmetic: BLS12-381 operations emulated over BN254 Fr (LIMB_BITS=104, NUM_LIMBS=5)

**G. BK Set Commitment**
- Poseidon hash over sorted (signer_index, x-coordinate CRT limbs) for each G1 pubkey
- Only x-coordinate is committed (y is derivable from on-curve constraint in BLS gadget)

### 3.3 Public Inputs (13 Field Elements)

| Index | Field | Description |
|-------|-------|-------------|
| 0 | `bk_set_commitment` | Poseidon commitment over sorted BK set |
| 1 | `num_layers` | Number of active layers (1..10) |
| 2..11 | `layer_hash_frs[0..10]` | Layer root hashes (inactive = 0) |
| 12 | `prev_max_level_layer_hash` | Previous top-level hash (chain anchor) |

### 3.4 Circuit Parameters

| Parameter | Value | Notes |
|-----------|-------|-------|
| K | 19 | Circuit degree (2^19 rows) |
| LOOKUP_BITS | 18 | Range check table size |
| LIMB_BITS | 104 | Non-native field limb size |
| NUM_LIMBS | 5 | Limbs per BLS12-381 field element |
| MAX_SIGNERS | 300 | Maximum BK set size |
| MAX_BLOCK_DATA_BYTES | 4096 | Maximum block data for SHA-256 |
| MAX_LAYERS | 10 | Maximum history proof layers |
| LAYER_TREE_DEPTH | 8 (prod) / 2 (test) | Merkle tree depth per layer |
| MAX_CHAIN_LEN | ≥ 11 | Maximum prev-chain steps (from `gosh-dense-balanced-tree`) |

### 3.5 Dependencies

| Crate | Source | Purpose |
|-------|--------|---------|
| `halo2-base` 0.4.0 | `gosh-sh/halo2-lib-zkevm-sha256-and-bls12-381` | Circuit builder (Axiom fork) |
| `halo2-ecc` 0.4.0 | Same repo | Elliptic curve operations |
| `gosh-sha256-chip` | `gosh-sh/gosh-halo2-crypto-lib` | In-circuit SHA-256 |
| `gosh-dense-balanced-tree` | Same repo | Poseidon Merkle chain verification |
| `gosh-bls-verification` | Same repo | BLS12-381 signature verification |
| `pse-poseidon` | `axiom-crypto/pse-poseidon` | Native Poseidon (tests) |
| `tvm_block`, `tvm_types` | `tvmlabs/tvm-sdk` (branch `feature/cache-poseidon-spec`) | Block data generation (test) |

**Missing dependency**: `gosh-halo2-crypto-lib` must be cloned to `../gosh-halo2-crypto-lib` for the `[patch]` table to resolve. The repository is at `https://github.com/gosh-sh/gosh-halo2-crypto-lib` but requires explicit access grant.

### 3.6 Test Fixtures

Four JSON fixtures captured from a local Acki Nacki node with window size 4 (LAYER_TREE_DEPTH=2):

| Fixture | Layers | Block Height | Prev Height | Chain Steps |
|---------|--------|-------------|-------------|-------------|
| `circuit_test_data_L2_H16_prevH0_S1` | 2 | 16 | 0 | 1 |
| `circuit_test_data_L2_H32_prevH16_S1` | 2 | 32 | 16 | 1 |
| `circuit_test_data_L5_H12288_prevH1024_S11` | 5 | 12288 | 1024 | 11 |
| `circuit_test_data_L6_H45056_prevH0_S11` | 6 | 45056 | 0 | 11 |

Fixture JSON fields:
- **Public**: `num_layers`, `root_hashes_hex`, `prev_max_level_layer_hash_hex`
- **Private witness**: `block_envelope_hex`, `attestation_hex`, `bk_set`, `layer_hash_byte_offsets`, `num_prev_chain_steps`, `prev_chain_proofs`

### 3.7 Halo2 Stack Compatibility

The partner uses **halo2-base 0.4.0** with `halo2-axiom` feature (Axiom's Halo2 fork), **KZG + SHPLONK on BN254**, and **Blake2b transcript** (via `gosh-zk-snark-halo2-utils`). This matches the BN254 field used by Ethereum's pairing precompiles, making Groth16 wrapping feasible.

---

## 4. Partner's Work: ZK SNARK Halo2 Utils

### 4.1 Repository: `gosh-zk-snark-halo2-utils`

A generic Halo2/KZG utility library standardizing:
- KZG SRS loading (`ParamsKZG<Bn256>`)
- Circuit layout config I/O (`BaseCircuitParams` as JSON)
- Proving/verifying key generation and serialization
- Proof creation and verification with Blake2b transcript + SHPLONK

### 4.2 Key APIs

**`keygen.rs`**: `generate_and_save_keys<C: Circuit<Fr>>` — runs `keygen_vk` / `keygen_pk` and saves VK, PK, and config params to files.

**`proof.rs`**: `Proof` type with:
- `create_for_circuit` / `create_for_circuit_from_paths` — prove using Blake2bWrite + ProverSHPLONK
- `verify_with_vk` / `verify_with_vk_from_bytes` / `verify_with_vk_from_path` — verify using Blake2bRead + VerifierSHPLONK + SingleStrategy

**`io.rs`**: Universal PK/VK deserialization using `BaseCircuitBuilder<Fr>` as the config type — works for any circuit whose `configure_with_params` delegates to `BaseCircuitBuilder`.

### 4.3 Transcript

**Blake2b** Fiat-Shamir transcript with `Challenge255` for challenge squeezing. This is the transcript the Ethereum verifier must reproduce if verifying Halo2 proofs directly (or the Groth16 wrapper must understand when wrapping).

### 4.4 Test Workflow (from Alina's Instructions)

1. **Keygen** (`test_layer_hashes_keygen_d3`): Load fixture → build circuit → gen SRS(K=19) → `generate_and_save_keys` → writes `keys/layer_hashes_d3_vk.bin` (~small), `_pk.bin` (~5 GB), `_config_params.json`
2. **Prove** (`test_layer_hashes_real_data_prove_d3`): Load fixture → build instances → reload PK/config → `Proof::create_for_circuit_from_paths` → save proof + instances
3. **Verify** (`test_layer_hashes_real_data_verify_*_d3`): Load proof + instances + VK → verify
4. **All fixtures** (`test_layer_hashes_prove_and_verify_all_fixtures_d3`): Prove + verify all 4 fixtures against the same d3 keys

One keygen produces keys that work for all fixtures with the same circuit parameters (same K, same `LAYER_TREE_DEPTH`).

---

## 5. Gap Analysis

### What Exists

| Component | Status |
|-----------|--------|
| Ethereum bridge contract (deposit/withdraw) | Complete |
| Groth16 deposit verifier | Complete |
| Deposit-prover Halo2 circuit | Complete |
| Gnark Groth16 wrapper (for deposits) | Working (stub `Define`) |
| Partner's layer-hashes Halo2 circuit | Complete + tested |
| Partner's utils (keygen/prove/verify) | Complete + tested |
| Test fixtures from real node | 4 fixtures available |

### What's Missing for Integration

| Component | Status | Notes |
|-----------|--------|-------|
| `gosh-halo2-crypto-lib` source review | Done | SHA-256, BLS, dense tree chips audited; see audit doc |
| `halo2-lib` Gosh fork review | Done | Fork audited; adds BLS12-381 module; core halo2-base unchanged; build succeeds |
| Partner circuit build + test | Done | Builds with `state_to_bytes` pub fix; mock + real prover tests pass |
| Gnark wrapper for layer-hashes circuit | Not started | Different public input count (13 vs 7), different VK |
| Ethereum contract for layer hash updates | Not started | Store/update layer hashes, verify Groth16 proofs |
| Layer hash verifier contract | Not started | Like `Groth16DepositVerifier` but for 13 inputs |
| Relayer service | Not started | Watch AN node → prove → wrap → submit to ETH |
| BK set rotation mechanism | Undefined | How/when does the bridge learn about new BK sets? |
| `acki-nacki-interface` real implementation | Not started | Only traits + mock exist |
| Production LAYER_TREE_DEPTH=8 testing | Not started | Only depth=2 (window=4) fixtures available |
