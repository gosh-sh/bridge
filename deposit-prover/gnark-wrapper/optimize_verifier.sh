#!/bin/bash
set -e

echo "=========================================="
echo "Optimizing Groth16 Verifier"
echo "=========================================="
echo ""

INPUT_FILE="Groth16Verifier.sol"
OUTPUT_FILE="Groth16VerifierOptimized.sol"
TEMP_FILE="Groth16Verifier_temp.sol"

if [ ! -f "$INPUT_FILE" ]; then
    echo "Error: $INPUT_FILE not found"
    echo "Run 'go run .' first to generate the verifier"
    exit 1
fi

ORIGINAL_SIZE=$(wc -c < "$INPUT_FILE")
ORIGINAL_SIZE_KB=$((ORIGINAL_SIZE / 1024))

echo "Original verifier size: $ORIGINAL_SIZE bytes ($ORIGINAL_SIZE_KB KB)"
echo ""

# Step 1: Remove comments and documentation
echo "Step 1: Removing comments and documentation..."
sed '/^[[:space:]]*\/\//d' "$INPUT_FILE" | \
sed '/^[[:space:]]*\/\*/,/\*\//d' | \
sed 's/\/\/.*$//' > "$TEMP_FILE"

STEP1_SIZE=$(wc -c < "$TEMP_FILE")
STEP1_SAVED=$((ORIGINAL_SIZE - STEP1_SIZE))
echo "  Saved: $STEP1_SAVED bytes"

# Step 2: Shorten error messages
echo "Step 2: Shortening error messages..."
sed -i 's/error PublicInputNotInField();/error E1();/' "$TEMP_FILE"
sed -i 's/error ProofInvalid();/error E2();/' "$TEMP_FILE"
sed -i 's/PublicInputNotInField/E1/g' "$TEMP_FILE"
sed -i 's/ProofInvalid/E2/g' "$TEMP_FILE"

STEP2_SIZE=$(wc -c < "$TEMP_FILE")
STEP2_SAVED=$((STEP1_SIZE - STEP2_SIZE))
echo "  Saved: $STEP2_SAVED bytes"

# Step 3: Remove empty lines
echo "Step 3: Removing empty lines..."
sed -i '/^[[:space:]]*$/d' "$TEMP_FILE"

STEP3_SIZE=$(wc -c < "$TEMP_FILE")
STEP3_SAVED=$((STEP2_SIZE - STEP3_SIZE))
echo "  Saved: $STEP3_SAVED bytes"

# Step 4: Shorten contract name
echo "Step 4: Shortening contract name..."
sed -i 's/contract Verifier/contract V/' "$TEMP_FILE"

STEP4_SIZE=$(wc -c < "$TEMP_FILE")
STEP4_SAVED=$((STEP3_SIZE - STEP4_SIZE))
echo "  Saved: $STEP4_SAVED bytes"

# Step 5: Remove unnecessary whitespace (carefully)
echo "Step 5: Optimizing whitespace..."
# Remove trailing whitespace
sed -i 's/[[:space:]]*$//' "$TEMP_FILE"
# Remove leading whitespace (but keep indentation for readability)
# sed -i 's/^[[:space:]]*//' "$TEMP_FILE"  # Too aggressive

STEP5_SIZE=$(wc -c < "$TEMP_FILE")
STEP5_SAVED=$((STEP4_SIZE - STEP5_SIZE))
echo "  Saved: $STEP5_SAVED bytes"

# Final output
mv "$TEMP_FILE" "$OUTPUT_FILE"

FINAL_SIZE=$(wc -c < "$OUTPUT_FILE")
FINAL_SIZE_KB=$((FINAL_SIZE / 1024))
TOTAL_SAVED=$((ORIGINAL_SIZE - FINAL_SIZE))
PERCENT_SAVED=$((TOTAL_SAVED * 100 / ORIGINAL_SIZE))

echo ""
echo "=========================================="
echo "Optimization Complete"
echo "=========================================="
echo "Original size:  $ORIGINAL_SIZE bytes ($ORIGINAL_SIZE_KB KB)"
echo "Optimized size: $FINAL_SIZE bytes ($FINAL_SIZE_KB KB)"
echo "Total saved:    $TOTAL_SAVED bytes ($PERCENT_SAVED%)"
echo ""

if [ $FINAL_SIZE_KB -lt 24 ]; then
    echo "✓ SUCCESS: Verifier is under 24KB limit!"
else
    EXCESS=$((FINAL_SIZE_KB - 24))
    echo "✗ WARNING: Verifier still exceeds 24KB limit by ${EXCESS}KB"
    echo ""
    echo "Additional optimization options:"
    echo "1. Deploy on L2 (Arbitrum/Optimism) - no 24KB limit"
    echo "2. Use Solidity compiler with optimizer_runs=1"
    echo "3. Further code optimization (inline functions, remove helpers)"
fi

echo ""
echo "Output file: $OUTPUT_FILE"

