# Deployment Guide - Test Bridge on Sepolia

This guide walks through deploying the AckiNackiBridge to Sepolia testnet for integration testing with the deposit prover.

## Prerequisites

1. **Foundry installed**
   ```bash
   curl -L https://foundry.paradigm.xyz | bash
   foundryup
   ```

2. **Sepolia ETH** (for gas fees)
   - Get from [Sepolia Faucet](https://sepoliafaucet.com/)
   - Need ~0.1 ETH for deployment and testing

3. **Alchemy or Infura account** (for RPC access)
   - Sign up at [Alchemy](https://www.alchemy.com/) or [Infura](https://infura.io/)
   - Create a Sepolia app and get API key

## Setup

### 1. Configure Environment

Create `.env` file (copy from `.env.example`):

```bash
cp .env.example .env
```

Edit `.env` and add your values:

```bash
# Sepolia RPC URL (Alchemy example)
SEPOLIA_RPC_URL=https://eth-sepolia.g.alchemy.com/v2/YOUR_API_KEY

# Your private key (account with Sepolia ETH)
PRIVATE_KEY=0x...

# Etherscan API key (optional, for verification)
ETHERSCAN_API_KEY=YOUR_KEY
```

**⚠️ IMPORTANT:** Never commit your `.env` file! It's already in `.gitignore`.

### 2. Load Environment

```bash
source .env
```

## Deployment

### Deploy to Sepolia

```bash
forge script script/DeployTestBridge.s.sol:DeployTestBridge \
  --rpc-url $SEPOLIA_RPC_URL \
  --broadcast \
  --verify \
  -vvvv
```

**Expected output:**
```
== Logs ==
  TestDepositVerifier deployed at: 0x1234...
  AckiNackiBridge deployed at: 0x5678...

=== Deployment Complete ===
Network: Sepolia
Verifier: 0x1234...
Bridge: 0x5678...
```

**Save these addresses!** You'll need them for testing.

### Verify Contracts (if auto-verify failed)

```bash
# Verify TestDepositVerifier
forge verify-contract \
  --chain-id 11155111 \
  --compiler-version v0.8.19 \
  0x1234... \
  script/DeployTestBridge.s.sol:TestDepositVerifier \
  --etherscan-api-key $ETHERSCAN_API_KEY

# Verify AckiNackiBridge
forge verify-contract \
  --chain-id 11155111 \
  --compiler-version v0.8.19 \
  --constructor-args $(cast abi-encode "constructor(address)" 0x1234...) \
  0x5678... \
  src/AckiNackiBridge.sol:AckiNackiBridge \
  --etherscan-api-key $ETHERSCAN_API_KEY
```

## Testing

### 1. Make a Test Deposit

```bash
# Deposit 0.1 ETH
cast send 0x5678... \
  "deposit(uint256)" 100000000000000000 \
  --value 0.1ether \
  --rpc-url $SEPOLIA_RPC_URL \
  --private-key $PRIVATE_KEY
```

**Expected output:**
```
blockHash               0xabcd...
blockNumber             5123456
...
transactionHash         0xdef0...
status                  1 (success)
```

**Save the transaction hash!** You'll use it to fetch the deposit proof.

### 2. Check Deposit Event

```bash
# Get transaction receipt
cast receipt 0xdef0... --rpc-url $SEPOLIA_RPC_URL

# Or view on Etherscan
# https://sepolia.etherscan.io/tx/0xdef0...
```

Look for the `Deposit` event in the logs:
```
Deposit(
  depositId: 0,
  sender: 0x...,
  amount: 100000000000000000,
  timestamp: 1706123456
)
```

### 3. Fetch Deposit Proof

Now use the deposit-prover to fetch the proof:

```bash
cd ../../deposit-prover

cargo run --example fetch_deposit_data -- \
  --rpc-url $SEPOLIA_RPC_URL \
  --tx-hash 0xdef0... \
  --contract 0x5678... \
  --log-index 0 \
  --output deposit_proof_input.json
```

**Expected output:**
```
🔍 Fetching deposit data from Ethereum...
  - RPC URL: https://eth-sepolia.g.alchemy.com/v2/...
  - Transaction: 0xdef0...
  - Contract: 0x5678...
  - Log index: 0

📦 Fetching transaction receipt...
  ✓ Receipt found in block 5123456

🔍 Parsing Deposit event...
  ✓ Event signature matches
  ✓ Contract address matches
  
Event Data:
  Deposit ID: 0
  Sender: 0x...
  Amount: 100000000000000000
  Timestamp: 1706123456

📜 Fetching receipt proof...
  - Encoding receipt as RLP...
    ✓ Receipt RLP encoded (234 bytes)
  - Encoding block header as RLP...
    ✓ Block header RLP encoded (567 bytes)
  - Generating MPT proof...
    (This will fetch all receipts in the block and build the trie)
    Progress: 0/150
    Progress: 10/150
    ...
    Progress: 150/150
    ✅ Fetched all receipts
    🌳 Building receipt trie...
    ✅ Trie root verified: 0x...
    🔐 Generating MPT proof for tx index 5...
    ✅ Generated proof with 8 nodes

✅ Deposit proof fetched successfully!

Saved to: deposit_proof_input.json
```

### 4. Test Circuit with Real Data

```bash
cargo run --example test_with_real_data -- \
  --input deposit_proof_input.json \
  --degree 18
```

**Expected output:**
```
🧪 Testing deposit circuit with real Ethereum data...

📖 Loading proof input from: deposit_proof_input.json
  ✓ Loaded successfully

Event Data:
  Deposit ID: 0
  Sender: 0x...
  Amount: 100000000000000000
  Contract: 0x5678...

Receipt Proof:
  Receipt RLP: 234 bytes
  Proof Nodes: 8
  Receipt Root: 0x...
  Block Header RLP: 567 bytes

🔧 Creating circuit...
  - Degree: 18 (2^18 = 262144 rows)
  - Max data byte len: 256
  - Max log num: 20

⚙️  Building circuit (Phase 0 + Phase 1)...
  ✓ Circuit built successfully

🧮 Running MockProver...
  ✓ All constraints satisfied!

✅ Circuit test passed with real Ethereum data!

Next steps:
  1. Generate SNARK proof: cargo run --release --example generate_proof -- --input deposit_proof_input.json
  2. Verify proof: cargo run --example verify_proof -- --proof deposit_proof_1.bin
```

### 5. Generate SNARK Proof (Optional - takes ~5-10 minutes)

```bash
cargo run --release --example generate_proof -- \
  --input deposit_proof_input.json \
  --output deposit_proof_1.bin
```

### 6. Verify SNARK Proof (Optional)

```bash
cargo run --example verify_proof -- \
  --proof deposit_proof_1.bin
```

## Troubleshooting

### Error: "insufficient funds for gas * price + value"

**Solution:** Add more Sepolia ETH to your account from a faucet.

### Error: "transaction underpriced"

**Solution:** Increase gas price:
```bash
cast send ... --gas-price 2gwei
```

### Error: "nonce too low"

**Solution:** Wait a few seconds and retry, or specify nonce manually:
```bash
cast send ... --nonce $(cast nonce YOUR_ADDRESS --rpc-url $SEPOLIA_RPC_URL)
```

### Error: "Circuit test failed: MPT proof verification failed"

**Possible causes:**
1. Transaction not confirmed yet - wait a few blocks
2. Wrong contract address
3. Wrong log index (if multiple events in transaction)

**Solution:** 
- Check transaction on Etherscan
- Verify contract address matches
- Try with `--log-index 0` (first event)

## Next Steps

After successful testing:

1. **Generate Solidity Verifier**
   ```bash
   cd ../deposit-prover
   cargo run --example generate_verifier --release
   ```

2. **Deploy Real Verifier**
   - Deploy the generated `DepositVerifier.sol` to Sepolia
   - Update bridge to use real verifier

3. **Test Full Withdrawal Flow**
   - Generate proof for deposit
   - Submit proof to bridge contract
   - Verify withdrawal succeeds

4. **Optimize Circuit Parameters**
   - Tune degree for optimal proof size/time
   - Measure gas costs

5. **Security Audit**
   - Professional review before mainnet

6. **Mainnet Deployment**
   - Deploy to Ethereum mainnet
   - Integrate with Acki Nacki

## Useful Commands

```bash
# Check balance
cast balance YOUR_ADDRESS --rpc-url $SEPOLIA_RPC_URL

# Get transaction receipt
cast receipt TX_HASH --rpc-url $SEPOLIA_RPC_URL

# Call contract (read)
cast call BRIDGE_ADDRESS "depositCounter()" --rpc-url $SEPOLIA_RPC_URL

# Get logs
cast logs --address BRIDGE_ADDRESS --rpc-url $SEPOLIA_RPC_URL

# Decode event
cast --abi-decode "Deposit(uint256,address,uint256,uint256)" LOG_DATA
```

## Resources

- [Foundry Book](https://book.getfoundry.sh/)
- [Sepolia Faucet](https://sepoliafaucet.com/)
- [Sepolia Etherscan](https://sepolia.etherscan.io/)
- [Alchemy Dashboard](https://dashboard.alchemy.com/)

