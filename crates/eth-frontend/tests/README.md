# Integration Tests

This directory contains integration tests that test the Ethereum frontend with actual smart contracts.

## Prerequisites

1. **Install Foundry** (includes Anvil for local Ethereum node):
   ```bash
   curl -L https://foundry.paradigm.xyz | bash
   foundryup
   ```

2. **Deploy the smart contract**:
   ```bash
   cd contracts/ethereum
   forge build
   ```

## Running Integration Tests

### Step 1: Start Local Ethereum Node

In a separate terminal, start Anvil (local Ethereum node):

```bash
anvil
```

This will start a local Ethereum node at `http://localhost:8545` with pre-funded test accounts.

### Step 2: Deploy the Contracts

First, deploy the DummyVerifier contract:

```bash
cd contracts/ethereum
forge create --rpc-url http://localhost:8545 \
  --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 \
  src/DummyVerifier.sol:DummyVerifier
```

Note the deployed verifier address from the output.

Then, deploy the AckiNackiBridge contract with the verifier address:

```bash
forge create --rpc-url http://localhost:8545 \
  --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 \
  --constructor-args <VERIFIER_ADDRESS> \
  src/AckiNackiBridge.sol:AckiNackiBridge
```

Replace `<VERIFIER_ADDRESS>` with the address from the previous step.

Note the deployed bridge contract address from the output.

### Step 3: Update Test Configuration

Update the `contract_address` in `integration_test.rs` with the deployed address.

### Step 4: Run Tests

Run the integration tests (they are ignored by default):

```bash
cargo test --package eth-frontend --test integration_test -- --ignored
```

Or run a specific test:

```bash
cargo test --package eth-frontend --test integration_test test_deposit_integration -- --ignored
```

## Test Coverage

The integration tests cover:

- **Deposit Flow**: Making deposits and verifying state changes
- **Multiple Deposits**: Testing sequential deposits
- **Merkle Root Changes**: Verifying root updates after deposits
- **Withdrawal Flow**: Complete withdrawal with proof verification
- **Double-Spend Prevention**: Ensuring nullifiers prevent reuse
- **Invalid Root Rejection**: Verifying wrong roots are rejected
- **Empty Proof Rejection**: Ensuring proofs are required

## Notes

- Tests are marked with `#[ignore]` by default since they require a running Ethereum node
- The tests use the first Anvil account (private key: `0xac0974...`)
- Each test assumes a fresh contract deployment or proper cleanup between tests
- For CI/CD, you can automate Anvil startup and contract deployment

## Troubleshooting

**Connection refused**: Make sure Anvil is running on `http://localhost:8545`

**Contract not found**: Ensure the contract is deployed and the address is correct

**Transaction reverted**: Check that the account has enough ETH and the contract state is as expected

