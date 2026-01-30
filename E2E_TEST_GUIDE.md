# End-to-End Test Guide

This guide explains how to run the complete end-to-end test for the Acki Nacki Bridge.

## Overview

The end-to-end test validates the complete bridge workflow:

1. **Deploy** - Deploy bridge and verifier contracts to Sepolia testnet
2. **Deposit** - Make a test deposit to the bridge
3. **Fetch** - Fetch deposit event data and generate MPT proof
4. **Verify** - Test the circuit with MockProver
5. **Prove** - Generate a ZK SNARK proof
6. **Withdraw** - Submit withdrawal transaction with the proof

## Prerequisites

### 1. Environment Setup

Ensure you have filled in the `.env` file in `contracts/ethereum/`:

```bash
# Ethereum RPC URLs
SEPOLIA_RPC_URL=https://eth-sepolia.g.alchemy.com/v2/YOUR_API_KEY

# Private key for deployment (DO NOT COMMIT!)
PRIVATE_KEY=0x...

# Etherscan API key for contract verification
ETHERSCAN_API_KEY=YOUR_API_KEY
```

### 2. Required Tools

- **Rust** (1.70+) - For proof generation
- **Foundry** (forge, cast) - For contract deployment
- **jq** - For JSON parsing
- **curl** - For RPC calls

Install Foundry:
```bash
curl -L https://foundry.paradigm.xyz | bash
foundryup
```

Install jq:
```bash
# Ubuntu/Debian
sudo apt-get install jq

# macOS
brew install jq
```

### 3. Sepolia Testnet ETH

You need Sepolia ETH for:
- Deploying contracts (~0.01 ETH)
- Making test deposits (0.1 ETH per test)
- Submitting withdrawals (~0.001 ETH)

Get Sepolia ETH from faucets:
- https://sepoliafaucet.com/
- https://www.alchemy.com/faucets/ethereum-sepolia
- https://faucet.quicknode.com/ethereum/sepolia

## Running the Test

### Quick Start

Simply run the test script:

```bash
./test_e2e.sh
```

The script will:
1. Deploy contracts (or reuse existing deployment)
2. Make a 0.1 ETH deposit
3. Fetch event data and MPT proof
4. Test circuit with MockProver
5. Generate ZK proof (takes 5-10 minutes)
6. Submit withdrawal transaction

### Expected Output

```
╔════════════════════════════════════════════════════════════╗
║  Acki Nacki Bridge - End-to-End Test                      ║
╚════════════════════════════════════════════════════════════╝

[1/6] Deploying Bridge Contract to Sepolia...
✓ Contracts deployed successfully!
  Bridge: 0x...
  Verifier: 0x...

[2/6] Making Test Deposit...
✓ Deposit transaction sent!
  TX Hash: 0x...
✓ Deposit confirmed!
  Deposit ID: 0

[3/6] Fetching Deposit Event Data...
✓ Event data fetched successfully!

[4/6] Testing Circuit with MockProver...
✓ Circuit test passed!

[5/6] Generating ZK Proof...
Generating SNARK proof (this may take several minutes)...
✓ Proof generated successfully!

[6/6] Submitting Withdrawal Transaction...
✓ Withdrawal transaction sent!
✓ Withdrawal successful!

╔════════════════════════════════════════════════════════════╗
║  End-to-End Test PASSED! ✓                                ║
╚════════════════════════════════════════════════════════════╝
```

## Test Data

All test data is saved to `e2e_test_data/`:

- `deployment.json` - Contract addresses
- `deposit_info.json` - Deposit transaction details
- `deposit_proof_input.json` - Event data and MPT proof
- `deposit_proof_output.json` - Generated ZK proof

## Manual Testing

You can also run each step manually:

### 1. Deploy Contracts

```bash
cd contracts/ethereum
source .env

forge script script/DeployTestBridge.s.sol:DeployTestBridge \
  --rpc-url $SEPOLIA_RPC_URL \
  --broadcast \
  --private-key $PRIVATE_KEY
```

### 2. Make a Deposit

```bash
BRIDGE_ADDRESS="0x..."  # From deployment
ACKI_ADDRESS="100000000000000000"  # Placeholder

cast send $BRIDGE_ADDRESS \
  "deposit(uint256)" $ACKI_ADDRESS \
  --value 0.1ether \
  --rpc-url $SEPOLIA_RPC_URL \
  --private-key $PRIVATE_KEY
```

### 3. Fetch Event Data

```bash
cd deposit-prover

TX_HASH="0x..."  # From deposit transaction

cargo run --release --example fetch_deposit_data -- \
  --tx-hash $TX_HASH \
  --contract $BRIDGE_ADDRESS \
  --rpc-url $SEPOLIA_RPC_URL \
  --output deposit_proof_input.json
```

### 4. Test Circuit

```bash
cargo run --release --example test_with_real_data -- \
  --input deposit_proof_input.json \
  --mock-only
```

### 5. Generate Proof

```bash
cargo run --release --example test_with_real_data -- \
  --input deposit_proof_input.json \
  --generate-proof \
  --output deposit_proof_output.json
```

### 6. Submit Withdrawal

```bash
cd ../contracts/ethereum

DEPOSIT_ID=0  # From deposit event
SENDER_ADDRESS=$(cast wallet address $PRIVATE_KEY)
PROOF_BYTES=$(jq -r '.proof_bytes' ../../deposit-prover/deposit_proof_output.json)

cast send $BRIDGE_ADDRESS \
  "withdraw(address,uint256,uint256,bytes)" \
  $SENDER_ADDRESS \
  100000000000000000 \
  $DEPOSIT_ID \
  $PROOF_BYTES \
  --rpc-url $SEPOLIA_RPC_URL \
  --private-key $PRIVATE_KEY
```

## Troubleshooting

### "Failed to fetch deposit event data"

- Check that the transaction was mined (wait 10-15 seconds)
- Verify the RPC URL is correct and accessible
- Ensure the contract address matches the deployed bridge

### "Circuit test failed"

- Check that the MPT proof is valid
- Verify the event data matches the transaction
- Ensure you're using the correct block number

### "Proof generation failed"

- Ensure you have enough RAM (16GB+ recommended)
- Check that KZG parameters are downloaded
- Verify the circuit configuration matches the input

### "Withdrawal transaction failed"

- Check that the deposit hasn't been processed already
- Verify the proof is valid
- Ensure the bridge has sufficient balance
- Check that the deposit ID matches

## Performance Notes

- **MockProver test**: ~10 seconds
- **Proof generation**: 5-10 minutes (depends on hardware)
- **Transaction confirmation**: 10-15 seconds on Sepolia

## Security Notes

⚠️ **WARNING**: The test deployment uses a `TestDepositVerifier` that accepts any proof with valid format. This is **NOT SECURE** and is only for testing purposes.

For production:
1. Generate the real Halo2 verifier contract
2. Deploy the production verifier
3. Update the bridge to use the production verifier

## Next Steps

After successful end-to-end testing:

1. **Generate Production Verifier**:
   ```bash
   cd deposit-prover
   cargo run --release --example generate_verifier
   ```

2. **Deploy Production Bridge**:
   - Update deployment script to use real verifier
   - Deploy to mainnet or production testnet
   - Verify contracts on Etherscan

3. **Frontend Integration**:
   - Update frontend config with deployed addresses
   - Test deposit/withdrawal flow through UI
   - Deploy frontend to production

## Support

If you encounter issues:

1. Check the logs in `e2e_test_data/`
2. Verify all prerequisites are installed
3. Ensure you have sufficient Sepolia ETH
4. Review the transaction on Etherscan for error details

## References

- [Deployment Guide](contracts/ethereum/DEPLOYMENT_GUIDE.md)
- [Circuit Design](deposit-prover/CIRCUIT_DESIGN.md)
- [Integration Testing](deposit-prover/INTEGRATION_TESTING.md)
- [Frontend Guide](frontend/README.md)

