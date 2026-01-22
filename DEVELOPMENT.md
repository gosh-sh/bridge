# Development Guide

This guide provides detailed information for developers working on the Acki Nacki Bridge.

## Table of Contents

- [Prerequisites](#prerequisites)
- [Setup](#setup)
- [Project Structure](#project-structure)
- [Development Workflow](#development-workflow)
- [Testing](#testing)
- [Building](#building)
- [Code Style](#code-style)
- [Debugging](#debugging)
- [Common Tasks](#common-tasks)

## Prerequisites

### Required Tools

- **Rust** (nightly): Latest nightly version
- **Foundry**: For Solidity development
  - `forge`: Solidity compiler and test runner
  - `cast`: Ethereum CLI tool
  - `anvil`: Local Ethereum node
- **Git**: Version control

### Optional Tools

- **Docker**: For containerized development
- **Node.js**: For some tooling (optional)
- **cargo-nextest**: Better test runner
- **cargo-watch**: Auto-rebuild on changes
- **cargo-audit**: Security auditing

## Setup

### Quick Setup

```bash
# Clone the repository
git clone https://vcs.modus-ponens.com/ton/acki-nacki-bridge.git
cd acki-nacki-bridge

# Run setup script (installs all dependencies)
chmod +x setup.sh
./setup.sh

# Or use Make
make setup
```

### Manual Setup

If you prefer to set up manually:

```bash
# Install Rust nightly
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup default nightly
rustup component add rustfmt clippy

# Install Foundry
curl -L https://foundry.paradigm.xyz | bash
foundryup

# Install optional tools
cargo install cargo-nextest cargo-watch cargo-audit

# Initialize Foundry project
cd contracts/ethereum
forge install
cd ../..

# Build the project
cargo build --workspace
```

## Project Structure

```
acki-nacki-bridge/
├── crates/
│   ├── crypto/              # Cryptographic primitives (Poseidon, etc.)
│   ├── merkle-tree/         # Incremental Merkle tree implementation
│   ├── zk-proofs/           # PLONK proof generation and verification
│   ├── acki-nacki-interface/# Interface and mocks for Acki Nacki
│   └── eth-frontend/        # Ethereum frontend application
├── contracts/
│   └── ethereum/            # Solidity smart contracts
│       ├── src/             # Contract source files
│       ├── test/            # Contract tests
│       └── script/          # Deployment scripts
├── test/
│   └── integration/         # Integration tests
├── docs/                    # Documentation
├── Cargo.toml               # Rust workspace configuration
├── Makefile                 # Convenience commands
├── setup.sh                 # Setup script
├── build.sh                 # Build script
└── test.sh                  # Test script
```

## Development Workflow

### Using Make (Recommended)

```bash
# Show all available commands
make help

# Build the project
make build

# Run tests
make test

# Format code
make format

# Run linter
make lint

# Run all checks
make check

# Watch for changes
make watch
```

### Using Scripts Directly

```bash
# Build
./build.sh                    # Debug build
./build.sh --release          # Release build
./build.sh --all              # Build + format + lint + test

# Test
./test.sh                     # All tests
./test.sh --rust              # Rust tests only
./test.sh --solidity          # Solidity tests only
./test.sh --verbose           # Verbose output
./test.sh --coverage          # With coverage
```

### Using Cargo Directly

```bash
# Build
cargo build --workspace
cargo build --release --workspace

# Test
cargo test --workspace
cargo nextest run --workspace  # If nextest is installed

# Check without building
cargo check --workspace

# Format
cargo fmt --all

# Lint
cargo clippy --all-targets --all-features -- -D warnings
```

### Using Forge Directly

```bash
cd contracts/ethereum

# Build contracts
forge build

# Run tests
forge test
forge test -vvv              # Verbose
forge test --match-test testDeposit  # Specific test

# Coverage
forge coverage

# Format
forge fmt
```

## Testing

### Test Organization

Tests are organized into three categories:

1. **Unit Tests**: Test individual components in isolation
2. **Integration Tests**: Test interactions between components
3. **Property Tests**: Test invariants using property-based testing

### Running Tests

```bash
# All tests
make test

# Rust unit tests only
cargo test --workspace --lib

# Rust integration tests only
cargo test --workspace --test '*'

# Solidity tests
cd contracts/ethereum && forge test

# With coverage
make test-coverage

# Specific test
cargo test test_merkle_tree
forge test --match-test testDeposit
```

### Writing Tests

#### Rust Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_something() {
        // Test code
    }

    #[test]
    #[should_panic(expected = "error message")]
    fn test_error_case() {
        // Test code that should panic
    }
}
```

#### Solidity Tests

```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import "forge-std/Test.sol";
import "../src/Bridge.sol";

contract BridgeTest is Test {
    Bridge bridge;

    function setUp() public {
        bridge = new Bridge();
    }

    function testDeposit() public {
        // Test code
    }
}
```

## Building

### Debug Build

```bash
make build
# or
cargo build --workspace
```

### Release Build

```bash
make build-release
# or
cargo build --release --workspace
```

### Clean Build

```bash
make clean
# or
cargo clean && cd contracts/ethereum && forge clean
```

## Code Style

### Rust

We follow the standard Rust style guide:

- Use `rustfmt` for formatting
- Use `clippy` for linting
- Maximum line length: 100 characters
- Use meaningful variable names
- Add documentation comments for public APIs

```bash
# Format code
cargo fmt --all

# Check formatting
cargo fmt --all -- --check

# Run clippy
cargo clippy --all-targets --all-features -- -D warnings
```

### Solidity

We follow the Solidity style guide:

- Use `forge fmt` for formatting
- Maximum line length: 100 characters
- Use NatSpec comments for documentation

```bash
# Format code
cd contracts/ethereum && forge fmt

# Check formatting
cd contracts/ethereum && forge fmt --check
```

## Debugging

### Rust Debugging

```bash
# Run with debug output
RUST_LOG=debug cargo run

# Run with backtrace
RUST_BACKTRACE=1 cargo test

# Use rust-gdb or rust-lldb
rust-gdb target/debug/eth-frontend
```

### Solidity Debugging

```bash
# Verbose test output
forge test -vvvv

# Trace specific transaction
forge test --match-test testDeposit -vvvv

# Use console.log in contracts
import "forge-std/console.sol";
console.log("Debug message", value);
```

## Common Tasks

### Adding a New Dependency

#### Rust

```bash
# Add to workspace dependencies in root Cargo.toml
# Then reference in crate's Cargo.toml
```

#### Solidity

```bash
cd contracts/ethereum
forge install <org>/<repo>
```

### Running Local Ethereum Node

```bash
# Using Anvil
anvil

# Or using Docker
docker-compose up anvil
```

### Deploying Contracts Locally

```bash
cd contracts/ethereum
forge script script/Deploy.s.sol --rpc-url http://localhost:8545 --broadcast
```

### Generating Documentation

```bash
# Rust docs
cargo doc --workspace --no-deps --open

# Solidity docs
cd contracts/ethereum && forge doc
```

### Running Security Audit

```bash
# Rust
cargo audit

# Solidity (requires Slither)
cd contracts/ethereum
slither .
```

## Troubleshooting

### Common Issues

1. **Build fails with missing dependencies**
   ```bash
   cargo clean
   cargo fetch
   cargo build
   ```

2. **Foundry not found**
   ```bash
   export PATH="$HOME/.foundry/bin:$PATH"
   ```

3. **Tests fail intermittently**
   ```bash
   # Run tests sequentially
   cargo test -- --test-threads=1
   ```

## Getting Help

- Check the [README.md](README.md) for general information
- Review the [Architecture Documentation](docs/ARCHITECTURE.md)
- Open an issue on GitLab
- Contact the team

