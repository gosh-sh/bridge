# Testing Guide

This document describes how to run tests for the Acki Nacki Bridge project.

## Quick Start

Run all tests (Rust + Solidity):
```bash
make test
```

Or using the test script:
```bash
./test.sh
```

## Test Summary

### ✅ Rust Tests: 77 unit tests
- **acki_nacki_interface**: 9 tests
- **crypto**: 16 tests (Poseidon hash, commitments, field elements)
- **eth-frontend**: 9 unit tests
- **merkle-tree**: 29 tests (incremental tree, proofs, serialization)
- **zk-proofs**: 14 tests (deposit/withdrawal circuits, proof generation)

### ✅ Solidity Tests: 13 tests
- Deposit functionality (single and multiple)
- Invalid amount handling
- Merkle root changes
- Withdrawal with ZK proof
- Double-spend prevention
- Invalid root rejection
- Insufficient treasury handling
- Empty proof rejection
- Hash function properties
- Tree capacity limits

### 📋 Integration Tests: 7 tests (require Anvil)
- Deposit integration
- Multiple deposits integration
- Merkle root changes integration
- Withdrawal integration
- Double-spend prevention integration
- Invalid root rejection integration
- Empty proof rejection integration

## Running Tests

### All Tests

Run all tests (Rust + Solidity):
```bash
make test
```

### Rust Tests Only

```bash
make test-rust
```

Or:
```bash
cargo test --workspace
```

### Solidity Tests Only

```bash
make test-solidity
```

Or:
```bash
cd contracts/ethereum
forge test
```

### Integration Tests

Integration tests require a local Ethereum node (Anvil) to be running.

**Step 1**: Start Anvil in a separate terminal:
```bash
make run-local
```

Or:
```bash
anvil
```

**Step 2**: Run integration tests:
```bash
make test-integration
```

Or:
```bash
cargo test --package eth-frontend --test integration_test -- --ignored
```

See `crates/eth-frontend/tests/README.md` for detailed setup instructions.

### Verbose Output

Run tests with verbose output:
```bash
./test.sh --verbose
```

Or for Rust only:
```bash
cargo test --workspace -- --nocapture
```

Or for Solidity only:
```bash
cd contracts/ethereum
forge test -vvv
```

### Filter Tests

Run tests matching a pattern:
```bash
./test.sh --filter merkle
```

Or for Rust:
```bash
cargo test merkle
```

Or for Solidity:
```bash
cd contracts/ethereum
forge test --match-test Deposit
```

### Coverage

Generate test coverage report:
```bash
make test-coverage
```

Or:
```bash
./test.sh --coverage
```

For Solidity coverage:
```bash
cd contracts/ethereum
forge coverage
```

## Test Organization

### Rust Tests

```
crates/
├── acki-nacki-interface/
│   └── src/
│       ├── mock.rs (tests)
│       ├── types.rs (tests)
│       └── lib.rs (tests)
├── crypto/
│   └── src/
│       ├── poseidon.rs (tests)
│       ├── random.rs (tests)
│       ├── types.rs (tests)
│       └── lib.rs (tests)
├── eth-frontend/
│   ├── src/
│   │   └── tests.rs (unit tests)
│   └── tests/
│       └── integration_test.rs (integration tests, ignored by default)
├── merkle-tree/
│   └── src/
│       ├── proof.rs (tests)
│       ├── tree.rs (tests)
│       └── lib.rs (tests)
└── zk-proofs/
    └── src/
        ├── deposit.rs (tests)
        ├── withdrawal.rs (tests)
        ├── burn_proof.rs (tests)
        └── lib.rs (tests)
```

### Solidity Tests

```
contracts/ethereum/
└── test/
    └── AckiNackiBridge.t.sol (all contract tests)
```

## Continuous Integration

Run all CI checks locally:
```bash
make ci
```

This runs:
1. Format check (`cargo fmt --check` + `forge fmt --check`)
2. Linter (`cargo clippy`)
3. All tests (`cargo test` + `forge test`)

## Test Performance

Some tests may take longer to run:

- **merkle-tree tests**: ~250 seconds (includes property-based tests)
- **zk-proofs tests**: ~3 seconds (proof generation)
- **Other tests**: < 1 second each

To run only fast tests, exclude the slow ones:
```bash
cargo test --workspace --lib
```

## Troubleshooting

### Foundry Not Found

If you get "forge: command not found":
```bash
# Install Foundry
curl -L https://foundry.paradigm.xyz | bash
foundryup

# Or run setup
make setup
```

### Integration Tests Fail

If integration tests fail with "connection refused":
1. Make sure Anvil is running: `anvil`
2. Check that it's listening on `http://localhost:8545`
3. Verify the contract is deployed (see `crates/eth-frontend/tests/README.md`)

### Tests Timeout

If tests timeout, increase the timeout or run with fewer threads:
```bash
cargo test --workspace -- --test-threads=1
```

### Compilation Errors

If you get compilation errors:
```bash
# Clean and rebuild
make clean
make build
```

## Writing New Tests

### Rust Unit Tests

Add tests in the same file as the code:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_something() {
        // Test code
    }
}
```

### Rust Integration Tests

Add tests in `crates/<package>/tests/`:
```rust
#[test]
fn test_integration() {
    // Test code
}

// For tests requiring Anvil
#[test]
#[ignore]
fn test_with_anvil() {
    // Test code
}
```

### Solidity Tests

Add tests in `contracts/ethereum/test/`:
```solidity
import "forge-std/Test.sol";

contract MyTest is Test {
    function testSomething() public {
        // Test code
    }
}
```

## Best Practices

1. **Run tests before committing**: `make ci`
2. **Write tests for new features**: Aim for high coverage
3. **Use descriptive test names**: `test_deposit_with_invalid_amount`
4. **Test edge cases**: Empty inputs, maximum values, etc.
5. **Keep tests fast**: Mock external dependencies
6. **Use integration tests sparingly**: They're slower and require setup

## Resources

- [Rust Testing Guide](https://doc.rust-lang.org/book/ch11-00-testing.html)
- [Foundry Book](https://book.getfoundry.sh/forge/tests)
- [Cargo Test Documentation](https://doc.rust-lang.org/cargo/commands/cargo-test.html)
- [Forge Test Documentation](https://book.getfoundry.sh/reference/forge/forge-test)

