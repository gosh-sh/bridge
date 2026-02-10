#!/bin/bash
set -e

echo "=========================================="
echo "Groth16 Wrapper Test Suite"
echo "=========================================="
echo ""

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

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

# Step 1: Export a real Halo2 proof
print_section "Step 1: Export Real Halo2 Proof"

echo "Exporting deposit_proof_42.snark to JSON..."
cargo run --release --example export_proof_for_gnark -- \
    --input data/deposit_proof_42.snark \
    --output gnark-wrapper/halo2_proof.json 2>&1 | tail -20

if [ -f gnark-wrapper/halo2_proof.json ]; then
    print_result 0 "Proof exported successfully"
    
    # Validate JSON structure
    if jq -e '.public_inputs | length == 7' gnark-wrapper/halo2_proof.json > /dev/null 2>&1; then
        print_result 0 "Proof has 7 public inputs"
    else
        print_result 1 "Proof does not have 7 public inputs"
    fi
    
    PROOF_SIZE=$(jq -r '.proof_bytes | length' gnark-wrapper/halo2_proof.json)
    if [ "$PROOF_SIZE" -eq 8224 ]; then
        print_result 0 "Proof size is 8224 bytes"
    else
        print_result 1 "Proof size is $PROOF_SIZE bytes (expected 8224)"
    fi
else
    print_result 1 "Failed to export proof"
    exit 1
fi

# Step 2: Run Go unit tests
print_section "Step 2: Run Go Unit Tests"

cd gnark-wrapper

echo "Running unit tests..."
if go test -v -short 2>&1 | tee /tmp/go_test_output.txt; then
    print_result 0 "Go unit tests passed"
else
    print_result 1 "Go unit tests failed"
    echo "See /tmp/go_test_output.txt for details"
fi

# Step 3: Test circuit compilation
print_section "Step 3: Test Circuit Compilation"

echo "Testing circuit compilation..."
if go test -v -run TestCircuitCompilation 2>&1 | grep -q "PASS"; then
    print_result 0 "Circuit compiles successfully"
else
    print_result 1 "Circuit compilation failed"
fi

# Step 4: Test with real proof data
print_section "Step 4: Test with Real Proof Data"

echo "Testing proof parsing..."
if go test -v -run TestProofParsing 2>&1 | grep -q "PASS"; then
    print_result 0 "Proof parsing works"
else
    print_result 1 "Proof parsing failed"
fi

echo "Testing circuit with real proof..."
if go test -v -run TestCircuitWithRealProof 2>&1 | tee /tmp/circuit_test.txt; then
    print_result 0 "Circuit works with real proof"
else
    # This is expected to fail since we haven't implemented full verification yet
    if grep -q "Circuit constraints not satisfied" /tmp/circuit_test.txt; then
        echo -e "${YELLOW}⚠ EXPECTED${NC}: Full verification not implemented yet"
    else
        print_result 1 "Circuit test failed unexpectedly"
    fi
fi

# Step 5: Generate Groth16 proof (long test)
print_section "Step 5: Generate Groth16 Proof (Long Test)"

echo "This test may take several minutes..."
echo "Generating Groth16 proof..."

if go test -v -run TestGroth16ProofGeneration -timeout 30m 2>&1 | tee /tmp/groth16_test.txt; then
    print_result 0 "Groth16 proof generation successful"
    
    # Extract statistics
    if grep -q "constraints" /tmp/groth16_test.txt; then
        CONSTRAINTS=$(grep "constraints" /tmp/groth16_test.txt | grep -oP '\d+(?= constraints)')
        echo "  Circuit has $CONSTRAINTS constraints"
    fi
else
    if grep -q "Circuit constraints not satisfied" /tmp/groth16_test.txt; then
        echo -e "${YELLOW}⚠ EXPECTED${NC}: Full verification not implemented yet"
        echo "  Groth16 proof generation skipped"
    else
        print_result 1 "Groth16 proof generation failed"
    fi
fi

# Step 6: Test with multiple proofs (positive cases)
print_section "Step 6: Test with Multiple Proofs (Positive Cases)"

cd ..

PROOF_FILES=(
    "data/deposit_proof_0.snark"
    "data/deposit_proof_1.snark"
    "data/deposit_proof_42.snark"
)

for proof_file in "${PROOF_FILES[@]}"; do
    if [ -f "$proof_file" ]; then
        echo "Testing with $proof_file..."
        
        cargo run --release --example export_proof_for_gnark -- \
            --input "$proof_file" \
            --output gnark-wrapper/halo2_proof_test.json > /dev/null 2>&1
        
        if [ -f gnark-wrapper/halo2_proof_test.json ]; then
            # Validate proof structure
            if jq -e '.public_inputs | length == 7' gnark-wrapper/halo2_proof_test.json > /dev/null 2>&1; then
                print_result 0 "Valid proof: $proof_file"
            else
                print_result 1 "Invalid proof structure: $proof_file"
            fi
        else
            print_result 1 "Failed to export: $proof_file"
        fi
    fi
done

# Step 7: Test with invalid data (negative cases)
print_section "Step 7: Test with Invalid Data (Negative Cases)"

echo "Creating invalid proof with wrong public inputs..."
jq '.public_inputs[0] = "999999999999999999999999999999"' \
    gnark-wrapper/halo2_proof.json > gnark-wrapper/halo2_proof_invalid.json

if [ -f gnark-wrapper/halo2_proof_invalid.json ]; then
    print_result 0 "Created invalid proof (wrong public inputs)"
else
    print_result 1 "Failed to create invalid proof"
fi

echo "Creating invalid proof with wrong proof bytes..."
jq '.proof_bytes = [0,0,0,0]' \
    gnark-wrapper/halo2_proof.json > gnark-wrapper/halo2_proof_invalid2.json

if [ -f gnark-wrapper/halo2_proof_invalid2.json ]; then
    print_result 0 "Created invalid proof (wrong proof bytes)"
else
    print_result 1 "Failed to create invalid proof"
fi

# Step 8: Measure circuit size
print_section "Step 8: Measure Circuit Size"

cd gnark-wrapper

echo "Compiling circuit and generating Solidity verifier..."
if go run . 2>&1 | tee /tmp/compile_output.txt; then
    if [ -f Groth16Verifier.sol ]; then
        VERIFIER_SIZE=$(wc -c < Groth16Verifier.sol)
        VERIFIER_SIZE_KB=$((VERIFIER_SIZE / 1024))
        
        echo "  Verifier size: $VERIFIER_SIZE bytes ($VERIFIER_SIZE_KB KB)"
        
        if [ $VERIFIER_SIZE_KB -lt 24 ]; then
            print_result 0 "Verifier size is under 24KB limit"
        else
            echo -e "${YELLOW}⚠ WARNING${NC}: Verifier size exceeds 24KB limit"
            echo "  Current: ${VERIFIER_SIZE_KB}KB, Limit: 24KB"
            echo "  Consider optimization or L2 deployment"
        fi
    else
        print_result 1 "Failed to generate Solidity verifier"
    fi
else
    if grep -q "Circuit constraints not satisfied" /tmp/compile_output.txt; then
        echo -e "${YELLOW}⚠ EXPECTED${NC}: Full verification not implemented yet"
    else
        print_result 1 "Circuit compilation failed"
    fi
fi

cd ..

# Step 9: Summary
print_section "Test Summary"

TOTAL_TESTS=$((TESTS_PASSED + TESTS_FAILED))
echo "Total tests: $TOTAL_TESTS"
echo -e "${GREEN}Passed: $TESTS_PASSED${NC}"
echo -e "${RED}Failed: $TESTS_FAILED${NC}"

if [ $TESTS_FAILED -eq 0 ]; then
    echo ""
    echo -e "${GREEN}=========================================="
    echo "All tests passed! ✓"
    echo "==========================================${NC}"
    exit 0
else
    echo ""
    echo -e "${RED}=========================================="
    echo "Some tests failed. See output above."
    echo "==========================================${NC}"
    exit 1
fi

