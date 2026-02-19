#!/bin/bash
set -e

echo "=========================================="
echo "Testing Groth16 Wrapper with Multiple Proofs"
echo "=========================================="
echo ""

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Find all proof files
PROOF_FILES=(data/deposit_proof_*.snark)
TOTAL_PROOFS=${#PROOF_FILES[@]}

echo "Found $TOTAL_PROOFS proof files"
echo ""

# Test configuration
MAX_TESTS=5  # Test first 5 proofs
PASSED=0
FAILED=0

# Test each proof
for i in $(seq 0 $((MAX_TESTS - 1))); do
    if [ $i -ge $TOTAL_PROOFS ]; then
        break
    fi
    
    PROOF_FILE="${PROOF_FILES[$i]}"
    PROOF_NAME=$(basename "$PROOF_FILE")
    
    echo -e "${BLUE}Test $((i + 1))/$MAX_TESTS: $PROOF_NAME${NC}"
    echo "----------------------------------------"
    
    # Export proof
    echo "Exporting proof..."
    if cargo run --release --example export_proof_for_gnark -- \
        --input "$PROOF_FILE" \
        --output gnark-wrapper/halo2_proof.json > /tmp/export_$i.log 2>&1; then
        echo -e "${GREEN}✓ Export successful${NC}"
    else
        echo -e "${RED}✗ Export failed${NC}"
        cat /tmp/export_$i.log
        FAILED=$((FAILED + 1))
        echo ""
        continue
    fi
    
    # Generate Groth16 proof
    echo "Generating Groth16 proof..."
    cd gnark-wrapper
    if go run . > /tmp/groth16_$i.log 2>&1; then
        # Check if verification passed
        if grep -q "✓ Proof verified" /tmp/groth16_$i.log; then
            echo -e "${GREEN}✓ Groth16 proof generated and verified${NC}"
            PASSED=$((PASSED + 1))
            
            # Extract timing info
            PROVE_TIME=$(grep "Prove time:" /tmp/groth16_$i.log | awk '{print $3}')
            VERIFY_TIME=$(grep "Verify time:" /tmp/groth16_$i.log | awk '{print $3}')
            echo "  Prove time: $PROVE_TIME"
            echo "  Verify time: $VERIFY_TIME"
        else
            echo -e "${RED}✗ Verification failed${NC}"
            cat /tmp/groth16_$i.log
            FAILED=$((FAILED + 1))
        fi
    else
        echo -e "${RED}✗ Groth16 generation failed${NC}"
        cat /tmp/groth16_$i.log
        FAILED=$((FAILED + 1))
    fi
    cd ..
    
    echo ""
done

# Summary
echo "=========================================="
echo -e "${BLUE}Test Summary${NC}"
echo "=========================================="
echo "Total tests: $MAX_TESTS"
echo -e "${GREEN}Passed: $PASSED${NC}"
if [ $FAILED -gt 0 ]; then
    echo -e "${RED}Failed: $FAILED${NC}"
else
    echo "Failed: 0"
fi
echo ""

if [ $FAILED -eq 0 ]; then
    echo -e "${GREEN}✓ All tests passed!${NC}"
    echo ""
    echo "The Groth16 wrapper is working correctly with multiple proofs."
    echo "Ready for deployment to testnet."
    exit 0
else
    echo -e "${RED}✗ Some tests failed${NC}"
    echo ""
    echo "Please review the logs above and fix any issues."
    exit 1
fi

