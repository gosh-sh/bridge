#!/bin/bash
# Negative End-to-End Test Script for Acki Nacki Bridge
# Tests that the bridge correctly REJECTS invalid inputs on Sepolia.
#
# Prerequisites:
#   - Successful positive E2E test (test_e2e.sh) must have run first
#   - Existing deployment in e2e_test_data/deployment.json
#   - Existing proof data in deposit-prover/gnark-wrapper/groth16_proof_bytes.hex
#
# Usage:
#   ./test_e2e_negative.sh          # uses existing deployment
#   SKIP_DEPOSIT_TESTS=true ./test_e2e_negative.sh   # skip on-chain deposit tests

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONTRACTS_DIR="$SCRIPT_DIR/contracts/ethereum"
DEPOSIT_PROVER_DIR="$SCRIPT_DIR/deposit-prover"
DATA_DIR="$SCRIPT_DIR/e2e_test_data"

# Load environment
source "$CONTRACTS_DIR/.env"
SKIP_DEPOSIT_TESTS="${SKIP_DEPOSIT_TESTS:-false}"

# Counters
PASSED=0
FAILED=0
TOTAL=0

# ── Helpers ──────────────────────────────────────────────────

# expect_revert <description> <cast send args...>
#   Runs `cast send` and expects it to fail (revert).
#   Returns 0 on expected revert, 1 on unexpected success.
expect_revert() {
    local desc="$1"; shift
    TOTAL=$((TOTAL + 1))
    echo -ne "  [$TOTAL] $desc ... "

    # cast send returns non-zero on revert; capture output + exit code
    local output
    output=$(cast send "$@" --rpc-url "$SEPOLIA_RPC_URL" --private-key "$PRIVATE_KEY" --json 2>&1) && {
        # cast send succeeded – but check if on-chain status is 0x0 (revert)
        local status
        status=$(echo "$output" | jq -r '.status // "0x1"' 2>/dev/null)
        if [ "$status" = "0x0" ]; then
            echo -e "${GREEN}PASS${NC} (reverted on-chain)"
            PASSED=$((PASSED + 1))
            return 0
        fi
        echo -e "${RED}FAIL${NC} (transaction succeeded, should have reverted)"
        FAILED=$((FAILED + 1))
        return 1
    }

    # cast send itself failed (revert before inclusion, or estimation error)
    echo -e "${GREEN}PASS${NC} (reverted)"
    PASSED=$((PASSED + 1))
    return 0
}

# expect_revert_call <description> <cast call args...>
#   Same but uses `cast call` (free, no gas spent).
expect_revert_call() {
    local desc="$1"; shift
    TOTAL=$((TOTAL + 1))
    echo -ne "  [$TOTAL] $desc ... "

    local output
    output=$(cast call "$@" --rpc-url "$SEPOLIA_RPC_URL" 2>&1) && {
        echo -e "${RED}FAIL${NC} (call succeeded, should have reverted)"
        FAILED=$((FAILED + 1))
        return 1
    }

    echo -e "${GREEN}PASS${NC} (reverted)"
    PASSED=$((PASSED + 1))
    return 0
}

# ── Load existing deployment & proof data ────────────────────

echo -e "${BLUE}╔════════════════════════════════════════════════════════════╗${NC}"
echo -e "${BLUE}║  Acki Nacki Bridge - Negative E2E Tests                   ║${NC}"
echo -e "${BLUE}╚════════════════════════════════════════════════════════════╝${NC}"
echo ""

if [ ! -f "$DATA_DIR/deployment.json" ]; then
    echo -e "${RED}Error: No deployment found. Run test_e2e.sh first.${NC}"
    exit 1
fi

BRIDGE_ADDRESS=$(jq -r '.bridge_address' "$DATA_DIR/deployment.json")
ORACLE_ADDRESS=$(jq -r '.oracle_address' "$DATA_DIR/deployment.json")
VERIFIER_ADDRESS=$(jq -r '.verifier_address' "$DATA_DIR/deployment.json")
ORACLE_TYPE=$(jq -r '.oracle_type // "mock"' "$DATA_DIR/deployment.json")

PROOF_HEX_FILE="$DEPOSIT_PROVER_DIR/gnark-wrapper/groth16_proof_bytes.hex"
if [ ! -f "$PROOF_HEX_FILE" ]; then
    echo -e "${RED}Error: No proof file found at $PROOF_HEX_FILE. Run test_e2e.sh first.${NC}"
    exit 1
fi

PROOF_BYTES=$(cat "$PROOF_HEX_FILE")
SENDER_ADDRESS=$(cast wallet address "$PRIVATE_KEY")

# Load deposit info from the positive test
DEPOSIT_INFO="$DATA_DIR/deposit_info.json"
DEPOSIT_AMOUNT=$(jq -r '.amount' "$DEPOSIT_INFO")
BLOCK_NUMBER=$(jq -r '.block_number' "$DEPOSIT_INFO")
DEPOSIT_ID=$(jq -r '.deposit_id' "$DEPOSIT_INFO")

echo "Bridge:   $BRIDGE_ADDRESS"
echo "Oracle:   $ORACLE_ADDRESS ($ORACLE_TYPE)"
echo "Verifier: $VERIFIER_ADDRESS"
echo "Sender:   $SENDER_ADDRESS"
echo "Deposit:  ID=$DEPOSIT_ID  Amount=$DEPOSIT_AMOUNT  Block=$BLOCK_NUMBER"
echo ""

# ── Test Group 1: Withdrawal revert paths ────────────────────

echo -e "${YELLOW}═══ Group 1: Withdrawal Revert Paths ═══${NC}"
echo ""

# 1. InvalidRecipient — withdraw to address(0)
expect_revert_call \
    "InvalidRecipient: withdraw to address(0)" \
    "$BRIDGE_ADDRESS" \
    "withdraw(address,uint256,uint256,uint256,bytes)" \
    "0x0000000000000000000000000000000000000000" \
    "$DEPOSIT_AMOUNT" \
    "$DEPOSIT_ID" \
    "$BLOCK_NUMBER" \
    "$PROOF_BYTES" \
    || true

# 2. InvalidBlockHash — use a block number the oracle doesn't know about
#    Block 1 was long ago; mock oracle definitely doesn't have it
expect_revert_call \
    "InvalidBlockHash: unknown block number (1)" \
    "$BRIDGE_ADDRESS" \
    "withdraw(address,uint256,uint256,uint256,bytes)" \
    "$SENDER_ADDRESS" \
    "$DEPOSIT_AMOUNT" \
    "$DEPOSIT_ID" \
    "1" \
    "$PROOF_BYTES" \
    || true

# 3. InvalidProof — tamper with the proof (flip a byte)
#    Take valid proof and change 1 hex character
TAMPERED_PROOF="${PROOF_BYTES:0:4}ff${PROOF_BYTES:6}"
expect_revert_call \
    "InvalidProof: tampered proof bytes" \
    "$BRIDGE_ADDRESS" \
    "withdraw(address,uint256,uint256,uint256,bytes)" \
    "$SENDER_ADDRESS" \
    "$DEPOSIT_AMOUNT" \
    "$DEPOSIT_ID" \
    "$BLOCK_NUMBER" \
    "$TAMPERED_PROOF" \
    || true

# 4. InvalidProof — empty proof
expect_revert_call \
    "InvalidProof: empty proof" \
    "$BRIDGE_ADDRESS" \
    "withdraw(address,uint256,uint256,uint256,bytes)" \
    "$SENDER_ADDRESS" \
    "$DEPOSIT_AMOUNT" \
    "$DEPOSIT_ID" \
    "$BLOCK_NUMBER" \
    "0x" \
    || true

# 5. InvalidProof — truncated proof (only 128 bytes instead of 288)
TRUNCATED_PROOF="${PROOF_BYTES:0:258}"  # 0x + 256 hex chars = 128 bytes
expect_revert_call \
    "InvalidProof: truncated proof (128 bytes)" \
    "$BRIDGE_ADDRESS" \
    "withdraw(address,uint256,uint256,uint256,bytes)" \
    "$SENDER_ADDRESS" \
    "$DEPOSIT_AMOUNT" \
    "$DEPOSIT_ID" \
    "$BLOCK_NUMBER" \
    "$TRUNCATED_PROOF" \
    || true

# 6. InvalidProof — wrong amount (proof was for 100000000000000, use double)
WRONG_AMOUNT=$((DEPOSIT_AMOUNT * 2))
expect_revert_call \
    "InvalidProof: wrong amount ($WRONG_AMOUNT vs $DEPOSIT_AMOUNT)" \
    "$BRIDGE_ADDRESS" \
    "withdraw(address,uint256,uint256,uint256,bytes)" \
    "$SENDER_ADDRESS" \
    "$WRONG_AMOUNT" \
    "$DEPOSIT_ID" \
    "$BLOCK_NUMBER" \
    "$PROOF_BYTES" \
    || true

# 7. InvalidProof — wrong deposit ID (proof was for 0, try 999)
expect_revert_call \
    "InvalidProof: wrong depositId (999)" \
    "$BRIDGE_ADDRESS" \
    "withdraw(address,uint256,uint256,uint256,bytes)" \
    "$SENDER_ADDRESS" \
    "$DEPOSIT_AMOUNT" \
    "999" \
    "$BLOCK_NUMBER" \
    "$PROOF_BYTES" \
    || true

# 8. DepositAlreadyProcessed — replay the already-withdrawn deposit #0
#    This requires the oracle to have the block hash AND a valid proof.
#    Since deposit 0 was already processed, it should revert even with a valid proof.
#    Use cast call so we don't spend gas.
expect_revert_call \
    "DepositAlreadyProcessed: replay deposit #$DEPOSIT_ID" \
    "$BRIDGE_ADDRESS" \
    "withdraw(address,uint256,uint256,uint256,bytes)" \
    "$SENDER_ADDRESS" \
    "$DEPOSIT_AMOUNT" \
    "$DEPOSIT_ID" \
    "$BLOCK_NUMBER" \
    "$PROOF_BYTES" \
    || true

echo ""

# ── Test Group 2: Deposit revert paths ───────────────────────

if [ "$SKIP_DEPOSIT_TESTS" = "true" ]; then
    echo -e "${YELLOW}═══ Group 2: Deposit Revert Paths (SKIPPED) ═══${NC}"
else
    echo -e "${YELLOW}═══ Group 2: Deposit Revert Paths ═══${NC}"
    echo ""

    # 9. InvalidAmount — deposit 0 ETH
    expect_revert_call \
        "InvalidAmount: deposit 0 ETH" \
        "$BRIDGE_ADDRESS" \
        "deposit()" \
        --value "0" \
        || true

    # 10. DepositTooLarge — deposit more than MAX_DEPOSIT_AMOUNT (100 ether)
    expect_revert_call \
        "DepositTooLarge: deposit 101 ETH" \
        "$BRIDGE_ADDRESS" \
        "deposit()" \
        --value "101ether" \
        || true
fi

echo ""

# ── Test Group 3: Direct verifier return-value checks ────────
#    verifyWithdrawalProof returns (false, 0x0) for invalid inputs,
#    it does NOT revert. So we check the return value.

echo -e "${YELLOW}═══ Group 3: Direct Verifier Contract Rejection ═══${NC}"
echo ""

# Helper: expect verifier to return isValid=false
expect_verifier_false() {
    local desc="$1"; shift
    TOTAL=$((TOTAL + 1))
    echo -ne "  [$TOTAL] $desc ... "

    local result
    result=$(cast call "$VERIFIER_ADDRESS" \
        "verifyWithdrawalProof(bytes,uint256[])(bool,bytes32)" \
        "$@" \
        --rpc-url "$SEPOLIA_RPC_URL" 2>&1) || {
        # If the call itself fails, that's also an acceptable rejection
        echo -e "${GREEN}PASS${NC} (call reverted)"
        PASSED=$((PASSED + 1))
        return 0
    }

    local is_valid
    is_valid=$(echo "$result" | head -1)
    if [ "$is_valid" = "false" ]; then
        echo -e "${GREEN}PASS${NC} (returned false)"
        PASSED=$((PASSED + 1))
        return 0
    fi

    echo -e "${RED}FAIL${NC} (returned $is_valid, expected false)"
    FAILED=$((FAILED + 1))
    return 1
}

# 11. Verifier: garbage proof → returns false
expect_verifier_false \
    "Verifier returns false for garbage proof" \
    "0xdeadbeef" "[0,1,2,3,4,5]" \
    || true

# 12. Verifier: wrong number of public inputs → returns false
expect_verifier_false \
    "Verifier returns false for wrong input count" \
    "$PROOF_BYTES" "[0,1,2]" \
    || true

# 13. Verifier: valid-length proof but all zeros → returns false
ZERO_PROOF="0x$(printf '0%.0s' $(seq 1 576))"
expect_verifier_false \
    "Verifier returns false for zero proof (288 bytes)" \
    "$ZERO_PROOF" "[0,1,2,3,4,5]" \
    || true

echo ""

# ── Summary ──────────────────────────────────────────────────

echo -e "${BLUE}╔════════════════════════════════════════════════════════════╗${NC}"
if [ $FAILED -eq 0 ]; then
    echo -e "${GREEN}║  Negative E2E Tests: $PASSED/$TOTAL PASSED ✓                        ║${NC}"
    echo -e "${BLUE}╚════════════════════════════════════════════════════════════╝${NC}"
    echo ""
    echo -e "${GREEN}All revert paths work correctly on Sepolia!${NC}"
    exit 0
else
    echo -e "${RED}║  Negative E2E Tests: $PASSED passed, $FAILED FAILED ✗              ║${NC}"
    echo -e "${BLUE}╚════════════════════════════════════════════════════════════╝${NC}"
    echo ""
    echo -e "${RED}Some revert paths did not work as expected!${NC}"
    exit 1
fi

