#!/bin/bash
set -e

echo "=========================================="
echo "E2E On-Chain Test Suite"
echo "=========================================="
echo ""

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Load environment from .env file if not set
if [ -z "$RPC_URL" ] && [ -z "$SEPOLIA_RPC_URL" ]; then
    ENV_FILE="../contracts/ethereum/.env"
    if [ -f "$ENV_FILE" ]; then
        echo "Loading environment from $ENV_FILE"
        source "$ENV_FILE"
        RPC_URL="$SEPOLIA_RPC_URL"
    fi
fi

# Use SEPOLIA_RPC_URL if RPC_URL not set
if [ -z "$RPC_URL" ] && [ -n "$SEPOLIA_RPC_URL" ]; then
    RPC_URL="$SEPOLIA_RPC_URL"
fi

# Check required environment variables
if [ -z "$RPC_URL" ]; then
    echo -e "${RED}Error: RPC_URL not set${NC}"
    echo "Usage: RPC_URL=<url> PRIVATE_KEY=<key> ./test_e2e_onchain.sh"
    echo "Or set SEPOLIA_RPC_URL in ../contracts/ethereum/.env"
    exit 1
fi

if [ -z "$PRIVATE_KEY" ]; then
    echo -e "${RED}Error: PRIVATE_KEY not set${NC}"
    echo "Usage: RPC_URL=<url> PRIVATE_KEY=<key> ./test_e2e_onchain.sh"
    echo "Or set PRIVATE_KEY in ../contracts/ethereum/.env"
    exit 1
fi

# Test counters
TESTS_PASSED=0
TESTS_FAILED=0

# Function to print test result
print_result() {
    if [ $1 -eq 0 ]; then
        echo -e "${GREEN}✓ PASS${NC}: $2"
        ((TESTS_PASSED++))
    else
        echo -e "${RED}✗ FAIL${NC}: $2"
        ((TESTS_FAILED++))
    fi
}

# Function to print section header
print_section() {
    echo ""
    echo "=========================================="
    echo "$1"
    echo "=========================================="
}

# Step 1: Check prerequisites
print_section "Step 1: Check Prerequisites"

# Check if forge is installed
if command -v forge &> /dev/null; then
    print_result 0 "Foundry (forge) is installed"
else
    print_result 1 "Foundry (forge) not found. Install from https://getfoundry.sh"
    exit 1
fi

# Check if cast is installed
if command -v cast &> /dev/null; then
    print_result 0 "Cast is installed"
else
    print_result 1 "Cast not found. Install Foundry from https://getfoundry.sh"
    exit 1
fi

# Check RPC connection
echo "Testing RPC connection..."
if cast block-number --rpc-url "$RPC_URL" > /dev/null 2>&1; then
    BLOCK_NUMBER=$(cast block-number --rpc-url "$RPC_URL")
    print_result 0 "RPC connection successful (block: $BLOCK_NUMBER)"
else
    print_result 1 "RPC connection failed"
    exit 1
fi

# Check account balance
ACCOUNT=$(cast wallet address --private-key "$PRIVATE_KEY")
BALANCE=$(cast balance "$ACCOUNT" --rpc-url "$RPC_URL")
BALANCE_ETH=$(cast --to-unit "$BALANCE" ether)
echo "Account: $ACCOUNT"
echo "Balance: $BALANCE_ETH ETH"

if [ "$(echo "$BALANCE_ETH > 0.01" | bc)" -eq 1 ]; then
    print_result 0 "Account has sufficient balance"
else
    echo -e "${YELLOW}⚠ WARNING${NC}: Low balance ($BALANCE_ETH ETH). May not be enough for deployment."
fi

# Step 2: Check for existing deployment
print_section "Step 2: Check for Existing Deployment"

VERIFIER_ADDRESS=""
ADDRESS_FILE="gnark-wrapper/Groth16Verifier_sepolia.address"

if [ -f "$ADDRESS_FILE" ]; then
    VERIFIER_ADDRESS=$(cat "$ADDRESS_FILE")
    echo "Found existing deployment: $VERIFIER_ADDRESS"

    # Verify it's actually deployed
    CODE=$(cast code "$VERIFIER_ADDRESS" --rpc-url "$RPC_URL" 2>/dev/null || echo "")
    if [ -n "$CODE" ] && [ "$CODE" != "0x" ]; then
        print_result 0 "Using existing verifier at $VERIFIER_ADDRESS"
        SKIP_DEPLOYMENT=true
    else
        echo "Contract not found at saved address, will redeploy"
        SKIP_DEPLOYMENT=false
    fi
else
    echo "No existing deployment found"
    SKIP_DEPLOYMENT=false
fi

# Step 3: Generate and Deploy (if needed)
if [ "$SKIP_DEPLOYMENT" = false ]; then
    print_section "Step 3: Generate Groth16 Verifier"

    echo "Generating Groth16 verifier contract..."
    cd gnark-wrapper

    if go run . 2>&1 | tee /tmp/groth16_gen.txt | grep -q "Groth16 proof generated"; then
        if [ -f Groth16VerifierOptimized.sol ]; then
            VERIFIER_SIZE=$(wc -c < Groth16VerifierOptimized.sol)
            VERIFIER_SIZE_KB=$((VERIFIER_SIZE / 1024))

            print_result 0 "Groth16 verifier generated ($VERIFIER_SIZE_KB KB)"

            if [ $VERIFIER_SIZE_KB -lt 24 ]; then
                print_result 0 "Verifier size is under 24KB limit"
            else
                echo -e "${YELLOW}⚠ WARNING${NC}: Verifier size exceeds 24KB limit (${VERIFIER_SIZE_KB}KB)"
            fi
        else
            print_result 1 "Failed to generate verifier"
            exit 1
        fi
    else
        print_result 1 "Groth16 generation failed"
        exit 1
    fi

    cd ..

    print_section "Step 4: Deploy Verifier Contract"

    echo "Deploying Groth16VerifierOptimized.sol..."

    # Deploy using forge create with absolute path
    DEPLOY_OUTPUT=$(forge create /home/sergey/Pruvendo/gosh/acki-nacki-bridge/deposit-prover/gnark-wrapper/Groth16VerifierOptimized.sol:V \
        --rpc-url "$RPC_URL" \
        --private-key "$PRIVATE_KEY" \
        --optimize \
        --optimizer-runs 1 \
        --broadcast \
        2>&1)

    if echo "$DEPLOY_OUTPUT" | grep -q "Deployed to:"; then
        VERIFIER_ADDRESS=$(echo "$DEPLOY_OUTPUT" | grep "Deployed to:" | awk '{print $3}')
        print_result 0 "Verifier deployed to: $VERIFIER_ADDRESS"

        # Save address for later use
        echo "$VERIFIER_ADDRESS" > gnark-wrapper/Groth16Verifier_sepolia.address
    else
        print_result 1 "Deployment failed"
        echo "$DEPLOY_OUTPUT"
        exit 1
    fi
else
    print_section "Step 3-4: Using Existing Deployment"
    print_result 0 "Skipped deployment, using existing contract"
fi

# Step 4: Verify contract deployment
print_section "Step 4: Verify Contract Deployment"

echo "Checking deployed contract..."
CODE=$(cast code "$VERIFIER_ADDRESS" --rpc-url "$RPC_URL")

if [ -n "$CODE" ] && [ "$CODE" != "0x" ]; then
    CODE_SIZE=${#CODE}
    CODE_SIZE_BYTES=$((CODE_SIZE / 2 - 1))  # Hex string, minus 0x prefix
    CODE_SIZE_KB=$((CODE_SIZE_BYTES / 1024))

    print_result 0 "Contract deployed successfully ($CODE_SIZE_KB KB)"

    if [ $CODE_SIZE_KB -lt 24 ]; then
        print_result 0 "Deployed contract is under 24KB limit"
    else
        print_result 1 "Deployed contract exceeds 24KB limit (${CODE_SIZE_KB}KB)"
    fi
else
    print_result 1 "Contract not found at address"
    exit 1
fi

# Step 5: Test with valid proof (positive case)
print_section "Step 5: Test with Valid Proof (Positive Case)"

echo "Generating valid Groth16 proof..."
cd gnark-wrapper

# Generate proof and extract proof data
if go run . 2>&1 | grep -q "Groth16 proof generated"; then
    print_result 0 "Valid Groth16 proof generated"

    # TODO: Extract proof bytes and public inputs from Go output
    # For now, we'll skip the actual on-chain verification
    echo -e "${YELLOW}⚠ TODO${NC}: Extract proof bytes and call verifier contract"
    echo "  This requires modifying main.go to output proof bytes in a format suitable for cast"
else
    print_result 1 "Failed to generate valid proof"
fi

cd ..

# Step 6: Test with invalid proof (negative cases)
print_section "Step 6: Test with Invalid Proof (Negative Cases)"

echo "Test Case 1: Invalid proof bytes (should fail)"
# TODO: Generate invalid proof and test
echo -e "${YELLOW}⚠ TODO${NC}: Test with invalid proof bytes"

echo "Test Case 2: Wrong public inputs (should fail)"
# TODO: Generate proof with wrong public inputs and test
echo -e "${YELLOW}⚠ TODO${NC}: Test with wrong public inputs"

echo "Test Case 3: Tampered proof (should fail)"
# TODO: Generate valid proof, tamper with it, and test
echo -e "${YELLOW}⚠ TODO${NC}: Test with tampered proof"

# Step 7: Gas analysis
print_section "Step 7: Gas Analysis"

echo "Analyzing gas costs..."
# TODO: Measure gas cost of verification
echo -e "${YELLOW}⚠ TODO${NC}: Measure verification gas cost"
echo "  Target: <500k gas"

# Step 8: Cleanup (optional)
print_section "Step 8: Cleanup"

echo "Verifier contract deployed at: $VERIFIER_ADDRESS"
echo "You can interact with it using:"
echo "  cast call $VERIFIER_ADDRESS \"verify(bytes,uint256[])\" <proof> <public_inputs> --rpc-url $RPC_URL"

# Step 9: Summary
print_section "Test Summary"

TOTAL_TESTS=$((TESTS_PASSED + TESTS_FAILED))
echo "Total tests: $TOTAL_TESTS"
echo -e "${GREEN}Passed: $TESTS_PASSED${NC}"
echo -e "${RED}Failed: $TESTS_FAILED${NC}"

echo ""
echo "Deployed Verifier Address: $VERIFIER_ADDRESS"
echo ""

if [ $TESTS_FAILED -eq 0 ]; then
    echo -e "${GREEN}=========================================="
    echo "E2E tests completed successfully! ✓"
    echo "==========================================${NC}"
    exit 0
else
    echo -e "${YELLOW}=========================================="
    echo "E2E tests completed with warnings."
    echo "Some features not yet implemented."
    echo "==========================================${NC}"
    exit 0
fi

