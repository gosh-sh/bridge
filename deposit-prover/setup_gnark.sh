#!/bin/bash
set -e

echo "=========================================="
echo "Setting up gnark Groth16 Wrapper"
echo "=========================================="

# Check Go installation
if ! command -v go &> /dev/null; then
    echo "✗ Go is not installed"
    echo "Please run: sudo ./install_dependencies.sh"
    exit 1
fi

GO_VERSION=$(go version | awk '{print $3}' | sed 's/go//')
echo "✓ Go version: $GO_VERSION"

# Create gnark-wrapper directory
echo ""
echo "Creating gnark-wrapper directory..."
mkdir -p gnark-wrapper
cd gnark-wrapper

# Initialize Go module
echo ""
echo "Initializing Go module..."
if [ ! -f "go.mod" ]; then
    go mod init github.com/acki-nacki/deposit-prover/gnark-wrapper
    echo "✓ Go module initialized"
else
    echo "✓ Go module already exists"
fi

# Install gnark
echo ""
echo "Installing gnark library..."
go get github.com/consensys/gnark@latest
go get github.com/consensys/gnark-crypto@latest

# Verify installation
echo ""
echo "Verifying gnark installation..."
go list -m github.com/consensys/gnark
go list -m github.com/consensys/gnark-crypto

echo ""
echo "=========================================="
echo "✓ gnark setup completed successfully!"
echo "=========================================="
echo ""
echo "Directory structure:"
echo "  deposit-prover/"
echo "    ├── gnark-wrapper/          (Go code for Groth16 wrapper)"
echo "    │   ├── go.mod"
echo "    │   ├── go.sum"
echo "    │   ├── circuit.go          (to be created)"
echo "    │   ├── prover.go           (to be created)"
echo "    │   └── ffi.go              (to be created)"
echo "    └── src/"
echo "        └── groth16_wrapper/    (Rust FFI bindings, to be created)"
echo ""
echo "Next steps:"
echo "1. Implement Groth16 verifier circuit in gnark-wrapper/circuit.go"
echo "2. Implement Rust FFI bindings in src/groth16_wrapper/"
echo "3. Build and test the integration"

