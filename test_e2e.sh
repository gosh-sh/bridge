#!/bin/bash
# End-to-End Test Script for Acki Nacki Bridge
# This script tests the complete deposit -> proof generation -> withdrawal flow

set -e  # Exit on error

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONTRACTS_DIR="$SCRIPT_DIR/contracts/ethereum"
DEPOSIT_PROVER_DIR="$SCRIPT_DIR/deposit-prover"
DATA_DIR="$SCRIPT_DIR/e2e_test_data"

# Load environment variables
if [ -f "$CONTRACTS_DIR/.env" ]; then
    source "$CONTRACTS_DIR/.env"
else
    echo -e "${RED}Error: .env file not found in contracts/ethereum/${NC}"
    exit 1
fi

# Validate required environment variables
if [ -z "$SEPOLIA_RPC_URL" ] || [ -z "$PRIVATE_KEY" ]; then
    echo -e "${RED}Error: SEPOLIA_RPC_URL and PRIVATE_KEY must be set in .env${NC}"
    exit 1
fi

# Create data directory
mkdir -p "$DATA_DIR"

echo -e "${BLUE}╔════════════════════════════════════════════════════════════╗${NC}"
echo -e "${BLUE}║  Acki Nacki Bridge - End-to-End Test                      ║${NC}"
echo -e "${BLUE}╚════════════════════════════════════════════════════════════╝${NC}"
echo ""

# Step 1: Deploy contracts to Sepolia
echo -e "${YELLOW}[1/6] Deploying Bridge Contract to Sepolia...${NC}"
cd "$CONTRACTS_DIR"

# Check if already deployed
if [ -f "$DATA_DIR/deployment.json" ]; then
    echo -e "${GREEN}Found existing deployment, loading addresses...${NC}"
    BRIDGE_ADDRESS=$(jq -r '.bridge_address' "$DATA_DIR/deployment.json")
    VERIFIER_ADDRESS=$(jq -r '.verifier_address' "$DATA_DIR/deployment.json")
    echo "  Bridge: $BRIDGE_ADDRESS"
    echo "  Verifier: $VERIFIER_ADDRESS"
else
    echo "Deploying new contracts..."
    DEPLOY_OUTPUT=$(forge script script/DeployTestBridge.s.sol:DeployTestBridge \
        --rpc-url "$SEPOLIA_RPC_URL" \
        --broadcast \
        --private-key "$PRIVATE_KEY" \
        2>&1)
    
    echo "$DEPLOY_OUTPUT"
    
    # Extract addresses from deployment output
    VERIFIER_ADDRESS=$(echo "$DEPLOY_OUTPUT" | grep "TestDepositVerifier deployed at:" | awk '{print $NF}')
    BRIDGE_ADDRESS=$(echo "$DEPLOY_OUTPUT" | grep "AckiNackiBridge deployed at:" | awk '{print $NF}')
    
    if [ -z "$BRIDGE_ADDRESS" ] || [ -z "$VERIFIER_ADDRESS" ]; then
        echo -e "${RED}Failed to extract contract addresses from deployment${NC}"
        exit 1
    fi
    
    # Save deployment info
    cat > "$DATA_DIR/deployment.json" <<EOF
{
  "bridge_address": "$BRIDGE_ADDRESS",
  "verifier_address": "$VERIFIER_ADDRESS",
  "network": "sepolia",
  "deployed_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
EOF
    
    echo -e "${GREEN}✓ Contracts deployed successfully!${NC}"
    echo "  Bridge: $BRIDGE_ADDRESS"
    echo "  Verifier: $VERIFIER_ADDRESS"
fi

echo ""

# Step 2: Make a deposit
echo -e "${YELLOW}[2/6] Making Test Deposit...${NC}"

DEPOSIT_AMOUNT="10000000000000000"  # 0.01 ETH in wei

echo "Depositing 0.01 ETH to bridge..."
DEPOSIT_TX=$(cast send "$BRIDGE_ADDRESS" \
    "deposit()" \
    --value "$DEPOSIT_AMOUNT" \
    --rpc-url "$SEPOLIA_RPC_URL" \
    --private-key "$PRIVATE_KEY" \
    --json)

DEPOSIT_TX_HASH=$(echo "$DEPOSIT_TX" | jq -r '.transactionHash')

if [ -z "$DEPOSIT_TX_HASH" ] || [ "$DEPOSIT_TX_HASH" = "null" ]; then
    echo -e "${RED}Failed to get deposit transaction hash${NC}"
    exit 1
fi

echo -e "${GREEN}✓ Deposit transaction sent!${NC}"
echo "  TX Hash: $DEPOSIT_TX_HASH"

# Wait for transaction to be mined
echo "Waiting for transaction to be mined..."
sleep 10

# Get transaction receipt
RECEIPT=$(cast receipt "$DEPOSIT_TX_HASH" --rpc-url "$SEPOLIA_RPC_URL" --json)
BLOCK_NUMBER=$(echo "$RECEIPT" | jq -r '.blockNumber')
TX_INDEX=$(echo "$RECEIPT" | jq -r '.transactionIndex')

echo "  Block: $BLOCK_NUMBER"
echo "  TX Index: $TX_INDEX"

# Extract deposit ID from event logs
DEPOSIT_ID=$(echo "$RECEIPT" | jq -r '.logs[0].topics[1]' | xargs printf "%d")

echo -e "${GREEN}✓ Deposit confirmed!${NC}"
echo "  Deposit ID: $DEPOSIT_ID"

# Save deposit info
cat > "$DATA_DIR/deposit_info.json" <<EOF
{
  "tx_hash": "$DEPOSIT_TX_HASH",
  "block_number": "$BLOCK_NUMBER",
  "tx_index": "$TX_INDEX",
  "deposit_id": $DEPOSIT_ID,
  "amount": "$DEPOSIT_AMOUNT",
  "bridge_address": "$BRIDGE_ADDRESS"
}
EOF

echo ""

# Step 3: Fetch deposit event data and generate MPT proof
echo -e "${YELLOW}[3/6] Fetching Deposit Event Data...${NC}"

cd "$DEPOSIT_PROVER_DIR"

echo "Fetching event data and MPT proof from Ethereum..."
cargo run --release --example fetch_deposit_data -- \
    --tx-hash "$DEPOSIT_TX_HASH" \
    --contract "$BRIDGE_ADDRESS" \
    --rpc-url "$SEPOLIA_RPC_URL" \
    --output "$DATA_DIR/deposit_proof_input.json"

if [ ! -f "$DATA_DIR/deposit_proof_input.json" ]; then
    echo -e "${RED}Failed to fetch deposit event data${NC}"
    exit 1
fi

echo -e "${GREEN}✓ Event data fetched successfully!${NC}"
echo "  Saved to: $DATA_DIR/deposit_proof_input.json"

echo ""

# Step 4: Test circuit with MockProver
echo -e "${YELLOW}[4/6] Testing Circuit with MockProver...${NC}"

echo "Running MockProver test (fast, no proof generation)..."
echo "Note: Using max-data-byte-len=256 (standard for event data)"
cargo run --release --example test_with_real_data -- \
    --input "$DATA_DIR/deposit_proof_input.json" \
    --mock-only \
    --max-data-byte-len 256

echo -e "${GREEN}✓ Circuit test passed!${NC}"

echo ""

# Step 5: Generate ZK proof
echo -e "${YELLOW}[5/6] Generating ZK Proof...${NC}"

echo "Generating SNARK proof (this may take several minutes)..."
cargo run --release --example test_with_real_data -- \
    --input "$DATA_DIR/deposit_proof_input.json" \
    --generate-proof \
    --output "$DATA_DIR/deposit_proof_output.json" \
    --max-data-byte-len 1024

if [ ! -f "$DATA_DIR/deposit_proof_output.json" ]; then
    echo -e "${RED}Failed to generate proof${NC}"
    exit 1
fi

echo -e "${GREEN}✓ Proof generated successfully!${NC}"
echo "  Saved to: $DATA_DIR/deposit_proof_output.json"

# Extract proof bytes for withdrawal
PROOF_BYTES=$(jq -r '.proof_bytes' "$DATA_DIR/deposit_proof_output.json")

echo ""

# Step 6: Submit withdrawal transaction
echo -e "${YELLOW}[6/6] Submitting Withdrawal Transaction...${NC}"

cd "$CONTRACTS_DIR"

# Get sender address from private key
SENDER_ADDRESS=$(cast wallet address "$PRIVATE_KEY")

echo "Withdrawing to: $SENDER_ADDRESS"
echo "Amount: 0.01 ETH"
echo "Deposit ID: $DEPOSIT_ID"

# Prepare public inputs array
# [depositId, sender, amount, contractAddress]
PUBLIC_INPUTS="[$DEPOSIT_ID,$(cast --to-uint256 $SENDER_ADDRESS),$DEPOSIT_AMOUNT,$(cast --to-uint256 $BRIDGE_ADDRESS)]"

echo "Submitting withdrawal transaction..."
WITHDRAW_TX=$(cast send "$BRIDGE_ADDRESS" \
    "withdraw(address,uint256,uint256,bytes)" \
    "$SENDER_ADDRESS" \
    "$DEPOSIT_AMOUNT" \
    "$DEPOSIT_ID" \
    "$PROOF_BYTES" \
    --rpc-url "$SEPOLIA_RPC_URL" \
    --private-key "$PRIVATE_KEY" \
    --json)

WITHDRAW_TX_HASH=$(echo "$WITHDRAW_TX" | jq -r '.transactionHash')

if [ -z "$WITHDRAW_TX_HASH" ] || [ "$WITHDRAW_TX_HASH" = "null" ]; then
    echo -e "${RED}Failed to submit withdrawal transaction${NC}"
    exit 1
fi

echo -e "${GREEN}✓ Withdrawal transaction sent!${NC}"
echo "  TX Hash: $WITHDRAW_TX_HASH"

# Wait for confirmation
echo "Waiting for confirmation..."
sleep 10

# Check if withdrawal was successful
WITHDRAW_RECEIPT=$(cast receipt "$WITHDRAW_TX_HASH" --rpc-url "$SEPOLIA_RPC_URL" --json)
WITHDRAW_STATUS=$(echo "$WITHDRAW_RECEIPT" | jq -r '.status')

if [ "$WITHDRAW_STATUS" = "0x1" ]; then
    echo -e "${GREEN}✓ Withdrawal successful!${NC}"
else
    echo -e "${RED}✗ Withdrawal failed!${NC}"
    echo "$WITHDRAW_RECEIPT" | jq
    exit 1
fi

echo ""
echo -e "${GREEN}╔════════════════════════════════════════════════════════════╗${NC}"
echo -e "${GREEN}║  End-to-End Test PASSED! ✓                                ║${NC}"
echo -e "${GREEN}╚════════════════════════════════════════════════════════════╝${NC}"
echo ""
echo "Summary:"
echo "  Bridge Address: $BRIDGE_ADDRESS"
echo "  Deposit TX: $DEPOSIT_TX_HASH"
echo "  Deposit ID: $DEPOSIT_ID"
echo "  Withdrawal TX: $WITHDRAW_TX_HASH"
echo ""
echo "All test data saved to: $DATA_DIR"
echo ""
echo -e "${BLUE}View transactions on Etherscan:${NC}"
echo "  Deposit: https://sepolia.etherscan.io/tx/$DEPOSIT_TX_HASH"
echo "  Withdrawal: https://sepolia.etherscan.io/tx/$WITHDRAW_TX_HASH"

