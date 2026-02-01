# End-to-End Test Workflow

This document provides a detailed step-by-step explanation of the Acki Nacki Bridge end-to-end test workflow.

## Overview

The E2E test validates the complete deposit → proof generation → withdrawal flow on the Sepolia testnet. It demonstrates that:
1. Users can deposit ETH to the bridge contract
2. The deposit event is emitted and recorded on-chain
3. A zero-knowledge proof can be generated to prove the deposit occurred
4. The proof can be verified and used to withdraw funds

## Prerequisites

Before running the E2E test, ensure you have:

1. **Environment Variables** (in `.env` file):
   - `SEPOLIA_RPC_URL` - Sepolia RPC endpoint
   - `PRIVATE_KEY` - Private key with Sepolia ETH (for gas fees)
   - `ETHERSCAN_API_KEY` - Etherscan API key (for contract verification)

2. **Required Tools**:
   - Foundry (forge, cast, anvil)
   - Rust toolchain (cargo, rustc)
   - jq (JSON processor)

3. **Sufficient Sepolia ETH**:
   - At least 0.1 ETH for gas fees + deposit amount
   - Get testnet ETH from [Sepolia faucet](https://sepoliafaucet.com/)

## Test Workflow Steps

### Step 1: Deploy Contracts

**What happens:**
- Deploys `TestDepositVerifier` contract (test-only verifier)
- Deploys `AckiNackiBridge` contract with the verifier address
- Saves deployment addresses to `e2e_test_data/deployment.json`

**Commands executed:**
```bash
cd contracts/ethereum
forge script script/DeployTestBridge.s.sol:DeployTestBridge \
    --rpc-url $SEPOLIA_RPC_URL \
    --private-key $PRIVATE_KEY \
    --broadcast
```

**Output:**
```json
{
  "verifier": "0x...",
  "bridge": "0x..."
}
```

**What to verify:**
- Both contracts are deployed successfully
- Deployment addresses are saved
- Contracts are visible on [Sepolia Etherscan](https://sepolia.etherscan.io/)

---

### Step 2: Make Test Deposit

**What happens:**
- Calls the `deposit()` function on the bridge contract
- Sends 0.01 ETH (10000000000000000 wei) as `msg.value`
- Bridge contract emits a `Deposit` event with:
  - `depositId` - Unique deposit identifier
  - `sender` - Address that made the deposit
  - `amount` - Amount deposited (in wei)
  - `timestamp` - Block timestamp

**Commands executed:**
```bash
cast send $BRIDGE_ADDRESS \
    "deposit()" \
    --value 10000000000000000 \
    --rpc-url $SEPOLIA_RPC_URL \
    --private-key $PRIVATE_KEY \
    --json
```

**Output:**
```json
{
  "transactionHash": "0x...",
  "blockNumber": "0x...",
  "status": "0x1"
}
```

**What to verify:**
- Transaction is successful (status = 0x1)
- Transaction hash is returned
- Deposit event is emitted (visible on Etherscan)

---

### Step 3: Fetch Event Data and MPT Proof

**What happens:**
- Retrieves the transaction receipt from Sepolia
- Extracts the `Deposit` event from the receipt logs
- Fetches the block header for the transaction's block
- Generates a Merkle Patricia Trie (MPT) proof that:
  - The receipt exists in the block's receipt trie
  - The receipt root matches the one in the block header

**Commands executed:**
```bash
# Get transaction receipt
cast receipt $TX_HASH --rpc-url $SEPOLIA_RPC_URL --json

# Get block header
cast block $BLOCK_NUMBER --rpc-url $SEPOLIA_RPC_URL --json

# Generate MPT proof using Rust tool
cd deposit-prover
cargo run --release --example fetch_receipt_proof -- \
    --rpc-url $SEPOLIA_RPC_URL \
    --tx-hash $TX_HASH \
    --output ../e2e_test_data/deposit_proof_input.json
```

**Output (`e2e_test_data/deposit_proof_input.json`):**
```json
{
  "receipt_rlp": "0x...",
  "receipt_proof": ["0x...", "0x..."],
  "block_header_rlp": "0x...",
  "tx_index": 42,
  "contract_address": "0x...",
  "deposit_id": 0,
  "sender": "0x...",
  "amount": "10000000000000000"
}
```

**What to verify:**
- Receipt RLP is valid
- MPT proof nodes are present
- Block header RLP is valid
- Deposit event data is correctly extracted

---

### Step 4: Run MockProver Test

**What happens:**
- Creates a zero-knowledge circuit that verifies:
  1. The receipt is valid and matches the MPT proof
  2. The receipt root matches the block header's receipt root
  3. The `Deposit` event exists in the receipt logs
  4. The event was emitted by the correct contract address
  5. The event signature matches `Deposit(uint256,address,uint256,uint256)`
  6. The deposit ID, sender, and amount match expected values
- Runs the MockProver to check for constraint violations

**Commands executed:**
```bash
cd deposit-prover
cargo run --release --example test_with_real_data -- \
    --input ../e2e_test_data/deposit_proof_input.json \
    --max-data-byte-len 1024 \
    --mock-only
```

**Output:**
```
✅ MockProver verification PASSED
Circuit has X rows and Y columns
All constraints are satisfied
```

**What to verify:**
- No constraint violations
- No index out of bounds errors
- All assertions pass (contract address, event signature, deposit data)

---

### Step 5: Generate SNARK Proof

**What happens:**
- Generates or loads the proving key (first run takes ~5 minutes)
- Creates the prover circuit with the same input data
- Applies break points from the keygen phase
- Generates a SNARK proof using the Halo2 proving system
- Serializes the proof to bytes

**Commands executed:**
```bash
cd deposit-prover
cargo run --release --example test_with_real_data -- \
    --input ../e2e_test_data/deposit_proof_input.json \
    --max-data-byte-len 1024 \
    --generate-proof \
    --output ../e2e_test_data/deposit_proof_output.json
```

**Output (`e2e_test_data/deposit_proof_output.json`):**
```json
{
  "proof_bytes": "0x...",
  "public_inputs": [
    "0x...",  // deposit_id
    "0x...",  // sender
    "0x..."   // amount
  ]
}
```

**Performance:**
- First run: ~5 minutes (keygen + proof generation)
- Subsequent runs: ~45-60 seconds (proof generation only)
- Proving key is cached in `deposit-prover/data/deposit_prover_k18.pk`

**What to verify:**
- Proof generation completes without errors
- Proof bytes are non-empty
- Public inputs match the deposit data

---

### Step 6: Submit Withdrawal Transaction

**What happens:**
- Calls the `withdraw()` function on the bridge contract
- Passes the SNARK proof and public inputs
- Bridge contract:
  1. Verifies the proof using the verifier contract
  2. Checks that this deposit hasn't been withdrawn before
  3. Transfers the deposited amount to the sender
  4. Marks the deposit as withdrawn
  5. Emits a `Withdrawal` event

**Commands executed:**
```bash
# Encode the proof and public inputs
PROOF_BYTES=$(jq -r '.proof_bytes' e2e_test_data/deposit_proof_output.json)
PUBLIC_INPUTS=$(jq -r '.public_inputs | join(",")' e2e_test_data/deposit_proof_output.json)

# Submit withdrawal transaction
cast send $BRIDGE_ADDRESS \
    "withdraw(bytes,uint256[])" \
    "$PROOF_BYTES" \
    "[$PUBLIC_INPUTS]" \
    --rpc-url $SEPOLIA_RPC_URL \
    --private-key $PRIVATE_KEY \
    --json
```

**Output:**
```json
{
  "transactionHash": "0x...",
  "blockNumber": "0x...",
  "status": "0x1"
}
```

**What to verify:**
- Transaction is successful (status = 0x1)
- Withdrawal event is emitted
- User's balance increased by the deposit amount (minus gas fees)
- Attempting to withdraw again with the same proof fails

---

## Circuit Configuration

The zero-knowledge circuit uses the following parameters:

| Parameter | Value | Description |
|-----------|-------|-------------|
| `k` (degree) | 18 | Circuit size: 2^18 = 262,144 constraints |
| `max_data_byte_len` | 1024 | Maximum size for event data field |
| `max_log_num` | 20 | Maximum number of logs in a receipt |
| `topic_num_bounds` | (0, 4) | Min/max topics per log |
| `value_max_byte_len` | ~5000 | Maximum RLP-encoded receipt size |

**RLP Structure:**
- Receipt: `[status, cumulative_gas, bloom, logs]`
- Log: `[address, topics, data]`
- Topics: Concatenated 33-byte RLP-encoded values (0xa0 + 32 bytes)

---

## Troubleshooting

### Common Issues

**1. "Insufficient funds" error**
- **Cause:** Not enough Sepolia ETH for gas + deposit
- **Solution:** Get more testnet ETH from faucet

**2. "Break points not set" error**
- **Cause:** Circuit params mismatch between keygen and prover
- **Solution:** Delete `deposit-prover/data/*.pk` and regenerate

**3. "Index out of bounds" error**
- **Cause:** `value_max_byte_len` too small for receipt size
- **Solution:** Increase `--max-data-byte-len` parameter

**4. "Proof verification failed" error**
- **Cause:** Proof doesn't match public inputs or circuit changed
- **Solution:** Regenerate proof with correct inputs

**5. "Already withdrawn" error**
- **Cause:** Attempting to withdraw the same deposit twice
- **Solution:** Make a new deposit and generate a new proof

### Debug Mode

Run individual steps manually:

```bash
# Step 4: MockProver only
cd deposit-prover
cargo run --release --example test_with_real_data -- \
    --input ../e2e_test_data/deposit_proof_input.json \
    --max-data-byte-len 1024 \
    --mock-only

# Step 5: Proof generation only
cargo run --release --example test_with_real_data -- \
    --input ../e2e_test_data/deposit_proof_input.json \
    --max-data-byte-len 1024 \
    --generate-proof \
    --output ../e2e_test_data/deposit_proof_output.json
```

---

## Success Criteria

The E2E test is successful when:

1. ✅ All 6 steps complete without errors
2. ✅ Deposit transaction is confirmed on Sepolia
3. ✅ MockProver verification passes
4. ✅ SNARK proof is generated
5. ✅ Withdrawal transaction is confirmed on Sepolia
6. ✅ User receives the deposited amount back

---

## Next Steps

After a successful E2E test:

1. **Production Deployment:**
   - Replace `TestDepositVerifier` with a real verifier
   - Deploy to mainnet (requires significant ETH for gas)
   - Implement proper key management

2. **Frontend Integration:**
   - Connect the web UI to the deployed contracts
   - Add proof generation to the frontend workflow
   - Implement transaction monitoring

3. **Cross-Chain Bridge:**
   - Implement Acki Nacki blockchain integration
   - Add cross-chain message passing
   - Implement relayer infrastructure

---

## References

- [E2E Test Script](./test_e2e.sh)
- [Prerequisites Check](./check_e2e_prerequisites.sh)
- [Circuit Implementation](./deposit-prover/src/circuit_v2.rs)
- [Prover Implementation](./deposit-prover/src/prover.rs)
- [Contract Source](./contracts/ethereum/src/AckiNackiBridge.sol)

