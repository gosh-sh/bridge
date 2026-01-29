# Integration Testing Guide

This guide explains how to test the deposit prover with real Ethereum data.

## Overview

Integration testing validates the entire system end-to-end:
1. Deploy a test bridge contract to Sepolia
2. Make a test deposit transaction
3. Fetch the receipt and event data
4. Generate a ZK proof
5. Verify the proof
6. Submit to Acki Nacki for withdrawal

## Prerequisites

1. **Ethereum RPC Access**
   - Get a free API key from [Alchemy](https://www.alchemy.com/) or [Infura](https://infura.io/)
   - Set environment variable: `export ETH_RPC_URL="https://eth-sepolia.g.alchemy.com/v2/YOUR_KEY"`

2. **Test ETH on Sepolia**
   - Get free testnet ETH from [Sepolia Faucet](https://sepoliafaucet.com/)

3. **Rust Toolchain**
   - Install from https://rustup.rs/

## Step-by-Step Guide

### Step 1: Deploy Test Bridge Contract

First, deploy the bridge contract to Sepolia testnet:

```bash
cd ../contracts
npx hardhat run scripts/deploy.js --network sepolia
```

**Save the contract address!** You'll need it for the next steps.

Example output:
```
AckiNackiBridge deployed to: 0x1234567890123456789012345678901234567890
```

### Step 2: Make a Test Deposit

Make a deposit transaction to the bridge contract:

```bash
# Using cast (Foundry)
cast send 0x1234567890123456789012345678901234567890 \
  "deposit()" \
  --value 0.1ether \
  --rpc-url $ETH_RPC_URL \
  --private-key $PRIVATE_KEY

# Or using Hardhat
npx hardhat run scripts/make-deposit.js --network sepolia
```

**Save the transaction hash!** Example: `0xabcd...1234`

Wait for the transaction to be confirmed (usually 15-30 seconds on Sepolia).

### Step 3: Fetch Deposit Data

Use the `fetch_deposit_data` tool to fetch the event data and receipt proof:

```bash
cd deposit-prover

cargo run --example fetch_deposit_data -- \
  --rpc-url $ETH_RPC_URL \
  --tx-hash 0xabcd...1234 \
  --contract 0x1234567890123456789012345678901234567890 \
  --log-index 0 \
  --output deposit_proof_input.json
```

**Expected output:**
```
=== Deposit Data Fetcher ===

Configuration:
  RPC URL: https://eth-sepolia.g.alchemy.com/v2/...
  Transaction: 0xabcd...1234
  Contract: 0x1234567890123456789012345678901234567890
  Log Index: 0
  Output: deposit_proof_input.json

Connecting to Ethereum...
✓ Connected

Fetching deposit proof...
  - Fetching receipt...
    ✓ Receipt found (block: 12345678)
  - Parsing Deposit event...
    ✓ Event parsed (depositId: 1)
  - Fetching block header...
    ✓ Block header fetched
  - Encoding receipt as RLP...
    ✓ Receipt RLP encoded (234 bytes)
  - Encoding block header as RLP...
    ✓ Block header RLP encoded (567 bytes)
  ⚠️  MPT proof fetching not implemented yet
     Using placeholder proof nodes

✅ Deposit proof fetched successfully!

Event Data:
  Block Number: 12345678
  Transaction Index: 5
  Log Index: 0
  Deposit ID: 1
  Sender: 0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb
  Amount: 100000000000000000 wei
  Timestamp: 1234567890
  Contract: 0x1234567890123456789012345678901234567890

Receipt Proof:
  Receipt RLP: 234 bytes
  Proof Nodes: 0
  Receipt Root: 0x...
  Block Header RLP: 567 bytes

✓ Saved

Next steps:
1. Review the saved data in deposit_proof_input.json
2. Use this data to test the circuit
3. Generate a proof
```

### Step 4: Test with MockProver

Test the circuit with the real data using MockProver (fast, no proof generation):

```bash
cargo run --example test_with_real_data -- --input deposit_proof_input.json
```

**Expected output:**
```
=== Test Circuit with Real Data ===

Loading input from deposit_proof_input.json...
✓ Input loaded

Event Data:
  Block Number: 12345678
  Deposit ID: 1
  Sender: 0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb
  Amount: 100000000000000000 wei

Circuit Configuration:
  Degree (k): 18
  Max data byte len: 256
  Max log num: 20

Testing circuit with MockProver...
This may take a few minutes...

🔧 Phase 0: MPT Verification + RLP Decoding
   ✓ Loaded tx_index: 5
   ✓ MPT proof verified
   ✓ Receipt decoded
   ✓ Log extracted

🔧 Phase 1: Event Verification + Public Outputs
   ✓ Event signature verified
   ✓ Contract address verified
   ✓ Public outputs exposed

✅ SUCCESS!
Circuit test passed with real Ethereum data!

Next steps:
1. Generate a full SNARK proof
2. Verify the proof
```

**Note:** If MPT proof fetching is not implemented yet, this step may fail. See "Current Limitations" below.

### Step 5: Generate SNARK Proof

Generate a full SNARK proof (this takes 5-15 minutes):

```bash
cargo run --release --example generate_proof -- --input deposit_proof_input.json
```

**Expected output:**
```
=== Generate SNARK Proof ===

Loading input from deposit_proof_input.json...
✓ Input loaded

Circuit Configuration:
  Degree (k): 18

Generating proof...
This may take 5-15 minutes on first run...

Loading KZG parameters...
Generating KZG parameters for k=18 (this may take a few minutes)...
Loading proving key...
Generating proving key (this may take a few minutes)...
Proving key generated and saved to "data/deposit_prover_k18.pk"
Generating SNARK proof...

✅ Proof generated successfully!

Proof saved to: deposit_proof_1.bin
Proof size: 12345 bytes

Public outputs:
  Deposit ID: 1
  Sender: 0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb
  Amount: 100000000000000000
  Contract: 0x1234567890123456789012345678901234567890

Next steps:
1. Verify the proof locally
2. Submit to Acki Nacki for withdrawal
```

### Step 6: Verify Proof Locally

Verify the proof before submitting to Acki Nacki:

```bash
cargo run --example verify_proof -- --proof deposit_proof_1.bin
```

**Expected output:**
```
=== Verify SNARK Proof ===

Loading proof from deposit_proof_1.bin...
✓ Proof loaded (12345 bytes)

Verifying proof...

✅ Proof is valid!

Public outputs:
  Deposit ID: 1
  Sender: 0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb
  Amount: 100000000000000000
  Contract: 0x1234567890123456789012345678901234567890
```

### Step 7: Submit to Acki Nacki

Submit the proof to Acki Nacki for withdrawal:

```bash
# TODO: Implement Acki Nacki submission
# This will call the Acki Nacki contract with the proof
```

## Current Limitations

### MPT Proof Fetching Not Implemented

**Issue:** The `eth_getProof` RPC method is not standard and not all providers support it for receipts.

**Workaround:** We need to implement one of these solutions:

1. **Use a custom Ethereum node** with receipt proof support
2. **Implement client-side MPT proof generation** from block data
3. **Use a proof service** like Axiom's API

**Status:** Currently using placeholder proof nodes. The circuit will fail MPT verification without real proofs.

### Next Steps for MPT Proof Implementation

1. **Research proof providers:**
   - Check if Alchemy/Infura support receipt proofs
   - Look into Axiom's proof API
   - Consider running a local Ethereum node

2. **Implement proof generation:**
   - Add `get_receipt_proof()` method to `EthereumFetcher`
   - Generate MPT proof from block data
   - Validate proof locally before using in circuit

3. **Test with real proofs:**
   - Fetch a real receipt proof
   - Test circuit with real MPT verification
   - Measure proof generation time

## Troubleshooting

### Error: "Receipt not found"

**Cause:** Transaction not confirmed yet or wrong network

**Solution:**
- Wait for transaction confirmation (check on Etherscan)
- Verify you're using the correct RPC URL for the network
- Check transaction hash is correct

### Error: "Event signature doesn't match"

**Cause:** Wrong log index or contract doesn't emit Deposit events

**Solution:**
- Check the transaction logs on Etherscan
- Find the correct log index for the Deposit event
- Verify the contract address is correct

### Error: "Circuit test failed: MPT proof verification failed"

**Cause:** MPT proof is invalid or missing

**Solution:**
- This is expected until MPT proof fetching is implemented
- See "Current Limitations" section above

### Error: "Out of memory"

**Cause:** Circuit is too large for available RAM

**Solution:**
- Reduce circuit degree: `--degree 17` (requires less RAM)
- Close other applications
- Use a machine with more RAM

## Performance Metrics

### MockProver Testing
- Time: 10-30 seconds
- Memory: ~2-4 GB
- Purpose: Fast validation without proof generation

### SNARK Proof Generation
- Time: 5-15 minutes (first run), 2-5 minutes (cached keys)
- Memory: ~4-8 GB
- Disk: ~700 MB (KZG params + proving key)
- Purpose: Generate verifiable proof

### Proof Verification
- Time: <1 second
- Memory: ~100 MB
- Purpose: Validate proof before submission

## Next Steps

After successful integration testing:

1. **Implement MPT proof fetching** - Critical for production use
2. **Optimize circuit parameters** - Balance between proof size and generation time
3. **Deploy Solidity verifier** - Enable on-chain verification
4. **Integrate with Acki Nacki** - Complete the bridge
5. **Security audit** - Professional review of the system
6. **Mainnet deployment** - Deploy to Ethereum mainnet

## Resources

- [Sepolia Faucet](https://sepoliafaucet.com/) - Get test ETH
- [Sepolia Etherscan](https://sepolia.etherscan.io/) - View transactions
- [Alchemy](https://www.alchemy.com/) - Ethereum RPC provider
- [Infura](https://infura.io/) - Ethereum RPC provider
- [Foundry](https://book.getfoundry.sh/) - Ethereum development toolkit
- [Hardhat](https://hardhat.org/) - Ethereum development environment

