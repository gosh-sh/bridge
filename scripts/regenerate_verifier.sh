#!/bin/bash
# Regenerate Halo2 Yul verifier and compile to bytecode

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

print_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

print_step() {
    echo -e "${BLUE}[STEP]${NC} $1"
}

echo "=== Halo2 Verifier Generation ==="
echo ""

# Check for solc
if ! command -v solc &> /dev/null; then
    print_error "solc not found! Please install Solidity compiler."
    echo "Install with: sudo apt-get install solc"
    exit 1
fi

print_info "Using solc version: $(solc --version | grep Version | head -n 1)"

# Remove old verifier files
print_step "Removing old verifier files..."
rm -f contracts/ethereum/Halo2Verifier.yul
rm -f contracts/ethereum/verifier_bytecode.bin
rm -f contracts/ethereum/verifier_bytecode.hex
print_info "Old files removed"

# Generate new Yul verifier
print_step "Generating Yul verifier..."
cargo run --bin generate-verifier

# Check if Yul file was created
if [ ! -f "contracts/ethereum/Halo2Verifier.yul" ]; then
    print_error "Verifier generation failed - Halo2Verifier.yul not found"
    exit 1
fi
print_info "Yul verifier generated successfully"

# Compile Yul to bytecode
print_step "Compiling Yul to bytecode..."
cd contracts/ethereum

# Extract binary representation and save as hex
solc --yul --bin Halo2Verifier.yul 2>&1 | grep "Binary representation" -A 1 | tail -1 > verifier_bytecode.hex

# Check if hex file was created and has content
if [ ! -s "verifier_bytecode.hex" ]; then
    print_error "Bytecode compilation failed - verifier_bytecode.hex is empty or missing"
    cd ../..
    exit 1
fi

# Convert hex to binary
cat verifier_bytecode.hex | xxd -r -p > verifier_bytecode.bin

# Check if binary file was created
if [ ! -s "verifier_bytecode.bin" ]; then
    print_error "Binary conversion failed - verifier_bytecode.bin is empty or missing"
    cd ../..
    exit 1
fi

cd ../..

# Get file sizes
YUL_SIZE=$(wc -c < contracts/ethereum/Halo2Verifier.yul)
BIN_SIZE=$(wc -c < contracts/ethereum/verifier_bytecode.bin)
HEX_SIZE=$(wc -c < contracts/ethereum/verifier_bytecode.hex)

# Ethereum contract size limit is 24576 bytes (24KB)
MAX_SIZE=24576

echo ""
echo "========================================="
print_info "Verifier generation completed successfully!"
echo "========================================="
echo ""
echo "Generated files:"
echo "  - contracts/ethereum/Halo2Verifier.yul"
echo "  - contracts/ethereum/verifier_bytecode.hex (${HEX_SIZE} bytes)"
echo "  - contracts/ethereum/verifier_bytecode.bin (${BIN_SIZE} bytes)"
echo ""

if [ $BIN_SIZE -gt $MAX_SIZE ]; then
    print_error "WARNING: Bytecode size (${BIN_SIZE} bytes) exceeds Ethereum limit (${MAX_SIZE} bytes)!"
    echo "The contract is too large to deploy on Ethereum."
    exit 1
else
    REMAINING=$((MAX_SIZE - BIN_SIZE))
    print_info "Bytecode size: ${BIN_SIZE} bytes (${REMAINING} bytes under Ethereum 24KB limit)"
fi

echo ""

