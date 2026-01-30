# Acki Nacki Bridge - End-to-End Testing

## Quick Start

```bash
# 1. Check prerequisites
./check_e2e_prerequisites.sh

# 2. Run the end-to-end test
./test_e2e.sh
```

That's it! The script will automatically:
- Deploy contracts to Sepolia
- Make a test deposit
- Generate ZK proof
- Submit withdrawal
- Verify success

## What Gets Tested

The end-to-end test validates the complete bridge workflow:

```
┌──────────────┐     ┌──────────────┐     ┌──────────────┐
│   Ethereum   │────▶│  ZK Prover   │────▶│   Ethereum   │
│   (Deposit)  │     │  (Generate)  │     │ (Withdrawal) │
└──────────────┘     └──────────────┘     └──────────────┘
       │                     │                     │
       ▼                     ▼                     ▼
  Emit Event          Prove Event            Verify Proof
  Store ETH           Create SNARK           Release ETH
```

### Test Steps

1. **Deploy** - Deploy bridge and verifier contracts to Sepolia
2. **Deposit** - Send 0.1 ETH to the bridge
3. **Fetch** - Get deposit event data and MPT proof from Ethereum
4. **Verify** - Test circuit with MockProver (fast validation)
5. **Prove** - Generate ZK SNARK proof (5-10 minutes)
6. **Withdraw** - Submit withdrawal transaction with proof

## Prerequisites

### Required Tools

- ✅ Rust 1.70+ (`rustc --version`)
- ✅ Foundry (`forge --version`)
- ✅ jq (`jq --version`)
- ✅ curl

### Environment Setup

Create `contracts/ethereum/.env`:

```bash
SEPOLIA_RPC_URL=https://eth-sepolia.g.alchemy.com/v2/YOUR_API_KEY
PRIVATE_KEY=0x...
ETHERSCAN_API_KEY=YOUR_API_KEY
```

### Sepolia ETH

You need ~0.15 ETH on Sepolia testnet:
- 0.01 ETH for deployment
- 0.1 ETH for test deposit
- 0.04 ETH buffer for gas

Get Sepolia ETH from:
- https://sepoliafaucet.com/
- https://www.alchemy.com/faucets/ethereum-sepolia
- https://faucet.quicknode.com/ethereum/sepolia

## Running the Test

### Full Test (Recommended)

```bash
./test_e2e.sh
```

Expected time: **6-11 minutes**
- Contract deployment: ~30s
- Deposit: ~15s
- Fetch data: ~5s
- MockProver: ~10s
- **Proof generation: 5-10 minutes** ⏰
- Withdrawal: ~15s

### Check Prerequisites First

```bash
./check_e2e_prerequisites.sh
```

This validates:
- All tools are installed
- .env file is configured
- RPC connection works
- Wallet has sufficient balance
- System has enough RAM/disk

## Test Output

### Console Output

```
╔════════════════════════════════════════════════════════════╗
║  Acki Nacki Bridge - End-to-End Test                      ║
╚════════════════════════════════════════════════════════════╝

[1/6] Deploying Bridge Contract to Sepolia...
✓ Contracts deployed successfully!
  Bridge: 0x...
  Verifier: 0x...

[2/6] Making Test Deposit...
✓ Deposit confirmed!
  Deposit ID: 0

[3/6] Fetching Deposit Event Data...
✓ Event data fetched successfully!

[4/6] Testing Circuit with MockProver...
✓ Circuit test passed!

[5/6] Generating ZK Proof...
✓ Proof generated successfully!

[6/6] Submitting Withdrawal Transaction...
✓ Withdrawal successful!

╔════════════════════════════════════════════════════════════╗
║  End-to-End Test PASSED! ✓                                ║
╚════════════════════════════════════════════════════════════╝
```

### Test Data Files

All data saved to `e2e_test_data/`:

```
e2e_test_data/
├── deployment.json           # Contract addresses
├── deposit_info.json         # Deposit transaction details
├── deposit_proof_input.json  # Event data + MPT proof
└── deposit_proof_output.json # Generated ZK proof
```

## Manual Testing

You can also run each step individually:

### 1. Deploy Contracts

```bash
cd contracts/ethereum
source .env

forge script script/DeployTestBridge.s.sol:DeployTestBridge \
  --rpc-url $SEPOLIA_RPC_URL \
  --broadcast \
  --private-key $PRIVATE_KEY
```

### 2. Make Deposit

```bash
BRIDGE_ADDRESS="0x..."
cast send $BRIDGE_ADDRESS \
  "deposit(uint256)" 100000000000000000 \
  --value 0.1ether \
  --rpc-url $SEPOLIA_RPC_URL \
  --private-key $PRIVATE_KEY
```

### 3. Fetch Event Data

```bash
cd deposit-prover
TX_HASH="0x..."

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
SENDER=$(cast wallet address $PRIVATE_KEY)
PROOF=$(jq -r '.proof_bytes' ../../deposit-prover/deposit_proof_output.json)

cast send $BRIDGE_ADDRESS \
  "withdraw(address,uint256,uint256,bytes)" \
  $SENDER \
  100000000000000000 \
  0 \
  $PROOF \
  --rpc-url $SEPOLIA_RPC_URL \
  --private-key $PRIVATE_KEY
```

## Troubleshooting

### Common Issues

**"Failed to fetch deposit event data"**
- Wait 15-20 seconds for transaction to be mined
- Check RPC URL is correct
- Verify contract address

**"Circuit test failed"**
- Ensure MPT proof is valid
- Verify event data matches transaction
- Check circuit parameters

**"Proof generation failed"**
- Ensure 16GB+ RAM available
- Check KZG parameters downloaded
- Verify circuit configuration

**"Withdrawal transaction failed"**
- Check deposit not already processed
- Verify proof is valid
- Ensure bridge has balance

### Getting Help

1. Check test data in `e2e_test_data/`
2. View transactions on Etherscan
3. Review error logs
4. See [E2E_TEST_GUIDE.md](E2E_TEST_GUIDE.md) for detailed troubleshooting

## Security Warning

⚠️ **IMPORTANT**: The test uses `TestDepositVerifier` which accepts any proof with valid format. This is **NOT SECURE** and is only for testing!

For production:
1. Generate real Halo2 verifier
2. Deploy production verifier
3. Update bridge contract
4. Never use TestDepositVerifier in production!

## Performance

Typical performance on modern hardware (16GB RAM, 8-core CPU):

| Step | Time |
|------|------|
| Deploy contracts | ~30s |
| Make deposit | ~15s |
| Fetch event data | ~5s |
| MockProver test | ~10s |
| **Generate proof** | **5-10 min** |
| Submit withdrawal | ~15s |
| **Total** | **~6-11 min** |

## Next Steps

After successful testing:

1. **Generate Production Verifier**
   ```bash
   cd deposit-prover
   cargo run --release --example generate_verifier
   ```

2. **Deploy to Production**
   - Update deployment script
   - Deploy to mainnet
   - Verify on Etherscan

3. **Integrate Frontend**
   - Update contract addresses
   - Test UI flow
   - Deploy frontend

## Documentation

- [E2E Test Guide](E2E_TEST_GUIDE.md) - Detailed instructions
- [E2E Test Summary](E2E_TEST_SUMMARY.md) - Implementation details
- [Deployment Guide](contracts/ethereum/DEPLOYMENT_GUIDE.md) - Contract deployment
- [Circuit Design](deposit-prover/CIRCUIT_DESIGN.md) - ZK circuit architecture

## Support

For issues:
1. Run `./check_e2e_prerequisites.sh`
2. Check test data files
3. Review Etherscan transactions
4. See troubleshooting guide

