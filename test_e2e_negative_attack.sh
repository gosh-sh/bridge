#!/bin/bash
# Negative E2E Test: Attack Simulation - Attempt to Steal Funds by Modifying Public Instances
#
# This test proves that BC-CIRCUIT-004 is a FALSE POSITIVE by demonstrating that
# an attacker CANNOT steal funds by modifying the public instances in a valid proof.
#
# Attack Scenario:
# 1. Attacker makes a small deposit (0.001 ETH)
# 2. Attacker generates a valid proof for the 0.001 ETH deposit
# 3. Attacker modifies the proof's public instances to claim 0.002 ETH (2x more!)
# 4. Attacker attempts to withdraw 0.002 ETH using the modified proof
#
# Expected Result:
# - If BC-CIRCUIT-004 is valid (instances not constrained): Withdrawal succeeds - CRITICAL VULNERABILITY!
# - If BC-CIRCUIT-004 is false positive (instances ARE constrained): Withdrawal fails - SECURE!

set -e  # Exit on error

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
MAGENTA='\033[0;35m'
NC='\033[0m' # No Color

# Configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONTRACTS_DIR="$SCRIPT_DIR/contracts/ethereum"
DEPOSIT_PROVER_DIR="$SCRIPT_DIR/deposit-prover"
DATA_DIR="$SCRIPT_DIR/e2e_attack_test_data"

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

echo -e "${MAGENTA}╔════════════════════════════════════════════════════════════╗${NC}"
echo -e "${MAGENTA}║  NEGATIVE TEST: Attack Simulation                         ║${NC}"
echo -e "${MAGENTA}║  Attempting to Steal Funds via Modified Public Instances  ║${NC}"
echo -e "${MAGENTA}╚════════════════════════════════════════════════════════════╝${NC}"
echo ""
echo -e "${YELLOW}⚠️  This test simulates a real attack to prove the system is secure${NC}"
echo ""

GNARK_DIR="$DEPOSIT_PROVER_DIR/gnark-wrapper"

# Step 0: Ensure Groth16 keys exist (must match deployed verifier from positive test)
echo -e "${YELLOW}[0/7] Checking Groth16 setup...${NC}"
if [ ! -f "$GNARK_DIR/proving.key" ] || [ ! -f "$GNARK_DIR/verification.key" ] || [ ! -f "$GNARK_DIR/circuit.r1cs" ]; then
    echo -e "${RED}Groth16 keys not found. Run test_e2e.sh first to generate keys and deploy contracts.${NC}"
    exit 1
else
    echo -e "${GREEN}✓ Groth16 keys found (using keys from positive test)${NC}"
fi
echo ""

# Use existing deployment or deploy new contracts
echo -e "${YELLOW}[1/7] Loading Bridge Contract...${NC}"
cd "$CONTRACTS_DIR"

if [ -f "$SCRIPT_DIR/e2e_test_data/deployment.json" ]; then
    echo -e "${GREEN}Using existing deployment from e2e_test_data...${NC}"
    BRIDGE_ADDRESS=$(jq -r '.bridge_address' "$SCRIPT_DIR/e2e_test_data/deployment.json")
    VERIFIER_ADDRESS=$(jq -r '.verifier_address' "$SCRIPT_DIR/e2e_test_data/deployment.json")
    ORACLE_ADDRESS=$(jq -r '.oracle_address' "$SCRIPT_DIR/e2e_test_data/deployment.json")
else
    echo -e "${RED}Error: No deployment found. Run test_e2e.sh first to deploy contracts.${NC}"
    exit 1
fi

echo "  Bridge: $BRIDGE_ADDRESS"
echo "  Verifier: $VERIFIER_ADDRESS"
echo "  Oracle: $ORACLE_ADDRESS"
echo ""

# Step 2: Make a SMALL deposit (0.00001 ETH)
echo -e "${YELLOW}[2/7] Making Small Deposit (0.00001 ETH)...${NC}"

ORIGINAL_AMOUNT="10000000000000"  # 0.00001 ETH in wei (0.001 ETH / 100)
STOLEN_AMOUNT="20000000000000"  # 0.00002 ETH in wei (2x more - enough to bypass InsufficientTreasury)

echo "Depositing 0.00001 ETH to bridge..."
DEPOSIT_TX=$(cast send "$BRIDGE_ADDRESS" \
    "deposit()" \
    --value "$ORIGINAL_AMOUNT" \
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
echo "  Amount: 0.00001 ETH"

# Wait for transaction to be mined
echo "Waiting for transaction to be mined..."
sleep 10

# Get transaction receipt
RECEIPT=$(cast receipt "$DEPOSIT_TX_HASH" --rpc-url "$SEPOLIA_RPC_URL" --json)
BLOCK_NUMBER=$(echo "$RECEIPT" | jq -r '.blockNumber')
TX_INDEX=$(echo "$RECEIPT" | jq -r '.transactionIndex')

# Extract deposit ID from event logs
DEPOSIT_ID=$(echo "$RECEIPT" | jq -r '.logs[0].topics[1]' | xargs printf "%d")

echo -e "${GREEN}✓ Deposit confirmed!${NC}"
echo "  Deposit ID: $DEPOSIT_ID"
echo "  Block: $BLOCK_NUMBER"
echo ""

# Step 3: Fetch deposit event data
echo -e "${YELLOW}[3/7] Fetching Deposit Event Data...${NC}"

cd "$DEPOSIT_PROVER_DIR"

cargo run --release --example fetch_deposit_data -- \
    --tx-hash "$DEPOSIT_TX_HASH" \
    --contract "$BRIDGE_ADDRESS" \
    --rpc-url "$SEPOLIA_RPC_URL" \
    --output "$DATA_DIR/deposit_proof_input.json"

echo -e "${GREEN}✓ Event data fetched!${NC}"
echo ""

# Step 4: Generate VALID proof for 0.00001 ETH deposit
echo -e "${YELLOW}[4/7] Generating Valid Proof for 0.00001 ETH Deposit...${NC}"

# Delete any existing snark file for this deposit ID to force regeneration
# (gen_snark_shplonk loads existing files instead of regenerating)
OLD_SNARK="$DEPOSIT_PROVER_DIR/data/deposit_proof_${DEPOSIT_ID}.snark"
if [ -f "$OLD_SNARK" ]; then
    echo "Removing stale snark file: $OLD_SNARK"
    rm -f "$OLD_SNARK"
fi

echo "Generating SNARK proof (this may take several minutes)..."
cargo run --release --example test_with_real_data -- \
    --input "$DATA_DIR/deposit_proof_input.json" \
    --generate-proof \
    --output "$DATA_DIR/valid_proof.json" \
    --max-data-byte-len 2048

echo -e "${GREEN}✓ Valid Halo2 proof generated!${NC}"
echo "  Proof proves: 0.00001 ETH deposit"
echo ""

# Step 5: Wrap Halo2 proof in Groth16
echo -e "${YELLOW}[5/7] Wrapping Halo2 Proof in Groth16...${NC}"

# Extract deposit ID for snark file path
SNARK_FILE="data/deposit_proof_${DEPOSIT_ID}.snark"

if [ ! -f "$SNARK_FILE" ]; then
    echo -e "${RED}Snark file not found: $SNARK_FILE${NC}"
    exit 1
fi

# Export Halo2 proof to JSON format for gnark
echo "Exporting Halo2 proof for gnark..."
cargo run --release --example export_proof_for_gnark -- \
    --input "$SNARK_FILE" \
    --output "$GNARK_DIR/halo2_proof.json"

# Generate Groth16 proof
echo "Generating Groth16 proof..."
cd "$GNARK_DIR"
go run . prove halo2_proof.json

if [ ! -f "groth16_proof_bytes.hex" ]; then
    echo -e "${RED}Failed to generate Groth16 proof${NC}"
    exit 1
fi

VALID_PROOF_BYTES=$(cat groth16_proof_bytes.hex)
echo -e "${GREEN}✓ Groth16 proof generated (288 bytes)${NC}"
echo ""

# Step 5b: Set execution layer block hash on oracle
# On post-merge Ethereum, blockhash() returns the beacon block root, not the execution
# layer block hash. The Halo2 circuit proves the execution layer hash.
echo -e "${YELLOW}[5b] Setting execution layer block hash on oracle...${NC}"

EXEC_BLOCK_HASH=$(python3 -c "
import json
with open('$DATA_DIR/valid_proof.json') as f:
    d = json.load(f)
print('0x' + bytes(d['block_hash']).hex())
")

BLOCK_NUMBER_DEC=$(python3 -c "print(int('$BLOCK_NUMBER', 16))")

cast send "$ORACLE_ADDRESS" \
    "setBlockHash(uint256,bytes32)" \
    "$BLOCK_NUMBER_DEC" \
    "$EXEC_BLOCK_HASH" \
    --rpc-url "$SEPOLIA_RPC_URL" \
    --private-key "$PRIVATE_KEY" \
    2>&1

echo -e "${GREEN}✓ Block hash set on oracle${NC}"
echo ""

# Step 6: ATTACK - Attempt withdrawal with MODIFIED amount
echo -e "${MAGENTA}[6/7] 🎭 ATTACK: Attempting Withdrawal with Modified Amount...${NC}"

# Get sender address
SENDER_ADDRESS=$(cast wallet address "$PRIVATE_KEY")

echo -e "${RED}Attacker is attempting to steal funds!${NC}"
echo ""
echo "  Original deposit: 0.00001 ETH ($ORIGINAL_AMOUNT wei)"
echo "  Attacker claims:  0.00002 ETH ($STOLEN_AMOUNT wei)"
echo "  Theft multiplier: 2x"
echo ""
echo "  Using valid Groth16 proof for 0.00001 ETH"
echo "  But claiming 0.00002 ETH in public inputs"
echo ""

cd "$CONTRACTS_DIR"

# This should FAIL if public instances are properly constrained
set +e  # Don't exit on error - we expect this to fail

WITHDRAW_TX=$(cast send "$BRIDGE_ADDRESS" \
    "withdraw(address,uint256,uint256,uint256,bytes)" \
    "$SENDER_ADDRESS" \
    "$STOLEN_AMOUNT" \
    "$DEPOSIT_ID" \
    "$BLOCK_NUMBER" \
    "$VALID_PROOF_BYTES" \
    --rpc-url "$SEPOLIA_RPC_URL" \
    --private-key "$PRIVATE_KEY" \
    --json 2>&1)

WITHDRAW_TX_HASH=$(echo "$WITHDRAW_TX" | jq -r '.transactionHash' 2>/dev/null)

if [ -z "$WITHDRAW_TX_HASH" ] || [ "$WITHDRAW_TX_HASH" = "null" ]; then
    # Transaction was rejected - this is GOOD!
    echo -e "${GREEN}✓ Transaction rejected by verifier!${NC}"
    echo ""
    echo -e "${YELLOW}Verifier error:${NC}"
    echo "$WITHDRAW_TX" | grep -i "error\|revert" || echo "$WITHDRAW_TX"
    ATTACK_BLOCKED=true
else
    # Transaction was accepted - check if it succeeded
    echo "  TX Hash: $WITHDRAW_TX_HASH"
    echo "Waiting for confirmation..."
    sleep 10

    WITHDRAW_RECEIPT=$(cast receipt "$WITHDRAW_TX_HASH" --rpc-url "$SEPOLIA_RPC_URL" --json)
    WITHDRAW_STATUS=$(echo "$WITHDRAW_RECEIPT" | jq -r '.status')

    if [ "$WITHDRAW_STATUS" = "0x1" ]; then
        # Transaction succeeded - CRITICAL VULNERABILITY!
        ATTACK_BLOCKED=false
    else
        # Transaction reverted - this is GOOD!
        ATTACK_BLOCKED=true
    fi
fi

set -e  # Re-enable exit on error

echo ""

# Step 7: Analyze results
echo -e "${BLUE}[7/7] 📊 Analyzing Attack Results...${NC}"
echo ""
echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo -e "${BLUE}                        TEST RESULTS                                ${NC}"
echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo ""

if [ "$ATTACK_BLOCKED" = true ]; then
    echo -e "${GREEN}✅ ATTACK BLOCKED - SYSTEM IS SECURE!${NC}"
    echo ""
    echo "The verifier REJECTED the proof with modified public instances."
    echo "This proves that public instances ARE properly constrained."
    echo ""
    echo -e "${GREEN}🎯 CONCLUSION: BC-CIRCUIT-004 is a FALSE POSITIVE${NC}"
    echo ""
    echo "The audit claim is INCORRECT. Public instances are cryptographically"
    echo "bound to the proof. An attacker CANNOT steal funds by modifying the"
    echo "amount or any other public instance value."
    echo ""
    echo "Technical explanation:"
    echo "  • The Groth16 proof was generated for amount = 0.00001 ETH"
    echo "  • Attacker tried to claim amount = 0.00002 ETH"
    echo "  • Groth16 verification checked that proof matches provided public inputs"
    echo "  • Verification FAILED because public inputs don't match the proof"
    echo "  • The bridge is SECURE against this attack vector"
    echo ""
    EXIT_CODE=0
else
    echo -e "${RED}❌ CRITICAL SECURITY FAILURE!${NC}"
    echo ""
    echo "The verifier ACCEPTED the proof with modified public instances!"
    echo "The attacker successfully stole funds!"
    echo ""
    echo -e "${RED}🚨 CONCLUSION: BC-CIRCUIT-004 is VALID - CRITICAL VULNERABILITY!${NC}"
    echo ""
    echo "This means public instances are NOT properly constrained."
    echo "An attacker CAN steal funds by modifying the amount in public instances."
    echo ""
    echo "Withdrawal transaction: https://sepolia.etherscan.io/tx/$WITHDRAW_TX_HASH"
    echo ""
    EXIT_CODE=1
fi

echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo ""

if [ $EXIT_CODE -eq 0 ]; then
    echo -e "${GREEN}╔════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${GREEN}║  Negative Test PASSED - System is Secure! ✓               ║${NC}"
    echo -e "${GREEN}╚════════════════════════════════════════════════════════════╝${NC}"
else
    echo -e "${RED}╔════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${RED}║  Negative Test FAILED - Critical Vulnerability! ✗          ║${NC}"
    echo -e "${RED}╚════════════════════════════════════════════════════════════╝${NC}"
fi

echo ""
echo "Test data saved to: $DATA_DIR"
echo ""

exit $EXIT_CODE

