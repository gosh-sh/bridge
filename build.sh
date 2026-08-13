#!/bin/bash
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

print_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

print_step() {
    echo -e "${BLUE}[STEP]${NC} $1"
}

# Parse command line arguments
BUILD_MODE="debug"
RUN_TESTS=false
RUN_CLIPPY=false
RUN_FORMAT=false
CLEAN=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --release)
            BUILD_MODE="release"
            shift
            ;;
        --test)
            RUN_TESTS=true
            shift
            ;;
        --clippy)
            RUN_CLIPPY=true
            shift
            ;;
        --format)
            RUN_FORMAT=true
            shift
            ;;
        --clean)
            CLEAN=true
            shift
            ;;
        --all)
            RUN_TESTS=true
            RUN_CLIPPY=true
            RUN_FORMAT=true
            shift
            ;;
        --help)
            echo "Usage: ./build.sh [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --release    Build in release mode (default: debug)"
            echo "  --test       Run tests after building"
            echo "  --clippy     Run clippy linter"
            echo "  --format     Run code formatters"
            echo "  --clean      Clean build artifacts before building"
            echo "  --all        Run all checks (format, clippy, test)"
            echo "  --help       Show this help message"
            echo ""
            echo "Examples:"
            echo "  ./build.sh                    # Build in debug mode"
            echo "  ./build.sh --release --test   # Build release and run tests"
            echo "  ./build.sh --all              # Run all checks"
            exit 0
            ;;
        *)
            print_error "Unknown option: $1"
            echo "Run './build.sh --help' for usage information"
            exit 1
            ;;
    esac
done

echo "=== Acki Nacki Bridge Build Script ==="
echo ""

# Check for Foundry installation
print_step "Checking for Foundry installation..."
if ! command -v forge &> /dev/null; then
    print_warn "Foundry (forge) not found!"
    echo ""
    echo "Foundry is required to build Solidity contracts."
    echo "To install Foundry, run:"
    echo ""
    echo "  curl -L https://foundry.paradigm.xyz | bash"
    echo "  source ~/.bashrc"
    echo "  foundryup"
    echo ""
    echo "After installation, run this build script again."
    exit 1
fi
print_info "Foundry found: $(forge --version | head -n 1)"

# Clean if requested
if [ "$CLEAN" = true ]; then
    print_step "Cleaning build artifacts..."
    cargo clean
    cd contracts/ethereum && forge clean && cd ../..
    print_info "Clean completed"
fi

# Format code if requested
if [ "$RUN_FORMAT" = true ]; then
    print_step "Formatting Rust code..."
    cargo fmt --all
    print_info "Rust formatting completed"
    
    print_step "Formatting Solidity code..."
    cd contracts/ethereum
    forge fmt
    cd ../..
    print_info "Solidity formatting completed"
fi

# Run clippy if requested
if [ "$RUN_CLIPPY" = true ]; then
    print_step "Running clippy..."
    cargo clippy --all-targets --all-features -- -D warnings
    print_info "Clippy checks passed"
fi

# Build Rust workspace
print_step "Building Rust workspace ($BUILD_MODE mode)..."
if [ "$BUILD_MODE" = "release" ]; then
    cargo build --release --workspace
else
    cargo build --workspace
fi
print_info "Rust build completed"

# Build Solidity contracts
print_step "Bootstrapping Foundry dependencies..."
chmod +x scripts/bootstrap-foundry-deps.sh
./scripts/bootstrap-foundry-deps.sh

print_step "Building Solidity contracts..."
cd contracts/ethereum
forge build
cd ../..
print_info "Solidity build completed"

# Run tests if requested
if [ "$RUN_TESTS" = true ]; then
    print_step "Running Rust tests..."
    if command -v cargo-nextest &> /dev/null; then
        cargo nextest run --workspace
    else
        cargo test --workspace
    fi
    print_info "Rust tests completed"
    
    print_step "Running Solidity tests..."
    cd contracts/ethereum
    forge test
    cd ../..
    print_info "Solidity tests completed"
fi

# Print summary
echo ""
echo "========================================="
print_info "Build completed successfully!"
echo "========================================="
echo ""
echo "Build mode: $BUILD_MODE"
if [ "$RUN_TESTS" = true ]; then
    echo "Tests: PASSED"
fi
if [ "$RUN_CLIPPY" = true ]; then
    echo "Clippy: PASSED"
fi
if [ "$RUN_FORMAT" = true ]; then
    echo "Format: APPLIED"
fi
echo ""

