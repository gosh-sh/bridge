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
VERBOSE=false
COVERAGE=false
UNIT_ONLY=false
INTEGRATION_ONLY=false
SOLIDITY_ONLY=false
RUST_ONLY=false
FILTER=""

while [[ $# -gt 0 ]]; do
    case $1 in
        -v|--verbose)
            VERBOSE=true
            shift
            ;;
        --coverage)
            COVERAGE=true
            shift
            ;;
        --unit)
            UNIT_ONLY=true
            shift
            ;;
        --integration)
            INTEGRATION_ONLY=true
            shift
            ;;
        --solidity)
            SOLIDITY_ONLY=true
            shift
            ;;
        --rust)
            RUST_ONLY=true
            shift
            ;;
        --filter)
            FILTER="$2"
            shift 2
            ;;
        --help)
            echo "Usage: ./test.sh [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  -v, --verbose       Run tests with verbose output"
            echo "  --coverage          Generate code coverage report"
            echo "  --unit              Run only unit tests"
            echo "  --integration       Run only integration tests"
            echo "  --solidity          Run only Solidity tests"
            echo "  --rust              Run only Rust tests"
            echo "  --filter <pattern>  Run only tests matching pattern"
            echo "  --help              Show this help message"
            echo ""
            echo "Examples:"
            echo "  ./test.sh                           # Run all tests"
            echo "  ./test.sh --verbose                 # Run with verbose output"
            echo "  ./test.sh --rust --filter merkle    # Run Rust tests matching 'merkle'"
            echo "  ./test.sh --coverage                # Generate coverage report"
            exit 0
            ;;
        *)
            print_error "Unknown option: $1"
            echo "Run './test.sh --help' for usage information"
            exit 1
            ;;
    esac
done

echo "=== Acki Nacki Bridge Test Suite ==="
echo ""

# Track test results
RUST_TESTS_PASSED=true
SOLIDITY_TESTS_PASSED=true

# Run Rust tests
if [ "$SOLIDITY_ONLY" = false ]; then
    print_step "Running Rust tests..."
    
    RUST_TEST_CMD="cargo"
    
    # Use nextest if available
    if command -v cargo-nextest &> /dev/null && [ "$COVERAGE" = false ]; then
        RUST_TEST_CMD="cargo nextest run"
    else
        RUST_TEST_CMD="cargo test"
    fi
    
    # Add filter if specified
    if [ -n "$FILTER" ]; then
        RUST_TEST_CMD="$RUST_TEST_CMD $FILTER"
    fi
    
    # Add verbose flag
    if [ "$VERBOSE" = true ]; then
        RUST_TEST_CMD="$RUST_TEST_CMD -- --nocapture --test-threads=1"
    fi
    
    # Run unit tests only
    if [ "$UNIT_ONLY" = true ]; then
        RUST_TEST_CMD="$RUST_TEST_CMD --lib"
    fi
    
    # Run integration tests only
    if [ "$INTEGRATION_ONLY" = true ]; then
        RUST_TEST_CMD="$RUST_TEST_CMD --test '*'"
    fi
    
    # Run with coverage
    if [ "$COVERAGE" = true ]; then
        print_info "Running tests with coverage..."
        if command -v cargo-tarpaulin &> /dev/null; then
            cargo tarpaulin --workspace --out Html --out Xml --output-dir coverage
            print_info "Coverage report generated in coverage/"
        else
            print_warn "cargo-tarpaulin not installed. Install with: cargo install cargo-tarpaulin"
            print_info "Running tests without coverage..."
            eval "$RUST_TEST_CMD --workspace" || RUST_TESTS_PASSED=false
        fi
    else
        eval "$RUST_TEST_CMD --workspace" || RUST_TESTS_PASSED=false
    fi
    
    if [ "$RUST_TESTS_PASSED" = true ]; then
        print_info "Rust tests completed successfully"
    else
        print_error "Rust tests failed"
    fi
fi

# Run Solidity tests
if [ "$RUST_ONLY" = false ]; then
    print_step "Running Solidity tests..."
    
    cd contracts/ethereum
    
    FORGE_TEST_CMD="forge test"
    
    # Add verbosity
    if [ "$VERBOSE" = true ]; then
        FORGE_TEST_CMD="$FORGE_TEST_CMD -vvv"
    fi
    
    # Add filter if specified
    if [ -n "$FILTER" ]; then
        FORGE_TEST_CMD="$FORGE_TEST_CMD --match-test $FILTER"
    fi
    
    # Run with coverage
    if [ "$COVERAGE" = true ]; then
        print_info "Running Solidity tests with coverage..."
        forge coverage --report summary
    else
        eval "$FORGE_TEST_CMD" || SOLIDITY_TESTS_PASSED=false
    fi
    
    cd ../..
    
    if [ "$SOLIDITY_TESTS_PASSED" = true ]; then
        print_info "Solidity tests completed successfully"
    else
        print_error "Solidity tests failed"
    fi
fi

# Print summary
echo ""
echo "========================================="
if [ "$RUST_TESTS_PASSED" = true ] && [ "$SOLIDITY_TESTS_PASSED" = true ]; then
    print_info "All tests PASSED!"
    echo "========================================="
    exit 0
else
    print_error "Some tests FAILED!"
    echo "========================================="
    if [ "$RUST_TESTS_PASSED" = false ]; then
        echo "  - Rust tests: FAILED"
    fi
    if [ "$SOLIDITY_TESTS_PASSED" = false ]; then
        echo "  - Solidity tests: FAILED"
    fi
    echo ""
    exit 1
fi

