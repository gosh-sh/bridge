> **⚠️ ARCHIVED 2026-08-18 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/EVM-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

# Acki Nacki Bridge — Integration Analysis

> **v2 update (2026-05-10).** §3 and §5 have been rewritten for the four-circuit architecture
> (Phase 4.2 demolition complete; Phase 5.1 relayer skeleton complete). The single-circuit
> `LayerHashesUpdateCircuit` narrative is preserved only where the partner's code is still
> physically deployed (its `gosh-bls-verification`, `gosh-dense-balanced-tree`, and
> `gosh-sha256-chip` chips are all still in use, by Circuit 1A/1B/2). The active architecture
> overview lives in `docs/four_circuit_architecture.md`.
>
> **v2.1 update (2026-05-17, Phase 4.3 demolition).** §1 has been rewritten to remove the
> legacy ETH-side deposit-verifier chain: the `withdraw(...)` user path, the
> `IAckiNackiVerifier` interface, `Groth16DepositVerifier` adapter, `Groth16Verifier` (gnark-
> generated for deposit-prover), `DummyVerifier`, and the deposit-prover-side `gnark-wrapper`
> Go tree are **all gone**. The ETH→AN deposit-event proof is now consumed natively on the AN
> side via the future `VERHALO2SHPLONK` TVM opcode (in development in `tvm-sdk`). The AN→ETH
> per-circuit gnark wrappers under `crates/bridge-snark-utils/gnark-wrappers/` are
> unaffected (EIP-170 still forces a wrap on that side). See Decision Log 2026-05-17 in
> `docs/an_partner_integration_plan.md`.

This document provides a comprehensive analysis of both sides of the Acki Nacki cross-chain bridge: our Ethereum-side implementation and the partner's Acki Nacki-side ZK circuits.

## Table of Contents

1. [Our Side: Ethereum Bridge](#1-our-side-ethereum-bridge)
2. [Acki Nacki Platform](#2-acki-nacki-platform)
3. [Partner's Work: Four-Circuit Architecture (v2)](#3-partners-work-four-circuit-architecture-v2)
4. [Partner's Work: ZK SNARK Halo2 Utils](#4-partners-work-zk-snark-halo2-utils)
5. [Gap Analysis (v2)](#5-gap-analysis-v2)

---

## 1. Our Side: Ethereum Bridge

### 1.1 Architecture Overview

The `acki-nacki-bridge` repository implements the **Ethereum side** of a cross-chain bridge. It consists of:

- **Solidity smart contracts** (Foundry) — bridge vault, AN→ETH state verifiers, block hash oracles, AAVE V3 wiring.
- **deposit-prover** (Rust + axiom-eth) — Halo2 circuit proving Ethereum deposit events; the proof is consumed natively on the AN side (no gnark wrapper — retired in Phase 4.3 2026-05-17).
- **`crates/bridge-snark-utils/`** (Rust + Go) — drives the partner's 4-circuit Halo2 stack (Circuit 1A / 1B / 2 / [3]) and the per-circuit gnark Groth16 wrappers under `gnark-wrappers/circuit-{1a,1b,2}/`. These wrappers stay because the AN→ETH side is gated by EIP-170.
- **`crates/bridge-relayer-daemon/`** (Rust) — Phase 5.1 relayer skeleton (`Relayer::tick()` / `run_loop()`).
- **acki-nacki-interface** (Rust) — trait definitions for Acki Nacki blockchain interaction (mock-only).

### 1.2 Smart Contracts

All contracts are in `contracts/ethereum/src/`, compiled with Solidity 0.8.19 via Foundry.

#### AckiNackiBridge.sol

Main bridge contract with one user-facing entry point (`deposit()`), the AN→ETH state-update entry point `verifyBlock(...)`, and an optional AAVE V3 yield path:

- **`deposit()`** — accepts ETH (max 100 ETH), increments `depositCounter`, emits `Deposit(depositId, sender, amount, timestamp)`. Funds accumulate in the contract; `deposit()` itself never calls AAVE (kept cheap).
- **`verifyBlock(finType, attProof, attInputs, lhmProof, lhmInputs)`** — verifies the per-circuit Primary (1A) / Fallback (1B) attestation proof + the Circuit 2 layer-hashes movement proof, enforces cross-circuit binding (`block_id`, `bk_set_poseidon` must match between proofs), strict monotonicity (`block_seq_no > storedLastSeenBlockSeqNo`), and the Poseidon chain anchor (`prev_max_level_layer_hash == storedPrevMaxLevelLayerHash`).
- **(removed in Phase 4.3, 2026-05-17)** The legacy refund-style `withdraw(recipient, amount, depositId, blockNumber, proof)` and the `isDepositProcessed(depositId)` view used to live here. Both were retired together with the `IAckiNackiVerifier` chain.

Constructor (5 args): `(blockHeaderOracle, aavePool, wethGateway, aWETH, primaryVerifier|adapters)`. Pass `address(0)` for the AAVE addresses to disable AAVE; this keeps the bridge in plain-ETH custody mode. (In practice the production wiring also takes the Phase 4 `IPrimaryVerifier`, `IFallbackVerifier`, `ILayerHashesMovementVerifier` triplet — see `script/DeployRealBridge.s.sol` for the exact signature in your tree.)

Storage:

- Core: `depositCounter`, `treasuryBalance`, `blockHeaderOracle` (preserved for the future burn-proof flow but unused by the present surface).
- AN→ETH state: `storedBkSetCommitment`, `storedLastSeenBlockSeqNo`, `storedPrevMaxLevelLayerHash`, plus the three Phase 4 verifier addresses.
- AAVE: immutable `aavePool` / `wethGateway` / `aWETH`; `aaveEnabled`, `suppliedPrincipal` (book value of supplied ETH), `liquidReserveBps` (default 1000 = 10 %, capped at 5000).
- Access: `owner` (administers AAVE routing + AN-state genesis configuration), `yieldRecipient` (gets harvested yield).

`verifyBlock` is permissionless — anyone with a valid proof tuple can advance the AN state. `deposit()` is permissionless. The `owner` role only governs AAVE routing (`supplyToAave`, `withdrawFromAave`, `emergencyWithdrawAll`, `setAaveEnabled`, `setLiquidReserveBps`, `harvestYield`, `setYieldRecipient`, `transferOwnership`) and the AN-state genesis (`initializeAnState`). The owner **cannot** withdraw user principal: `harvestYield` is bounded by `aWETH.balanceOf(bridge) - suppliedPrincipal`, and there is no admin path that bypasses ZK verification.

See `docs/aave_integration.md` for the AAVE design, invariants, and verification protocol. See `docs/four_circuit_architecture.md` for the cross-circuit binding semantics.

#### IBlockHeaderOracle.sol

Oracle interface for trusted block hash sources:

- `getBlockHash(blockNumber)`, `isBlockHashAvailable(blockNumber)`, `getLatestVerifiedBlock()`
- Implementations: `MockBlockHeaderOracle` (testing), `AxiomBlockHeaderOracle` (production via Axiom V2)
- Currently unused by the public surface; preserved for a future burn-proof / ETH-side withdrawal flow.

#### AN→ETH per-circuit verifiers (Phase 4)

**Production (since 2026-06-22) wires all three circuits to the R15 SHPLONK aggregator adapters** — the gnark Groth16 path below is retained for 1A/2 test coverage only:

- `IFallbackVerifier.sol` / `FallbackAggregatorVerifier.sol` — Circuit 1B production adapter (Fallback attestation, > 1/2 split), 4 inner public inputs; wraps `verifiers/FallbackAggregatorVerifier.bin` (inner `K=21` so the Yul fits EIP-170). `PrimaryAggregatorVerifier.sol` / `LayerHashesAggregatorVerifier.sol` are the sibling SHPLONK adapters for 1A / 2.
- `IPrimaryVerifier.sol` / `PrimaryVerifier.sol` — Circuit 1A **gnark Groth16** adapter (≥ 2/3 BLS quorum), 4 public inputs — retained for test coverage only.
- `ILayerHashesMovementVerifier.sol` / `LayerHashesMovementVerifier.sol` — Circuit 2 **gnark Groth16** adapter, 14 public inputs — retained for test coverage only.
- `PrimaryGroth16VerifierGenerated.sol`, `LayerHashesGroth16VerifierGenerated.sol` — gnark-generated (1A / 2), do not edit manually. The 1B `FallbackGroth16VerifierGenerated.sol` + `FallbackVerifier.sol` were deleted when Circuit 1B moved to the SHPLONK aggregator.

#### Blake2b Verification Path

For Acki Nacki → Ethereum proofs in the bare Halo2 form (used by tests and as a reference; production goes through the SHPLONK aggregator adapters above):

- `Blake2bHalo2Verifier.sol` — Halo2 verifier using EIP-152 Blake2b precompile.
- `Blake2bTranscript.sol` — Fiat-Shamir transcript matching `halo2_proofs::Blake2bWrite`.
- `Blake2bChallengeComputer.sol` — challenge computation (split out for 24KB limit).

### 1.3 ZK Proof Pipeline (Deposits, post-Phase 4.3)

```
Ethereum deposit event (deposit() on AckiNackiBridge)
    → deposit-prover (Rust/axiom-eth): Halo2 circuit
        - Receipt trie inclusion (MPT proof)
        - RLP decoding, event log extraction
        - Event signature verification (keccak256)
        - Contract address binding
        - Block hash binding
        → 7 public inputs: [depositId, sender, amount, contractAddress,
                            blockHashHigh, blockHashLow, promiseCommit]

    → AN-side native verification (planned, via VERHALO2SHPLONK TVM opcode)
        - tvm-sdk: new opcode wraps `ark-groth16`-style native Halo2 SHPLONK verification
        - AN-side TokenBridge.finalizeDeposit(proof, publicInputs, vk)
            consumes the proof and mints the user's tokens
        - depositId nullifier prevents replay
```

The keccak coprocessor pattern (documented in `docs/keccak_coprocessor_flowchart.mmd`) delegates expensive keccak256 computations to a separate circuit via Poseidon-based promise commitments, achieving ~500x constraint savings per hash.

### 1.4 Integration Surfaces

**`crates/acki-nacki-interface`**: Defines `IAckiNacki` and `TransactionSender` traits for Acki Nacki blockchain interaction. Currently **mock-only** — the actual implementation is expected from the Acki Nacki team.

**`crates/eth-frontend`**: `DepositManager` is functional. The `contract.rs` abigen exposes `deposit()` + read-only views. The legacy `WithdrawalManager` stub and the v1 `withdraw` ABI rows were removed in Phase 4.3 (2026-05-17).

**`crates/bridge-snark-utils/gnark-wrappers/`**: One gnark wrapper per circuit (1A / 1B / 2), each producing its own `Groth16Verifier.sol`. Adapting them for a new circuit involves duplicating the per-circuit folder and updating the JSON schema + circuit struct.

> **Stub-status caveat (R15)**: as of 2026-05-17 the `circuit.go` in each gnark wrapper is a no-op (identity-only Define). The on-chain `*Groth16VerifierGenerated.sol` artefacts therefore do not yet cryptographically constrain the Halo2 SHPLONK proof — they verify Groth16 over a trivial relation. Phase 8 (Decision Log 2026-05-17) is the R&D track that closes this gap. Mainnet `v2.0.0` is explicitly gated on it.

### 1.5 Workspace Layout (v2, post-Phase 4.2)

Two main Cargo workspaces plus three excluded crates due to dependency incompatibilities:

| Workspace / Crate | Members | Halo2 Stack |
|---|---|---|
| Root `Cargo.toml` | `crates/eth-frontend`, `crates/acki-nacki-interface` | None |
| `deposit-prover/` (excluded) | `deposit-prover` | axiom-eth + halo2-pse v2023_04_20 |
| `crates/bridge-snark-utils/` (excluded) | orchestrator + per-circuit gnark wrappers | partner's `bridge-prover-lib` (halo2-axiom 0.5.x via gosh fork) |
| `crates/bridge-relayer-daemon/` (excluded) | relayer daemon (Phase 5.1, mock sources) | none — pure orchestration + ethers-rs |

Phase 4.2 removed the legacy `layer-hashes-prover/` and `bk-set-rotation-prover/` crates;
`crates/bridge-snark-utils/` is the v2 replacement and now hosts the per-circuit
gnark wrappers under `gnark-wrappers/{circuit-1a,circuit-1b,circuit-2}/`.

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

## 3. Partner's Work: Four-Circuit Architecture (v2)

### 3.1 Repositories

| Repo | Purpose |
|---|---|
| `acki-nacki-to-eth-bridge-halo2-circuits` | All four Halo2 circuits (`attestation-bls-checker-circuit/{primary,fallback}_circuit.rs`, `historical-layer-hashes-movement-checker-circuit`, `bk-set-update-checker-circuit`) plus the canonical envelope-hash spec (`circuits/ENVELOPE_HASH_MERKLE_SPEC.md`). |
| `acki-nacki-to-eth-bridge-halo2-prover` | `bridge-prover-lib` — keygen, BK-set fetcher, BOC parser; `bridge-test-data-gen` — synthetic generators used by our orchestrator's bound test data; `bridge-verifier-daemon` — partner's standalone Circuit-1A daemon (we don't use it; we have our own relayer). |
| `gosh-halo2-crypto-lib` | Reused by all four circuits: `gosh-sha256-chip`, `gosh-bls-verification`, `gosh-dense-balanced-tree`. Audit findings BLS-1 and FORK-2 (G2 subgroup gap) carry over to v2. |
| `halo2-lib-zkevm-sha256-and-bls12-381` | Gosh fork of axiom-crypto's halo2-lib; adds the BLS12-381 module to halo2-ecc. |
| `gosh-zk-snark-halo2-utils` | Generic keygen/prove/verify helpers (KZG SRS, Blake2b transcript, SHPLONK). Used by all four circuits. |

### 3.2 What the Four Circuits Prove

See `docs/four_circuit_architecture.md` §1 for the canonical table. Quick summary:

- **Circuit 1A (Primary attestation)** — BLS aggregate ≥ 2/3 of BK set signed an envelope whose `block_id` is committed; PI = `[blockId, bkSetCommitment, blockSeqNo, lastSeenBlockSeqNo]`.
- **Circuit 1B (Fallback attestation)** — same shape as 1A but for fallback target_type and >1/2 threshold.
- **Circuit 2 (Layer hashes movement)** — SHA-256-binds the layer-hash preimage to envelope leaf-0, runs a Poseidon Merkle chain from `prevMaxLevelLayerHash` to the new top-of-chain, range-checks `numLayers ∈ [1,10]`. PI = `[blockId, bkSetCommitment, numLayers, layerHashes[0..10], prevMaxLevelLayerHash]` (14 elements).
- **Circuit 3 (BK-set update)** — planned for Phase 1.C; will prove the previous committee attests the next committee, advancing `storedBkSetCommitment` in-place. PI = `[oldBkSetCommitment, newBkSetCommitment]`.

The *cross-circuit binding* lives at the public-input level: `block_id` (envelope leaf-1, offset
48) and `bk_set_poseidon` (envelope leaf-2, offset 96) appear at fixed indices in each circuit's
PI vector. The bridge passes a single `blockId` and a single `bkSetCommitment` argument into both
verifier calls — any mismatch between the two proofs surfaces as a gnark `false`. Cost: zero
additional gas vs. the legacy single-circuit design.

### 3.3 Per-Circuit Parameters (v2)

| Parameter | 1A Primary | 1B Fallback | 2 Layer hashes | 3 BK update (planned) |
|---|---|---|---|---|
| K | 19 | 19 | 19 | TBD |
| Public inputs | 4 | 4 | 14 | 2 |
| BLS12-381 path | Aggregate ≥ 2/3 | Aggregate > 1/2 | (none) | (none) |
| SHA-256 path | over attestation envelope | over fallback envelope | over layer-hash preimage (331 B) | (none) |
| Poseidon path | over BK set | over BK set | over BK set + Merkle chain | over old/new BK sets |
| Halo2 PK size | ~2 GB | ~2 GB | ~2 GB | TBD |
| Halo2 prove time | 3-6 min | 3-6 min | 3-6 min | TBD |
| gnark Groth16 wrap | 5-15 s | 5-15 s | 5-15 s | TBD |
| Final proof size | 256 B | 256 B | 256 B | 256 B |

`MAX_LAYERS = 10`, `LAYER_TREE_DEPTH = 8` (production) / 2 (test), `MAX_SIGNERS = 300`,
`MAX_BLOCK_DATA_BYTES = 4096`, `LIMB_BITS = 104`, `NUM_LIMBS = 5` are still the
production-relevant constants — unchanged from v1.

### 3.4 Test Fixtures (v2)

The legacy four "real-data" fixtures (`L2_H16_prevH0_S1`, `L2_H32_prevH16_S1`,
`L5_H12288_prevH1024_S11`, `L6_H45056_prevH0_S11`) were specific to the single-circuit
13-public-input layout and were retired with the legacy `layer-hashes-prover` crate in
Phase 4.2. They are not portable to v2 because:

- the new envelope hash tree (8 leaves, partner commit `672854b`) is not present in the
  AN node version that produced those fixtures;
- the public-input layout grew from 13 to a tuple of (4, 14) elements;
- the cross-circuit binding via `block_id` (envelope leaf-1 at offset 48) didn't exist yet.

v2 fixtures come in two flavours:

- **Bound synthetic fixtures**: generated by `crates/bridge-snark-utils` via
  `cargo run --bin export-bound-block-proofs`. Same-process consistency between Circuit 1A and
  Circuit 2; non-deterministic across runs (uses `rand::thread_rng()` inside the partner's
  `bridge-test-data-gen::generator::generate_bridge_test_data`).
- **Live-node fixtures**: pending Phase 5.2 — blocked on Q1 (AN-node branch
  `latest_an_to_eth_bridge_test` availability) and Q2 (`transition_hashes` migration).

### 3.5 Halo2 Stack Compatibility

Unchanged from v1: halo2-base 0.4.0 with `halo2-axiom` feature, KZG + SHPLONK on BN254,
Blake2b transcript via `gosh-zk-snark-halo2-utils`. The trusted setup (`kzg_bn254_19.srs`)
is shared across all four circuits.

### 3.6 What Each Circuit Internally Enforces (constraint groups)

The constraint anatomy is the same across 1A/1B/2 and reuses the audited gosh chips. Quick sketch:

| Constraint group | Where it appears | Notes |
|---|---|---|
| SHA-256 binding (4096-byte preimage, 65 × 64 compression blocks) | 1A, 1B (over attestation envelope), 2 (over 331-byte layer-hash preimage padded to 4096) | `gosh-sha256-chip` |
| Target-type enforcement (Primary vs Fallback bincode discriminant) | 1A (=Primary), 1B (=Fallback) | 4 bytes at `TARGET_TYPE_REL_OFFSET = 116` of `AttestationData` |
| Layer-count range check `[1, MAX_LAYERS]` | 2 only | three 4-bit range checks: `numLayers`, `numLayers-1`, `MAX_LAYERS-numLayers` |
| Layer-hash extraction from `block_data` at bincode-predictable offsets (`BTREE_ENTRY_SIZE = 124`) | 2 only | one-hot inner-product byte selector + powers-of-256 packing |
| Prev-chain Poseidon Merkle verification | 2 only | `gosh-dense-balanced-tree::verify_chain_of_dense_proofs`; Poseidon T=3 RATE=2 R_F=8 R_P=57 |
| BLS12-381 aggregate signature verification | 1A (≥ 2n/3 threshold), 1B (> n/2 threshold) | `gosh-bls-verification`; cross-field arithmetic with LIMB_BITS=104 NUM_LIMBS=5 |
| BK set Poseidon commitment | 1A, 1B, 2 | Poseidon over sorted `(signer_index, x_limbs)` tuples; only x-coordinate is committed (y is derivable) |
| Envelope-hash leaf binding (8-leaf SHA-256 tree, partner commit `672854b`) | 1A, 1B (leaf-1 = `block_id` at offset 48), 2 (leaf-0 = layer-hash preimage at offset 0; leaf-1 = `block_id`) | this is the cross-circuit binding hook — see `four_circuit_architecture.md` §2 |

### 3.7 Dependencies

| Crate | Source | Purpose |
|---|---|---|
| `halo2-base` 0.4.0 | `gosh-sh/halo2-lib-zkevm-sha256-and-bls12-381` | Circuit builder (Axiom fork) |
| `halo2-ecc` 0.4.0 | Same repo | Elliptic curve operations |
| `gosh-sha256-chip` | `gosh-sh/gosh-halo2-crypto-lib` | In-circuit SHA-256 |
| `gosh-dense-balanced-tree` | Same repo | Poseidon Merkle chain verification |
| `gosh-bls-verification` | Same repo | BLS12-381 signature verification (G2 subgroup gap = audit FORK-2 / BLS-1, carried over to v2) |
| `pse-poseidon` | `axiom-crypto/pse-poseidon` | Native Poseidon (tests) |
| `tvm_block`, `tvm_types` | `tvmlabs/tvm-sdk` (branch `feature/cache-poseidon-spec`) | Block data generation (test) |

`gosh-halo2-crypto-lib` must be cloned to `../gosh-halo2-crypto-lib` for the `[patch]` table to resolve.

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

**Blake2b** Fiat-Shamir transcript with `Challenge255` for challenge squeezing. This is the transcript the Ethereum-side verifier must reproduce when verifying Halo2 proofs directly, and the one the AN→ETH gnark wrappers must understand when wrapping (the ETH→AN deposit-prover side uses a Keccak transcript instead, since its proof is consumed natively on the AN side via `VERHALO2SHPLONK`).

### 4.4 Test Workflow (from Alina's Instructions)

1. **Keygen** (`test_layer_hashes_keygen_d3`): Load fixture → build circuit → gen SRS(K=19) → `generate_and_save_keys` → writes `keys/layer_hashes_d3_vk.bin` (~small), `_pk.bin` (~5 GB), `_config_params.json`
2. **Prove** (`test_layer_hashes_real_data_prove_d3`): Load fixture → build instances → reload PK/config → `Proof::create_for_circuit_from_paths` → save proof + instances
3. **Verify** (`test_layer_hashes_real_data_verify_*_d3`): Load proof + instances + VK → verify
4. **All fixtures** (`test_layer_hashes_prove_and_verify_all_fixtures_d3`): Prove + verify all 4 fixtures against the same d3 keys

One keygen produces keys that work for all fixtures with the same circuit parameters (same K, same `LAYER_TREE_DEPTH`).

---

## 5. Gap Analysis (v2)

### 5.1 What Exists Today (HEAD)

| Component | Status |
|---|---|
| Ethereum bridge vault (`AckiNackiBridge.sol`) — `deposit()` + Phase 4 `verifyBlock()` + AAVE | Complete (Phase 4.3 retired the legacy `withdraw()` flow, 2026-05-17) |
| AAVE V3 yield bolt-on | Complete (20 tests, see `aave_integration.md`) |
| Block-hash oracle (Axiom V2) | Complete (currently unused by the public surface; reserved for the future burn-proof flow) |
| Deposit-prover Halo2 circuit (ETH→AN) | Complete; consumed natively on the AN side via `VERHALO2SHPLONK` (in development) — no gnark wrapper |
| Partner's four Halo2 circuits (1A, 1B, 2, 3) | All complete + MockProver tests pass |
| Partner's `bridge-prover-lib` (keys, BK-set fetcher, BOC parser) | Live (Circuit 1A wired upstream; we extend on our side) |
| Partner's `bridge-test-data-gen` | Live; produces synthetic bound block scenarios |
| Our `crates/bridge-snark-utils` (Rust + per-circuit gnark wrappers) | Done — Phase 4.1 (bound test data generator + `export-bound-block-proofs` binary) |
| `AckiNackiBridge.verifyBlock` (Phase 4 AN→ETH state) | Done — additive Phase 4.1; Phase 4.2 demolished the legacy `LayerHashBridge.sol` |
| Foundry tests for verifyBlock | 17 single-block (`AckiNackiBridgeVerifyBlockTest`) + 6 multi-block (`AckiNackiBridgeRelayerLoopTest`) — all green |
| `crates/bridge-relayer-daemon` (relayer skeleton) | Done — Phase 5.1 (mock sources + mock bridge client); CLI runnable |

### 5.2 What's Still Missing

| Component | Phase | Blocked on |
|---|---|---|
| Live AN-node `BlockSource` (replaces mock) | 5.2 | Q1: confirm `latest_an_to_eth_bridge_test` branch exists in upstream; Q2: confirm `transition_hashes` migration in AN node — Plan B is to recompute locally inside the relayer |
| 10-block shellnet end-to-end against Anvil | 5.3 | Phase 5.2 |
| Circuit 3 (BK-set update) wiring + Solidity verifier + `bkSetUpdateProof` argument to `verifyBlock` | 1.C | Partner signal that `bk-set-update-checker-circuit` is feature-complete |
| Production `LAYER_TREE_DEPTH=8` proof generation against live data | 5.2 | Live node; the synthetic bound generator already supports it |
| `acki-nacki-interface` real (non-mock) implementation | 5.2 | Same as above |

### 5.3 Trust-assumption Delta vs v1

See `docs/four_circuit_architecture.md` §8 for the canonical list. Five legacy assumptions
are now *removed* (cross-circuit binding via `block_id`, monotonic seqno, chain anchor,
finalization-type routing, no silent garbage in layer-hash tail). No new assumptions
introduced for the AN→ETH path. Three carry over unchanged: Halo2/SHPLONK soundness,
`gosh-halo2-crypto-lib` correctness (incl. open BLS-1/FORK-2 medium audit findings), AN BFT
economic security.

### 5.4 Trust-assumption Delta from Phase 4.3 (ETH→AN path, 2026-05-17)

Phase 4.3 retired the legacy refund-style `withdraw()` mechanism and its ETH-side gnark
chain. Effect on the *deposit* side trust model:

- **Removed**: trust in the ETH-side `Groth16DepositVerifier` adapter + the gnark-generated
  verifier, since neither is on the path anymore. (Both were also subject to R15 — the
  no-op gnark wrapper finding — so this removal is a net trust reduction.)
- **Removed**: trust in the EIP-170-driven Halo2 → Groth16 wrapping for this direction,
  including any 24 KB-imposed simplifications.
- **Added**: trust in (a) the new `VERHALO2SHPLONK` TVM opcode implementation in `tvm-sdk`
  and its CI/audit story, and (b) the AN-side `TokenBridge` contract that will call it. Both
  are tracked in `docs/an_partner_integration_plan.md` Decision Log 2026-05-17 and Phase 8.
- **Unchanged**: trust in the deposit-prover Halo2 circuit itself (`deposit-prover/`), the
  Ethereum RPC quorum used to feed it (which now lives only producer-side), and the keccak
  coprocessor commitment.
