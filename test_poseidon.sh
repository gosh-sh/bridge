#!/bin/bash
# Test Poseidon hash compatibility between Rust and Solidity

echo "Testing Poseidon hash with inputs: 12345, 67890"
echo ""

echo "=== Rust (pse-poseidon) ==="
cd /home/sergey/Pruvendo/gosh/acki-nacki-bridge
cargo run --bin generate-proof -- 12345 67890 2 1000000000000000000 0x05d8910571e1f1b616680718b431a71565180cfca4c89bb1a0fdc9740fc0349f 2>&1 | grep "nullifier"

echo ""
echo "=== Solidity (poseidon-solidity) ==="
cd /home/sergey/Pruvendo/gosh/acki-nacki-bridge/contracts/ethereum
forge test --match-test testProofVerificationOnly -vv 2>&1 | grep "Computed nullifier"

