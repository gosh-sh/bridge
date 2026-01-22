# Acki Nacki Bridge

A secure, privacy-preserving bridge between Ethereum and Acki Nacki blockchain using zero-knowledge proofs.

## Overview

This project implements a 1-to-1 token bridge between Ethereum and Acki Nacki blockchain with the following features:

- **Privacy-preserving deposits**: Uses commitments and Merkle trees (Tornado Cash style)
- **Zero-knowledge proofs**: Halo2/PLONK proofs for withdrawal authorization
- **Poseidon hash**: ZK-friendly hash function for efficient proof generation
- **Incremental Merkle tree**: Efficient on-chain storage and proof generation
- **Nullifier system**: Prevents double-spending

## Architecture

The bridge consists of four main components:

1. **Ethereum Frontend** (`crates/eth-frontend`): Rust client for interacting with Ethereum
2. **Ethereum Contract** (`contracts/ethereum`): Solidity smart contract managing deposits/withdrawals
3. **Acki Nacki Frontend**: Rust client for Acki Nacki (interface provided)
4. **Acki Nacki Contract**: Smart contract on Acki Nacki side (to be implemented by Acki Nacki team)

### Core Crates

- **crypto** (`crates/crypto`): Cryptographic primitives (Poseidon hash, commitments, field elements)
- **merkle-tree** (`crates/merkle-tree`): Incremental Merkle tree implementation
- **zk-proofs** (`crates/zk-proofs`): Halo2 circuits for deposit and withdrawal proofs
- **acki-nacki-interface** (`crates/acki-nacki-interface`): Interface for Acki Nacki integration

### Smart Contracts

- **AckiNackiBridge** (`contracts/ethereum/src/AckiNackiBridge.sol`): Main bridge contract
- **IAckiNackiVerifier** (`contracts/ethereum/src/IAckiNackiVerifier.sol`): Verifier interface
- **DummyVerifier** (`contracts/ethereum/src/DummyVerifier.sol`): Testing verifier implementation

The bridge uses a modular verifier architecture where ZK proof verification is separated into its own contract. This allows for easy testing with a dummy verifier and future upgrades to the real Halo2 verifier. See `contracts/ethereum/VERIFIER.md` for details.

**Poseidon Hash**: Both Rust and Solidity implementations use PoseidonT3 (width 3, rate 2)
for ZK-friendly hashing. See `docs/POSEIDON_INTEGRATION.md` for compatibility details.

## Quick Start

### Prerequisites

- Rust (latest stable or nightly)
- Foundry (for Solidity development and testing)

### Installation

Run the setup script to install all dependencies:

```bash
./setup.sh
```

This will install:
- Rust toolchain with rustfmt and clippy
- Foundry (forge, cast, anvil, chisel)
- Additional Rust tools (cargo-nextest, cargo-watch, cargo-audit)
- OpenZeppelin contracts
- Git hooks for pre-commit checks

### Building

Build the entire project (Rust + Solidity):

```bash
./build.sh
```

Build with tests:

```bash
./build.sh --test
```

Build with all checks (format, clippy, tests):

```bash
./build.sh --all
```

### Testing

#### Unit Tests

Run all Rust unit tests:

```bash
cargo test --workspace
```

Run Solidity tests:

```bash
cd contracts/ethereum
forge test
```

#### Integration Tests

Integration tests require a local Ethereum node (Anvil).

1. Start Anvil in a separate terminal:
   ```bash
   anvil
   ```

2. Deploy the contract (see `crates/eth-frontend/tests/README.md` for details)

3. Run integration tests:
   ```bash
   cargo test --package eth-frontend --test integration_test -- --ignored
   ```

## Deposit Flow

1. User generates two random secrets: `withdrawal_hash` and `nullifier`
2. Compute commitment: `commitment = Poseidon(withdrawal_hash, nullifier)`
3. Send deposit transaction to Ethereum contract with commitment and ETH amount
4. Contract adds commitment to Merkle tree and emits event
5. Frontend generates ZK proof of knowledge and sends to Acki Nacki contract
6. User receives equivalent tokens on Acki Nacki

## Withdrawal Flow

1. User initiates withdrawal on Acki Nacki frontend
2. Frontend generates ZK proof that proves:
   - Knowledge of `withdrawal_hash` and `nullifier`
   - The commitment exists in the Merkle tree
   - The nullifier hasn't been used before
3. Submit proof to Ethereum contract
4. Contract verifies proof, marks nullifier as used, and transfers ETH to recipient

## Test Coverage

### Rust Tests
- **crypto**: 16 tests (Poseidon hash, commitments, field elements)
- **merkle-tree**: 29 tests (incremental tree, proofs, serialization)
- **zk-proofs**: 14 tests (deposit/withdrawal circuits, proof generation)
- **eth-frontend**: 9 unit tests + 7 integration tests

### Solidity Tests
- **AckiNackiBridge**: 19 tests (deposits, withdrawals, Merkle tree, nullifiers, verifier integration)

## Development

### Code Quality

Format code:
```bash
./build.sh --format
```

Run linter:
```bash
./build.sh --clippy
```

### Continuous Integration

The project uses GitLab CI/CD for automated testing and deployment. See `docs/CI_SETUP.md` for details.

Key features:
- Automated Rust and Solidity builds
- Comprehensive test suite (unit, integration, doc tests)
- Code quality checks (rustfmt, clippy, forge fmt)
- Security audits (cargo audit, slither)
- Automatic npm dependency installation for Poseidon library

### Continuous Development

Auto-rebuild on changes:
```bash
cargo watch -x check
```

### Security

Run security audit:
```bash
cargo audit
```

## Project Structure

```
acki-nacki-bridge/
├── crates/
│   ├── crypto/              # Cryptographic primitives
│   ├── merkle-tree/         # Incremental Merkle tree
│   ├── zk-proofs/           # Halo2 circuits
│   ├── eth-frontend/        # Ethereum client
│   └── acki-nacki-interface/# Acki Nacki interface
├── contracts/
│   └── ethereum/            # Solidity contracts
│       ├── src/             # Contract source
│       └── test/            # Contract tests
├── build.sh                 # Build script
├── setup.sh                 # Setup script
└── README.md
```

## Technology Stack

- **Rust**: Core implementation language
- **Solidity**: Ethereum smart contracts
- **Halo2**: Zero-knowledge proof system
- **Poseidon**: ZK-friendly hash function
- **Foundry**: Solidity development framework
- **Ethers-rs**: Ethereum library for Rust

## License

MIT

## Contributing

Contributions are welcome! Please ensure:
- All tests pass (`./build.sh --test`)
- Code is formatted (`./build.sh --format`)
- Clippy checks pass (`./build.sh --clippy`)
- New features include tests
