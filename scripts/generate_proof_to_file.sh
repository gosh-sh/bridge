#!/bin/bash
# Generate a ZK proof and save it to a file
# This script is called by Forge tests via FFI
#
# Usage: ./generate_proof_to_file.sh <withdrawal_hash> <nullifier_preimage> <recipient> <amount> <root> <output_file>
#
# Note: Cleanup of temporary proof files is handled by the caller (Solidity tests use vm.removeFile())

set -e

if [ "$#" -ne 6 ]; then
    echo "ERROR: Usage: $0 <withdrawal_hash> <nullifier_preimage> <recipient> <amount> <root> <output_file>" >&2
    exit 1
fi

WITHDRAWAL_HASH=$1
NULLIFIER_PREIMAGE=$2
RECIPIENT=$3
AMOUNT=$4
ROOT=$5
OUTPUT_FILE=$6

# Change to repo root
cd "$(dirname "$0")/.."

# Run the Rust proof generator and save the full output to a temp file
TEMP_OUTPUT=$(mktemp)
cargo run --bin generate-proof --quiet -- "$WITHDRAWAL_HASH" "$NULLIFIER_PREIMAGE" "$RECIPIENT" "$AMOUNT" "$ROOT" > "$TEMP_OUTPUT" 2>&1

# Extract the proof hex and save to output file
if ! grep "^Proof hex:" "$TEMP_OUTPUT" | sed 's/Proof hex: //' > "$OUTPUT_FILE"; then
    echo "ERROR: Failed to extract proof hex from output" >&2
    cat "$TEMP_OUTPUT" >&2
    rm "$TEMP_OUTPUT"
    exit 1
fi

# Also save the nullifier to a separate file for the test to read
NULLIFIER_FILE="${OUTPUT_FILE%.txt}_nullifier.txt"
if ! grep '"nullifier"' "$TEMP_OUTPUT" | sed 's/.*"nullifier": "\(0x[^"]*\)".*/\1/' > "$NULLIFIER_FILE"; then
    echo "ERROR: Failed to extract nullifier from output" >&2
    cat "$TEMP_OUTPUT" >&2
    rm "$TEMP_OUTPUT"
    exit 1
fi

# Verify the nullifier file is not empty
if [ ! -s "$NULLIFIER_FILE" ]; then
    echo "ERROR: Nullifier file is empty" >&2
    cat "$TEMP_OUTPUT" >&2
    rm "$TEMP_OUTPUT"
    exit 1
fi

rm "$TEMP_OUTPUT"

echo "Proof saved to $OUTPUT_FILE" >&2
echo "Nullifier saved to $NULLIFIER_FILE" >&2

