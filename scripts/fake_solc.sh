#!/bin/bash
# Fake solc that pretends to be 0.8.19 and outputs dummy bytecode

# Check if --version flag
if [[ "$*" == *"--version"* ]]; then
    echo "solc, the solidity compiler commandline interface"
    echo "Version: 0.8.19+commit.7dd6d404.Linux.g++"
    exit 0
fi

# Output dummy bytecode (minimal valid EVM bytecode - just STOP opcode)
echo "00"
exit 0

