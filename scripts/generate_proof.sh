#!/bin/bash
# Generate a ZK proof and output only the proof hex
# This script is called by Forge tests via FFI
#
# Usage: ./generate_proof.sh <nullifier> <recipient> <amount> <root>
# Output: proof_hex (without 0x prefix, no newline)

set -e

if [ "$#" -ne 4 ]; then
    echo "ERROR: Usage: $0 <nullifier> <recipient> <amount> <root>" >&2
    exit 1
fi

NULLIFIER=$1
RECIPIENT=$2
AMOUNT=$3
ROOT=$4

# Change to repo root
cd "$(dirname "$0")/.."

# Run the Rust proof generator and extract just the proof hex (no newline)
cargo run --bin generate-proof --quiet -- "$NULLIFIER" "$RECIPIENT" "$AMOUNT" "$ROOT" 2>&1 | \
    grep "^Proof hex:" | \
    sed 's/Proof hex: 0x//' | \
    tr -d '\n'

