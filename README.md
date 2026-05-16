# Acki Nacki Bridge

A cross-chain bridge between Ethereum and [Acki Nacki](https://docs.ackinacki.com/), using zero-knowledge proofs end-to-end. Ethereum-side state is held by `AckiNackiBridge.sol`; AN-side state is verified through a four-circuit Halo2 / Groth16 stack consumed by `verifyBlock(...)`.

## Overview

The bridge provides two cryptographically-distinct directions:

- **Ethereum → Acki Nacki (deposits)**. The user calls `AckiNackiBridge.deposit()` on Ethereum; a `Deposit` event is emitted. An off-chain prover (`deposit-prover/`) generates a Halo2 SHPLONK proof of that event. The AN side verifies the Halo2 proof **natively** through a TVM opcode (`VERHALO2SHPLONK` — work-in-progress in `tvm-sdk`; see Decision Log 2026-05-17 in `docs/an_partner_integration_plan.md`) and mints tokens to the user. No Ethereum-side `withdraw` is exposed.
- **Acki Nacki → Ethereum (state attestation)**. `AckiNackiBridge.verifyBlock(...)` advances a rolling commitment to the Acki Nacki side by checking a tuple of two cross-circuit-bound Halo2 proofs, each wrapped in gnark Groth16 to fit Ethereum's EIP-170 24 KB limit: a Primary (1A) or Fallback (1B) attestation, and a Circuit 2 layer-hashes movement proof. A future Circuit 3 will add an optional BK-set update proof.

Future genuine cross-chain withdrawals (token burn on AN → ETH release on Ethereum) will land alongside a burn-proof circuit + state-anchored verification — see §3 Phase 4 open design question and Decision Log 2026-05-17 in `docs/an_partner_integration_plan.md`. The legacy v1 refund-style `withdraw(depositId, ...)` was retired in Phase 4.3 (2026-05-17).

## Architecture

```
                  Ethereum → Acki Nacki (Deposits)
┌────────────────────┐         ┌──────────────────────────────┐
│ AckiNackiBridge    │         │ deposit-prover (Rust + Halo2)│
│   deposit()        │ ──▶ event ──▶ off-chain prover ────────│
│   (idle ETH →      │         │  • MPT receipt-inclusion     │
│    AAVE V3 yield)  │         │  • event log binding         │
└────────────────────┘         │  • keccak coprocessor        │
                               └──────────────────────────────┘
                                              │
                                              ▼ Halo2 SHPLONK proof
                               ┌──────────────────────────────┐
                               │ AN-side native verification  │
                               │ VERHALO2SHPLONK TVM opcode   │
                               │ (`tvm-sdk`, in development)  │
                               └──────────────────────────────┘


                  Acki Nacki → Ethereum (State attestation)
┌─────────────────────────┐         ┌──────────────────────────────┐
│ AN node + relayer       │ ──▶     │ bridge-prover-orchestrator   │
│ • blocks, attestations, │  data   │ • Circuit 1A/1B (attestation)│
│   layer-hashes, BK sets │         │ • Circuit 2 (layer hashes)   │
└─────────────────────────┘         │ • Halo2 SHPLONK + gnark wrap │
                                    └──────────────────────────────┘
                                                  │
                                                  ▼ Groth16 proofs + public inputs
                               ┌──────────────────────────────────┐
                               │ AckiNackiBridge.verifyBlock(...) │
                               │ • PrimaryVerifier / Fallback     │
                               │ • LayerHashesMovementVerifier    │
                               │ • cross-circuit binding +        │
                               │   monotonic seq + chain anchor   │
                               └──────────────────────────────────┘
```

### Pipeline detail — Ethereum → Acki Nacki (deposits)

1. User calls `AckiNackiBridge.deposit()` with ETH (`MAX_DEPOSIT_AMOUNT = 100 ether`). Contract increments `depositCounter`, adds to `treasuryBalance`, emits `Deposit(depositId, sender, amount, timestamp)`.
2. Idle ETH can be routed by the owner into AAVE V3 via `supplyToAave()` for yield (see `docs/aave_integration.md`).
3. `deposit-prover` (Rust + axiom-eth) fetches the transaction receipt + MPT inclusion path from an Ethereum RPC and builds a Halo2 circuit that proves the `Deposit` event was emitted by the bridge contract in a real Ethereum block.
4. The Halo2 proof is consumed natively on the AN side by the `VERHALO2SHPLONK` TVM opcode (under development in `tvm-sdk`); the AN-side bridge contract validates the public inputs and mints the corresponding token to the user.

### Pipeline detail — Acki Nacki → Ethereum (state attestation)

1. Acki Nacki blocks carry an 8-leaf SHA-256 Merkle `block_id`, a Poseidon commitment to the BK set, BLS-aggregated attestations (Primary or Fallback finalization), and layer-hash data tied to a dense balanced Poseidon Merkle chain.
2. `bridge-prover-orchestrator` (Rust crate, excluded from the root workspace) drives the partner's four-circuit Halo2 stack to produce:
   - Circuit 1A (Primary attestation) **or** Circuit 1B (Fallback attestation) — public inputs `[block_id, bk_set_poseidon, block_seq_no, last_seen_block_seqno]`.
   - Circuit 2 (Layer-hashes movement) — 14 public inputs `[block_id, bk_set_poseidon, num_layers, layer_hash[0..10], prev_max_level_layer_hash]`.
3. Each Halo2 proof is wrapped via the per-circuit gnark Groth16 wrappers under `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-{1a,1b,2}/` to produce a ~256-byte Groth16 proof and an auto-generated Solidity verifier (~26 KB source / ~7 KB runtime) under `contracts/ethereum/src/*Groth16VerifierGenerated.sol`.
4. The relayer (`crates/bridge-relayer-daemon`) submits the proof tuple to `AckiNackiBridge.verifyBlock(...)`, which enforces:
   - `bkSetCommitment == storedBkSetCommitment` (BK-set anchor),
   - `blockSeqNo > storedLastSeenBlockSeqNo` (strict monotonicity),
   - `prevMaxLevelLayerHash == storedPrevMaxLevelLayerHash` (chain anchor),
   - `1 ≤ numLayers ≤ MAX_LAYER_HASHES (10)` and tail zeroes,
   - both Groth16 verifiers accept the proofs.

## Project Structure

```
acki-nacki-bridge/
├── contracts/ethereum/         # Solidity smart contracts (Foundry)
│   ├── src/
│   │   ├── AckiNackiBridge.sol                       # Main bridge: deposit + verifyBlock + AAVE
│   │   ├── IPrimaryVerifier.sol / PrimaryVerifier.sol
│   │   ├── IFallbackVerifier.sol / FallbackVerifier.sol
│   │   ├── ILayerHashesMovementVerifier.sol / LayerHashesMovementVerifier.sol
│   │   ├── *Groth16VerifierGenerated.sol             # gnark-generated, per-circuit
│   │   ├── Halo2Verifier.sol / Blake2bHalo2Verifier.sol  # legacy Halo2 Yul verifiers (tests)
│   │   ├── Blake2bTranscript.sol / Blake2bChallengeComputer.sol
│   │   ├── IBlockHeaderOracle.sol / MockBlockHeaderOracle.sol / AxiomBlockHeaderOracle.sol
│   │   └── IAavePool.sol / IWrappedTokenGatewayV3.sol / IERC20.sol
│   ├── test/                   # Foundry tests (109 across 12 suites)
│   └── script/                 # Deployment scripts (Deploy{Real,Test}Bridge.s.sol)
│
├── crates/                     # Main Cargo workspace + standalone crates
│   ├── acki-nacki-interface/           # AN client traits + mock implementations (workspace member)
│   ├── eth-frontend/                   # Ethereum client wrapper (workspace member, deposit-only)
│   ├── bridge-prover-orchestrator/     # 4-circuit Halo2 prover + gnark wrappers (standalone)
│   │   └── gnark-wrappers/
│   │       ├── circuit-1a/             # Primary attestation Groth16 wrapper
│   │       ├── circuit-1b/             # Fallback attestation Groth16 wrapper
│   │       └── circuit-2/              # Layer-hashes Groth16 wrapper
│   └── bridge-relayer-daemon/          # AN-watching relayer (standalone)
│
├── deposit-prover/             # Standalone Rust crate (axiom-eth ecosystem; ETH→AN Halo2 proof)
│   ├── src/
│   │   ├── circuit_v2.rs               # Halo2 circuit: proves Deposit event via MPT
│   │   ├── ethereum_fetcher.rs         # Fetches receipts + MPT proofs from Ethereum
│   │   ├── mpt.rs / rlp_utils.rs       # MPT proof construction
│   │   ├── prover.rs / aggregation.rs  # Halo2 proof generation
│   │   └── types.rs
│   └── configs/                # Circuit configuration files
│
├── poseidon-proof/             # Standalone Rust crate (Poseidon + Blake2b transcript demo)
│
├── frontend/                   # WASM web frontend (Yew + Rust, excluded from workspace)
│
├── docs/                       # Architecture + audit + protocol documents
├── Makefile, build.sh, setup.sh, test.sh
└── docker-compose.yml / Dockerfile
```

### Cargo workspaces

Three Cargo workspaces, kept separate because of dependency-tree conflicts in the Halo2 ecosystem:

| Workspace                                  | Halo2 ecosystem                | Purpose                                |
| ------------------------------------------ | ------------------------------ | -------------------------------------- |
| Root (`Cargo.toml`)                        | —                              | Workspace; `eth-frontend`, `acki-nacki-interface` |
| `deposit-prover/`                          | axiom-eth + halo2-pse 2023_04  | ETH→AN deposit-event Halo2 circuit     |
| `poseidon-proof/`                          | halo2-axiom 0.5.x              | Poseidon preimage Blake2b-transcript demo |
| `crates/bridge-prover-orchestrator/`       | halo2-axiom 0.4.x (gosh fork)  | AN→ETH 4-circuit Halo2 prover (excluded from root, own `Cargo.lock`) |
| `crates/bridge-relayer-daemon/`            | —                              | Relayer (excluded from root, mirrors the orchestrator layout) |
| `frontend/`                                | —                              | Yew WASM frontend (excluded)            |

## Smart Contracts

All contracts are in `contracts/ethereum/src/` and compiled with Solidity 0.8.19 via Foundry.

### Core

| Contract                          | Description                                                                                                                                                                                                                            |
| --------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `AckiNackiBridge.sol`             | Main bridge. `deposit()` accepts ETH and emits a `Deposit` event. `verifyBlock(...)` advances the AN→ETH commitment after verifying the per-circuit Groth16 proofs and the cross-circuit / monotonic / chain-anchor invariants. Idle ETH can be supplied to AAVE V3 by the owner. |
| `IBlockHeaderOracle.sol`          | Interface for block hash oracles (`getBlockHash`, `isBlockHashAvailable`, `getLatestVerifiedBlock`). Currently unused by the public surface; preserved for the future burn-proof flow.                                                  |

### AN→ETH state verifiers (Phase 4)

| Contract                                          | Description                                                                                                                                                                |
| ------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `IPrimaryVerifier.sol` / `PrimaryVerifier.sol`    | Interface + adapter for Circuit 1A (Primary attestation, ≥2/3 BLS quorum). Adapter assembles the 4 public inputs and calls the gnark verifier in a `try/catch`.            |
| `IFallbackVerifier.sol` / `FallbackVerifier.sol`  | Same shape, for Circuit 1B (Fallback attestation, >1/2 split).                                                                                                              |
| `ILayerHashesMovementVerifier.sol` / `LayerHashesMovementVerifier.sol` | Adapter for Circuit 2 (Layer-hashes movement). Assembles 14 public inputs.                                                                                |
| `PrimaryGroth16VerifierGenerated.sol`             | gnark-generated Groth16 verifier (BN254), produced by `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-1a/`. **Do not edit manually**.                            |
| `FallbackGroth16VerifierGenerated.sol`            | Same, produced from `gnark-wrappers/circuit-1b/`.                                                                                                                           |
| `LayerHashesGroth16VerifierGenerated.sol`         | Same, produced from `gnark-wrappers/circuit-2/`.                                                                                                                            |

> **Audit note (R15, Decision Log 2026-05-17)**: the current `circuit.go` in each `gnark-wrappers/` slot is a no-op stub — it adds identity assertions only, not a real in-gnark Halo2 SHPLONK verifier. That means the on-chain `*Groth16VerifierGenerated.sol` does **not** today cryptographically constrain the Halo2 proof. The Foundry suite continues to pass; what's affected is the *interpretation* of those passes as forgery resistance. Phase 8 of `docs/an_partner_integration_plan.md` is the R&D track that closes this gap. Mainnet `v2.0.0` is explicitly gated on it.

### Oracle / AAVE / legacy

| Contract                                  | Description                                                                                                                                                              |
| ----------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `MockBlockHeaderOracle.sol`               | Test oracle. Owner can `setBlockHash()` manually; falls back to `blockhash()` for recent blocks.                                                                          |
| `AxiomBlockHeaderOracle.sol`              | Production oracle. Integrates with [Axiom V2](https://www.axiom.xyz/) for trustless block hash verification.                                                              |
| `IAavePool.sol` / `IWrappedTokenGatewayV3.sol` / `IERC20.sol` | Minimal AAVE V3 interfaces for the bridge's optional yield path.                                                                                       |
| `Halo2Verifier.sol` / `Blake2bHalo2Verifier.sol` / `Blake2bTranscript.sol` / `Blake2bChallengeComputer.sol` | Bare Halo2 Yul verifiers (Keccak / Blake2b transcripts). Exceed EIP-170 in a single deployment but exercised in tests for sanity coverage of the underlying Halo2 circuits. |

## Testing

### Test Coverage Summary

| Suite                                                       | Count                | Command                                                              |
| ----------------------------------------------------------- | -------------------- | -------------------------------------------------------------------- |
| Solidity (Foundry) — 12 suites                              | 109                  | `cd contracts/ethereum && forge test`                                |
| Rust workspace (`eth-frontend`, `acki-nacki-interface`)     | small unit suite     | `cargo test --workspace`                                             |
| `bridge-relayer-daemon` unit tests                          | 13                   | `cd crates/bridge-relayer-daemon && cargo test`                      |
| `bridge-prover-orchestrator` round-trip tests               | several              | `cd crates/bridge-prover-orchestrator && cargo test`                 |
| `deposit-prover` lib tests                                  | (depends on Ethereum RPC) | `cd deposit-prover && cargo test`                              |

### Foundry suites (current)

| Suite                          | Count | Notes |
| ------------------------------ | ----: | ----- |
| `AckiNackiBridgeAaveTest`      |    20 | AAVE supply / withdraw / yield (owner paths) |
| `AckiNackiBridgeVerifyBlockTest` |  17 | Phase 4 — real bound Primary + Layer-hashes proofs |
| `AckiNackiBridgeRelayerLoopTest` |  6 | Phase 5.1 — 10-block on-chain drive with mocks |
| `AxiomBlockHeaderOracleTest`   |    16 | Oracle constructor + queries |
| `Blake2bHalo2VerifierTest`     |     7 | Blake2b transcript + EIP-152 |
| `KeccakHalo2VerifierTest`      |     1 | Keccak fallback path |
| `Halo2PoseidonVerifierTest`    |     7 | Poseidon preimage |
| `PrimaryVerifierTest`          |     8 | Circuit 1A adapter |
| `FallbackVerifierTest`         |     8 | Circuit 1B adapter |
| `LayerHashesMovementVerifierTest` | 10 | Circuit 2 adapter |
| `FuzzHalo2VerifierTest`        |     6 | Bare Halo2 Yul fuzz |
| `FuzzAckiNackiBridgeDepositTest` |   3 | Deposit-side fuzz |

Run a single suite:

```bash
cd contracts/ethereum
forge test --match-contract LayerHashesMovementVerifierTest -vv
```

### Rust tests

```bash
cargo test --workspace                                # eth-frontend + acki-nacki-interface
cd crates/bridge-relayer-daemon && cargo test         # relayer (13 unit tests)
cd crates/bridge-prover-orchestrator && cargo test    # 4-circuit prover round-trips
cd deposit-prover && cargo test                       # ETH→AN deposit circuit
```

## ZK Proof Details

### ETH → AN deposit (Halo2 Keccak-transcript)

- **Proof system**: Halo2 with KZG polynomial commitments + SHPLONK batching on BN254.
- **Transcript**: Keccak256 (EVM-compatible).
- **Circuit**: `DepositEventCircuitV2` — proves a `Deposit` event by verifying receipt RLP, receipt-trie inclusion, event log signature, event-data extraction, and block-hash binding.
- **Keccak coprocessor**: keccak-heavy operations are delegated to a coprocessor circuit via Poseidon promise commitments (~500× constraint savings per hash).
- **Public inputs** (7): `[depositId, sender, amount, contractAddress, blockHashHigh, blockHashLow, promiseCommit]`.
- **On-chain consumer**: AN side, natively via the future `VERHALO2SHPLONK` TVM opcode (Decision Log 2026-05-17 in `docs/an_partner_integration_plan.md`).

### AN → ETH state (Halo2 Blake2b-transcript + gnark Groth16)

- **Proof system**: Halo2 SHPLONK on BN254, Blake2b transcript (matches AN-side native transcript).
- **Circuits**: 1A (Primary attestation), 1B (Fallback attestation), 2 (Layer-hashes movement). Circuit 3 (BK-set rotation) is pending partner sign-off.
- **gnark wrapper**: Each Halo2 proof is wrapped into a 256-byte Groth16 BN254 proof + an auto-generated `*Groth16VerifierGenerated.sol` (~26 KB source / ~7 KB runtime), keeping the on-chain verifier under EIP-170.
- **Public inputs**:
  - Circuit 1A/1B (4 Fr): `[block_id, bk_set_poseidon, block_seq_no, last_seen_block_seqno]`.
  - Circuit 2 (14 Fr): `[block_id, bk_set_poseidon, num_layers, layer_hash[0..10], prev_max_level_layer_hash]`.
- **On-chain gas**: ~225 k per 1A/1B + ~293 k for Circuit 2 + ~180 k wrapper overhead ≈ 700 k per `verifyBlock` call.

### Poseidon + Blake2b demo (`poseidon-proof/`)

Reference implementation of a Halo2 circuit with a Blake2b Fiat–Shamir transcript and an on-chain `Blake2bHalo2Verifier.sol` consumer using the EIP-152 precompile. Useful as the foundation for the production AN-transcript machinery; see `docs/BLAKE2B_HALO2_VERIFIER.md`.

## Prerequisites

- **Rust** (stable, 1.70+) — main language for all crates.
- **Go** (1.21+) — for the AN→ETH gnark wrappers (Groth16 proof wrapping).
- **Foundry** (`forge`, `cast`, `anvil`) — Solidity development and testing.
- **Node.js** / **Trunk** — only required for the WASM frontend.

### Quick install

```bash
./setup.sh
```

Or manually: install Rust via rustup, Foundry via `foundryup`, and Go from <https://go.dev/doc/install>.

## Building

```bash
./build.sh                                            # Build everything (Rust + Solidity)
./build.sh --test                                     # Build + run tests
./build.sh --all                                      # Build + fmt + clippy + tests

cargo build --workspace                               # Main workspace
cd crates/bridge-prover-orchestrator && cargo build   # AN→ETH 4-circuit prover
cd crates/bridge-relayer-daemon && cargo build        # Relayer
cd deposit-prover && cargo build                      # ETH→AN deposit prover
cd contracts/ethereum && forge build                  # Solidity contracts
```

### gnark wrappers (AN→ETH side, one-time per circuit)

```bash
cd crates/bridge-prover-orchestrator/gnark-wrappers/circuit-2
go build .
./circuit-2 setup ../../proofs/.../halo2_proof.json    # generates Groth16Verifier.sol + keys
./circuit-2 prove ../../proofs/.../halo2_proof.json    # 256-byte Groth16 proof
```

Repeat for `circuit-1a/`, `circuit-1b/`. The generated `Groth16Verifier.sol` is copied to `contracts/ethereum/src/{Primary,Fallback,LayerHashes}Groth16VerifierGenerated.sol`.

## Development

### Code quality

```bash
cargo fmt --all
cargo clippy --workspace
cd contracts/ethereum && forge fmt && forge test -vvv
```

### Docker

```bash
docker-compose up -d
docker-compose exec dev bash
```

### Foundry configuration

Key settings in `contracts/ethereum/foundry.toml`:

- `solc_version = "0.8.19"` — Solidity compiler version.
- `optimizer_runs = 1` — Optimised for deployment size (not runtime gas).
- `via_ir = true` — Required for the larger Yul verifiers (`Halo2Verifier`, `Blake2bHalo2Verifier`).

## Technology Stack

| Component                | Technology                                                                |
| ------------------------ | ------------------------------------------------------------------------- |
| Smart contracts          | Solidity 0.8.19, Foundry                                                  |
| ZK proof system          | Halo2 (KZG + SHPLONK on BN254)                                            |
| AN→ETH wrapper           | gnark Groth16 (Go), per-circuit                                           |
| AN→ETH transcript        | Blake2b (matches AN-side)                                                 |
| ETH→AN transcript        | Keccak256                                                                 |
| AN-side verification     | Native Halo2 SHPLONK via `VERHALO2SHPLONK` TVM opcode (in `tvm-sdk`, WIP) |
| ETH→AN deposit prover    | axiom-eth (Rust)                                                          |
| AN→ETH state prover      | `bridge-prover-orchestrator` (Rust + gosh halo2 fork)                     |
| Poseidon hash            | T=3, RATE=2, R_F=8, R_P=57                                                |
| Block hash oracle        | Axiom V2 (production), `blockhash()` (recent)                             |
| Ethereum client          | ethers-rs                                                                 |
| Relayer                  | `bridge-relayer-daemon` crate (`Relayer::tick()` / `run_loop()`)          |
| AAVE yield integration   | AAVE V3 mainnet (optional, owner-managed)                                 |
| Frontend                 | Yew + WebAssembly                                                         |
| Testing                  | Foundry (Solidity), `cargo test` (Rust)                                   |

## Further reading

- `docs/an_partner_integration_plan.md` — Active integration plan + Decision Log + risk register.
- `docs/four_circuit_architecture.md` — Per-circuit architecture deep-dive.
- `docs/bridge_verification.md` — Property-driven verification reference (DEP-#, LH-#, BK-#, OR-#, AC-#, FORK-#).
- `docs/manual_verification_runbook.md` — Hands-on, copy-pasteable manual review plan.
- `docs/aave_integration.md` — AAVE V3 yield integration design.
- `docs/verifying_an_proof.md` — End-to-end verification of an AN-side layer-hash proof.
- `docs/verifying_eth_proof_on_an.md` — End-to-end verification of an ETH-side deposit proof on the AN side.
- `docs/audit_trail_v2.md` — Cumulative audit trail.

## License

MIT
