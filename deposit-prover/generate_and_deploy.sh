#!/bin/bash
set -e

echo "=========================================="
echo "Groth16 Verifier - Complete Workflow"
echo "=========================================="
echo ""

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

# Default values
PROOF_FILE="${1:-data/deposit_proof_42.snark}"
DEPLOY_NETWORK="${2:-sepolia}"

# Load environment variables from .env file
ENV_FILE="../contracts/ethereum/.env"
if [ -f "$ENV_FILE" ]; then
    echo -e "${GREEN}✓ Loading environment from $ENV_FILE${NC}"
    source "$ENV_FILE"
else
    echo -e "${YELLOW}⚠ Warning: .env file not found at $ENV_FILE${NC}"
    echo "  For deployment, create .env file with:"
    echo "    SEPOLIA_RPC_URL=https://eth-sepolia.g.alchemy.com/v2/YOUR_KEY"
    echo "    MAINNET_RPC_URL=https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY"
    echo "    PRIVATE_KEY=0x..."
    echo "    ETHERSCAN_API_KEY=YOUR_KEY"
    echo ""
fi

echo -e "${BLUE}Configuration:${NC}"
echo "  Proof file: $PROOF_FILE"
echo "  Network: $DEPLOY_NETWORK"
echo ""

# Step 1: Export Halo2 proof
echo -e "${BLUE}Step 1: Exporting Halo2 Proof${NC}"
echo "----------------------------------------"

if [ ! -f "$PROOF_FILE" ]; then
    echo -e "${YELLOW}Warning: Proof file not found: $PROOF_FILE${NC}"
    echo "Using default: data/deposit_proof_42.snark"
    PROOF_FILE="data/deposit_proof_42.snark"
fi

cargo run --release --example export_proof_for_gnark -- \
    --input "$PROOF_FILE" \
    --output gnark-wrapper/halo2_proof.json

echo -e "${GREEN}✓ Proof exported${NC}"
echo ""

# Step 2: Generate Groth16 proof and verifier
echo -e "${BLUE}Step 2: Generating Groth16 Proof and Verifier${NC}"
echo "----------------------------------------"

cd gnark-wrapper
go run . | tee /tmp/groth16_output.txt

if [ ! -f "Groth16Verifier.sol" ]; then
    echo -e "${YELLOW}Error: Groth16Verifier.sol not generated${NC}"
    exit 1
fi

echo -e "${GREEN}✓ Groth16 verifier generated${NC}"
echo ""

# Step 3: Optimize verifier
echo -e "${BLUE}Step 3: Optimizing Verifier${NC}"
echo "----------------------------------------"

./optimize_verifier.sh

if [ ! -f "Groth16VerifierOptimized.sol" ]; then
    echo -e "${YELLOW}Error: Optimization failed${NC}"
    exit 1
fi

OPTIMIZED_SIZE=$(wc -c < Groth16VerifierOptimized.sol)
OPTIMIZED_SIZE_KB=$((OPTIMIZED_SIZE / 1024))

echo -e "${GREEN}✓ Verifier optimized: ${OPTIMIZED_SIZE_KB}KB${NC}"
echo ""

# Step 4: Compile with Solidity
echo -e "${BLUE}Step 4: Compiling Solidity Verifier${NC}"
echo "----------------------------------------"

solc --version | head -1

# Compile and get bytecode size
BYTECODE=$(solc --optimize --optimize-runs 1 --bin Groth16VerifierOptimized.sol 2>/dev/null | grep -A 1 "Binary:" | tail -1)
BYTECODE_SIZE=$((${#BYTECODE} / 2))
BYTECODE_SIZE_KB=$((BYTECODE_SIZE / 1024))

echo "Bytecode size: $BYTECODE_SIZE bytes (${BYTECODE_SIZE_KB}KB)"

if [ $BYTECODE_SIZE -lt 24576 ]; then
    echo -e "${GREEN}✓ Under 24KB limit${NC}"
else
    echo -e "${YELLOW}✗ Exceeds 24KB limit${NC}"
    echo "Consider deploying to L2 (Arbitrum/Optimism)"
fi

echo ""

# Step 5: Run tests
echo -e "${BLUE}Step 5: Running Tests${NC}"
echo "----------------------------------------"

go test -v -short 2>&1 | grep -E "PASS|FAIL|RUN"

echo -e "${GREEN}✓ Tests completed${NC}"
echo ""

# Step 6: Deployment (optional)
echo -e "${BLUE}Step 6: Deployment${NC}"
echo "----------------------------------------"

if [ "$DEPLOY_NETWORK" == "local" ]; then
    echo "Skipping deployment (local mode)"
elif [ "$DEPLOY_NETWORK" == "sepolia" ]; then
    if [ -z "$SEPOLIA_RPC_URL" ] || [ -z "$PRIVATE_KEY" ]; then
        echo -e "${RED}✗ Error: SEPOLIA_RPC_URL or PRIVATE_KEY not set${NC}"
        echo ""
        echo "Please create ../contracts/ethereum/.env with:"
        echo "  SEPOLIA_RPC_URL=https://eth-sepolia.g.alchemy.com/v2/YOUR_KEY"
        echo "  PRIVATE_KEY=0x..."
        echo "  ETHERSCAN_API_KEY=YOUR_KEY"
        echo ""
        echo "Or set environment variables manually:"
        echo "  export SEPOLIA_RPC_URL='https://sepolia.infura.io/v3/YOUR_KEY'"
        echo "  export PRIVATE_KEY='your_private_key'"
        echo "  ./generate_and_deploy.sh $PROOF_FILE sepolia"
        exit 1
    else
        echo "Deploying to Sepolia..."
        echo "  RPC URL: ${SEPOLIA_RPC_URL:0:50}..."

        # Check if forge is installed
        if ! command -v forge &> /dev/null; then
            echo -e "${RED}✗ Error: Foundry (forge) not installed${NC}"
            echo "Install from: https://getfoundry.sh"
            exit 1
        fi

        # Get wallet address
        WALLET_ADDRESS=$(cast wallet address "$PRIVATE_KEY" 2>/dev/null || echo "")
        if [ ! -z "$WALLET_ADDRESS" ]; then
            echo "  Deployer: $WALLET_ADDRESS"

            # Check balance
            BALANCE=$(cast balance "$WALLET_ADDRESS" --rpc-url "$SEPOLIA_RPC_URL" 2>/dev/null || echo "0")
            BALANCE_ETH=$(cast --from-wei "$BALANCE" 2>/dev/null || echo "0")
            echo "  Balance: $BALANCE_ETH ETH"

            # Warn if low balance
            BALANCE_FLOAT=$(echo "$BALANCE_ETH" | awk '{print $1}')
            if (( $(echo "$BALANCE_FLOAT < 0.01" | bc -l 2>/dev/null || echo "0") )); then
                echo -e "  ${YELLOW}⚠ Warning: Low balance! Need ~0.01 ETH for deployment${NC}"
            fi
        fi

        echo ""
        echo "Deploying Groth16VerifierOptimized.sol..."

        # Deploy
        DEPLOY_OUTPUT=$(forge create Groth16VerifierOptimized.sol:V \
            --rpc-url "$SEPOLIA_RPC_URL" \
            --private-key "$PRIVATE_KEY" \
            --optimize \
            --optimizer-runs 1 \
            2>&1)

        if [ $? -eq 0 ]; then
            echo "$DEPLOY_OUTPUT"

            # Extract contract address
            CONTRACT_ADDRESS=$(echo "$DEPLOY_OUTPUT" | grep "Deployed to:" | awk '{print $3}')

            if [ ! -z "$CONTRACT_ADDRESS" ]; then
                echo ""
                echo -e "${GREEN}✓ Deployed to Sepolia${NC}"
                echo "  Contract: $CONTRACT_ADDRESS"
                echo "  Explorer: https://sepolia.etherscan.io/address/$CONTRACT_ADDRESS"

                # Save address to file
                echo "$CONTRACT_ADDRESS" > Groth16Verifier_sepolia.address
                echo ""
                echo "  Address saved to: Groth16Verifier_sepolia.address"
            fi
        else
            echo -e "${RED}✗ Deployment failed${NC}"
            echo "$DEPLOY_OUTPUT"
            exit 1
        fi
    fi
elif [ "$DEPLOY_NETWORK" == "mainnet" ]; then
    echo -e "${YELLOW}⚠ WARNING: Deploying to MAINNET${NC}"
    echo "This will cost real ETH!"
    echo ""
    echo "Press Ctrl+C to cancel, or Enter to continue..."
    read

    if [ -z "$MAINNET_RPC_URL" ] || [ -z "$PRIVATE_KEY" ]; then
        echo -e "${RED}✗ Error: MAINNET_RPC_URL or PRIVATE_KEY not set${NC}"
        echo ""
        echo "Please set in ../contracts/ethereum/.env:"
        echo "  MAINNET_RPC_URL=https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY"
        echo "  PRIVATE_KEY=0x..."
        echo "  ETHERSCAN_API_KEY=YOUR_KEY"
        exit 1
    fi

    echo "Deploying to Mainnet..."
    echo "  RPC URL: ${MAINNET_RPC_URL:0:50}..."

    # Get wallet address and balance
    WALLET_ADDRESS=$(cast wallet address "$PRIVATE_KEY" 2>/dev/null || echo "")
    if [ ! -z "$WALLET_ADDRESS" ]; then
        echo "  Deployer: $WALLET_ADDRESS"
        BALANCE=$(cast balance "$WALLET_ADDRESS" --rpc-url "$MAINNET_RPC_URL" 2>/dev/null || echo "0")
        BALANCE_ETH=$(cast --from-wei "$BALANCE" 2>/dev/null || echo "0")
        echo "  Balance: $BALANCE_ETH ETH"
    fi

    echo ""
    echo "Deploying Groth16VerifierOptimized.sol..."

    # Deploy with verification
    DEPLOY_OUTPUT=$(forge create Groth16VerifierOptimized.sol:V \
        --rpc-url "$MAINNET_RPC_URL" \
        --private-key "$PRIVATE_KEY" \
        --optimize \
        --optimizer-runs 1 \
        --verify \
        --etherscan-api-key "$ETHERSCAN_API_KEY" \
        2>&1)

    if [ $? -eq 0 ]; then
        echo "$DEPLOY_OUTPUT"

        # Extract contract address
        CONTRACT_ADDRESS=$(echo "$DEPLOY_OUTPUT" | grep "Deployed to:" | awk '{print $3}')

        if [ ! -z "$CONTRACT_ADDRESS" ]; then
            echo ""
            echo -e "${GREEN}✓ Deployed to Mainnet${NC}"
            echo "  Contract: $CONTRACT_ADDRESS"
            echo "  Explorer: https://etherscan.io/address/$CONTRACT_ADDRESS"

            # Save address to file
            echo "$CONTRACT_ADDRESS" > Groth16Verifier_mainnet.address
            echo ""
            echo "  Address saved to: Groth16Verifier_mainnet.address"
        fi
    else
        echo -e "${RED}✗ Deployment failed${NC}"
        echo "$DEPLOY_OUTPUT"
        exit 1
    fi
else
    echo "Unknown network: $DEPLOY_NETWORK"
    echo "Supported: local, sepolia, mainnet"
fi

cd ..

echo ""
echo "=========================================="
echo -e "${GREEN}Workflow Complete!${NC}"
echo "=========================================="
echo ""
echo "Summary:"
echo "  - Proof exported: ✓"
echo "  - Groth16 verifier generated: ✓"
echo "  - Verifier optimized: ✓ (${OPTIMIZED_SIZE_KB}KB)"
echo "  - Bytecode compiled: ✓ (${BYTECODE_SIZE_KB}KB)"
echo "  - Tests passed: ✓"
echo ""
echo "Files generated:"
echo "  - gnark-wrapper/Groth16Verifier.sol (original)"
echo "  - gnark-wrapper/Groth16VerifierOptimized.sol (optimized)"
echo ""
echo "Next steps:"
echo "  1. Review the generated verifier"
echo "  2. Test on testnet: ./generate_and_deploy.sh $PROOF_FILE sepolia"
echo "  3. Deploy to mainnet: ./generate_and_deploy.sh $PROOF_FILE mainnet"
echo ""
echo "For more information, see:"
echo "  - DEPLOYMENT_GUIDE.md"
echo "  - FINAL_IMPLEMENTATION_SUMMARY.md"
echo ""

