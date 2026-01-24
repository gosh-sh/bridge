#!/bin/bash
# Generate a ZK proof and save it to a file
# This script is called by Forge tests via FFI
#
# Usage: ./generate_proof_to_file.sh <nullifier> <recipient> <amount> <root> <output_file>

set -e

if [ "$#" -ne 5 ]; then
    echo "ERROR: Usage: $0 <nullifier> <recipient> <amount> <root> <output_file>" >&2
    exit 1
fi

NULLIFIER=$1
RECIPIENT=$2
AMOUNT=$3
ROOT=$4
OUTPUT_FILE=$5

# Change to repo root
cd "$(dirname "$0")/.."

# Run the Rust proof generator and extract just the proof hex
cargo run --bin generate-proof --quiet -- "$NULLIFIER" "$RECIPIENT" "$AMOUNT" "$ROOT" 2>&1 | \
    grep "^Proof hex:" | \
    sed 's/Proof hex: //' > "$OUTPUT_FILE"

echo "Proof saved to $OUTPUT_FILE"

