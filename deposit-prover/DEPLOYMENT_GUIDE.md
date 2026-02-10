# Groth16 Verifier Deployment Guide

## Overview

This guide covers deploying the optimized Groth16 verifier for Halo2 proofs to Ethereum mainnet.

## Prerequisites

- Foundry (forge, cast) installed
- RPC endpoint (Infura, Alchemy, or local node)
- Private key with sufficient ETH for deployment (~0.01 ETH)
- Optimized verifier contract generated

## Step 1: Generate and Optimize Verifier

### 1.1 Export Halo2 Proof

```bash
cd deposit-prover
cargo run --release --example export_proof_for_gnark -- \
    --input data/deposit_proof_42.snark \
    --output gnark-wrapper/halo2_proof.json
```

### 1.2 Generate Groth16 Verifier

```bash
cd gnark-wrapper
go run .
```

This generates `Groth16Verifier.sol` (~28KB).

### 1.3 Optimize Verifier

```bash
./optimize_verifier.sh
```

This creates `Groth16VerifierOptimized.sol` (~15KB source, ~5.6KB bytecode).

**Optimization Results:**
- Original: 27,967 bytes (27KB)
- Optimized: 15,550 bytes (15KB)
- **Savings: 44%**
- **Status: ✓ Under 24KB limit**

## Step 2: Test Locally

### 2.1 Compile with Foundry

```bash
# Create a test Foundry project
mkdir -p test-deploy
cd test-deploy
forge init --no-commit

# Copy optimized verifier
cp ../Groth16VerifierOptimized.sol src/

# Compile
forge build --optimize --optimizer-runs 1
```

### 2.2 Check Bytecode Size

```bash
# Get bytecode size
BYTECODE=$(forge inspect V bytecode)
BYTECODE_SIZE=$((${#BYTECODE} / 2))
echo "Bytecode size: $BYTECODE_SIZE bytes"

# Should be < 24576 bytes
if [ $BYTECODE_SIZE -lt 24576 ]; then
    echo "✓ Under 24KB limit"
else
    echo "✗ Exceeds 24KB limit"
fi
```

### 2.3 Deploy to Local Testnet

```bash
# Start local testnet
anvil

# In another terminal, deploy
forge create src/Groth16VerifierOptimized.sol:V \
    --rpc-url http://localhost:8545 \
    --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
```

## Step 3: Deploy to Testnet (Sepolia)

### 3.1 Set Environment Variables

```bash
export SEPOLIA_RPC_URL="https://sepolia.infura.io/v3/YOUR_API_KEY"
export PRIVATE_KEY="your_private_key_here"
```

### 3.2 Deploy

```bash
forge create src/Groth16VerifierOptimized.sol:V \
    --rpc-url $SEPOLIA_RPC_URL \
    --private-key $PRIVATE_KEY \
    --verify \
    --etherscan-api-key $ETHERSCAN_API_KEY
```

### 3.3 Verify Deployment

```bash
# Get deployed address from output
VERIFIER_ADDRESS="0x..."

# Check contract code
cast code $VERIFIER_ADDRESS --rpc-url $SEPOLIA_RPC_URL

# Should return non-empty bytecode
```

## Step 4: Test On-Chain Verification

### 4.1 Generate Test Proof

```bash
cd ../gnark-wrapper
go run . > proof_output.txt

# Extract proof and public inputs from output
# (This requires parsing the Go output)
```

### 4.2 Call Verifier

```bash
# Call the verify function
cast call $VERIFIER_ADDRESS \
    "verifyProof(bytes,uint256[])" \
    $PROOF_BYTES \
    $PUBLIC_INPUTS \
    --rpc-url $SEPOLIA_RPC_URL
```

### 4.3 Measure Gas Cost

```bash
# Estimate gas
cast estimate $VERIFIER_ADDRESS \
    "verifyProof(bytes,uint256[])" \
    $PROOF_BYTES \
    $PUBLIC_INPUTS \
    --rpc-url $SEPOLIA_RPC_URL
```

Expected gas cost: ~250-350k gas

## Step 5: Deploy to Mainnet

### 5.1 Final Checks

- [ ] Verifier tested on testnet
- [ ] Multiple proofs verified successfully
- [ ] Gas costs acceptable
- [ ] Contract verified on Etherscan
- [ ] Sufficient ETH for deployment (~0.01 ETH + gas)

### 5.2 Deploy

```bash
export MAINNET_RPC_URL="https://mainnet.infura.io/v3/YOUR_API_KEY"
export PRIVATE_KEY="your_private_key_here"

forge create src/Groth16VerifierOptimized.sol:V \
    --rpc-url $MAINNET_RPC_URL \
    --private-key $PRIVATE_KEY \
    --verify \
    --etherscan-api-key $ETHERSCAN_API_KEY
```

### 5.3 Verify on Etherscan

The contract should be automatically verified if you used `--verify` flag.

If not, manually verify:

```bash
forge verify-contract \
    $VERIFIER_ADDRESS \
    src/Groth16VerifierOptimized.sol:V \
    --chain-id 1 \
    --etherscan-api-key $ETHERSCAN_API_KEY \
    --compiler-version v0.8.19+commit.7dd6d404 \
    --optimizer-runs 1
```

## Step 6: Integration

### 6.1 Update Bridge Contract

Update your bridge contract to use the new verifier:

```solidity
interface IGroth16Verifier {
    function verifyProof(
        bytes calldata proof,
        uint256[] calldata publicInputs
    ) external view returns (bool);
}

contract AckiNackiBridge {
    IGroth16Verifier public verifier;
    
    constructor(address _verifier) {
        verifier = IGroth16Verifier(_verifier);
    }
    
    function processDeposit(
        bytes calldata proof,
        uint256[] calldata publicInputs
    ) external {
        require(verifier.verifyProof(proof, publicInputs), "Invalid proof");
        // Process deposit...
    }
}
```

### 6.2 Generate Proofs

For each deposit, generate a Groth16 proof:

```bash
# Export Halo2 proof
cargo run --release --example export_proof_for_gnark -- \
    --input data/deposit_proof_X.snark \
    --output gnark-wrapper/halo2_proof.json

# Generate Groth16 proof
cd gnark-wrapper
go run .

# Extract proof bytes and public inputs
# Submit to bridge contract
```

## Troubleshooting

### Contract Size Still Too Large

If the optimized verifier still exceeds 24KB:

1. **Use more aggressive optimization:**
   ```bash
   solc --optimize --optimize-runs 1 --via-ir Groth16VerifierOptimized.sol
   ```

2. **Deploy on L2:**
   - Arbitrum: No contract size limit
   - Optimism: No contract size limit
   - Polygon zkEVM: No contract size limit

3. **Split contract:**
   - Deploy helper functions as separate library
   - Use DELEGATECALL to access them

### Verification Fails

If on-chain verification fails:

1. **Check proof format:**
   - Ensure proof bytes are correctly encoded
   - Verify public inputs are in correct order

2. **Test locally first:**
   - Deploy to local testnet (anvil)
   - Test with known-good proofs

3. **Check gas limit:**
   - Verification may require more gas than default
   - Increase gas limit in transaction

### High Gas Costs

If gas costs are too high:

1. **Optimize circuit:**
   - Reduce number of constraints
   - Use simpler verification logic

2. **Batch verifications:**
   - Verify multiple proofs in one transaction
   - Amortize fixed costs

## Performance Metrics

### Contract Size
- Source code: 15,550 bytes (15KB)
- Compiled bytecode: ~5,600 bytes (5.6KB)
- **Status: ✓ Well under 24KB limit**

### Gas Costs (Estimated)
- Deployment: ~1,500,000 gas (~0.003 ETH at 20 gwei)
- Verification: ~300,000 gas (~0.0006 ETH at 20 gwei)

### Verification Time
- On-chain: ~1-2 seconds (depends on block time)
- Off-chain (Go): ~1.2ms

## Security Considerations

1. **Trusted Setup:**
   - Groth16 requires trusted setup
   - Use ceremony from reputable source
   - Document setup parameters

2. **Circuit Correctness:**
   - Verify circuit logic thoroughly
   - Test with edge cases
   - Audit before mainnet deployment

3. **Proof Malleability:**
   - Ensure proofs are unique
   - Prevent replay attacks
   - Use nonces or timestamps

## Next Steps

1. Deploy to Sepolia testnet
2. Test with real deposit proofs
3. Measure gas costs
4. Audit contract
5. Deploy to mainnet
6. Integrate with bridge

## Support

For issues or questions:
- Check logs: `forge test -vvv`
- Review documentation: `docs/`
- Contact: [your contact info]

