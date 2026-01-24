#!/bin/bash
# Generate a ZK proof and output only the proof hex
# This script is called by Forge tests via FFI
#
# Usage: ./generate_proof.sh <withdrawal_hash> <nullifier_preimage> <recipient> <amount> <root>
# Output: proof_hex (without 0x prefix, no newline)

set -e

if [ "$#" -ne 5 ]; then
    echo "ERROR: Usage: $0 <withdrawal_hash> <nullifier_preimage> <recipient> <amount> <root>" >&2
    exit 1
fi

WITHDRAWAL_HASH=$1
NULLIFIER_PREIMAGE=$2
RECIPIENT=$3
AMOUNT=$4
ROOT=$5

# Change to repo root
cd "$(dirname "$0")/.."

# Run the Rust proof generator and extract just the proof hex (no newline)
cargo run --bin generate-proof --quiet -- "$WITHDRAWAL_HASH" "$NULLIFIER_PREIMAGE" "$RECIPIENT" "$AMOUNT" "$ROOT" 2>&1 | \
    grep "^Proof hex:" | \
    sed 's/Proof hex: 0x//' | \
    tr -d '\n'

