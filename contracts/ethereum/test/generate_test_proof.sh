#!/bin/bash
# Generate a ZK proof for testing
#
# Usage: ./generate_test_proof.sh <nullifier> <recipient> <amount> <root>
#
# Example: ./generate_test_proof.sh 12345 43981 1000000000000000000 12345678

set -e

if [ "$#" -ne 4 ]; then
    echo "Usage: $0 <nullifier> <recipient> <amount> <root>"
    echo "Example: $0 12345 43981 1000000000000000000 12345678"
    exit 1
fi

NULLIFIER=$1
RECIPIENT=$2
AMOUNT=$3
ROOT=$4

echo "Generating proof for:"
echo "  Nullifier: $NULLIFIER"
echo "  Recipient: $RECIPIENT"
echo "  Amount: $AMOUNT"
echo "  Root: $ROOT"
echo ""

# Run the Rust proof generator
cd ../..
cargo run --bin generate-proof -- "$NULLIFIER" "$RECIPIENT" "$AMOUNT" "$ROOT" > contracts/ethereum/test/generated_proof.json 2>&1

# Extract just the JSON part
cd contracts/ethereum/test
tail -n 10 generated_proof.json | grep -A 20 "^{" > proof.json

echo ""
echo "Proof saved to contracts/ethereum/test/proof.json"

