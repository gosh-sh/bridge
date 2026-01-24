#!/bin/bash
# Generate a ZK proof and save it to a file
# This script is called by Forge tests via FFI
#
# Usage: ./generate_proof_to_file.sh <withdrawal_hash> <nullifier_preimage> <recipient> <amount> <root> <output_file>

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

# Run the Rust proof generator and extract just the proof hex
cargo run --bin generate-proof --quiet -- "$WITHDRAWAL_HASH" "$NULLIFIER_PREIMAGE" "$RECIPIENT" "$AMOUNT" "$ROOT" 2>&1 | \
    grep "^Proof hex:" | \
    sed 's/Proof hex: //' > "$OUTPUT_FILE"

echo "Proof saved to $OUTPUT_FILE"

