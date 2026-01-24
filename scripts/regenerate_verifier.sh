#!/bin/bash
# Regenerate Halo2Verifier.sol with correct circuit

set -e

echo "Removing old verifier..."
rm -f contracts/ethereum/src/Halo2Verifier.sol

echo "Setting up fake solc..."
# Backup real solc if it exists
if [ -f "/usr/bin/solc" ]; then
    sudo mv /usr/bin/solc /usr/bin/solc.bak
fi

# Install fake solc
sudo cp scripts/fake_solc.sh /usr/bin/solc
sudo chmod +x /usr/bin/solc

echo "Generating new verifier..."
cargo run --bin generate-verifier

# Restore real solc
echo "Restoring real solc..."
sudo rm /usr/bin/solc
if [ -f "/usr/bin/solc.bak" ]; then
    sudo mv /usr/bin/solc.bak /usr/bin/solc
fi

# Check if file was created
if [ -f "contracts/ethereum/src/Halo2Verifier.sol" ]; then
    echo "✓ Verifier generated successfully!"
    # Fix pragma
    sed -i 's/pragma solidity \^0\.8\.19;/pragma solidity 0.8.19;/g' contracts/ethereum/src/Halo2Verifier.sol
    echo "✓ Pragma fixed to 0.8.19"
else
    echo "✗ Verifier generation failed"
    exit 1
fi

