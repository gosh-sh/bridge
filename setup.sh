#!/bin/bash
set -e

# Every path below is relative to the repository root.
cd "$(dirname "$0")"

echo "=== Acki Nacki Bridge Setup Script ==="
echo ""

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Function to print colored output
print_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

print_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Check if running on Linux or macOS
OS="$(uname -s)"
case "${OS}" in
    Linux*)     MACHINE=Linux;;
    Darwin*)    MACHINE=Mac;;
    *)          MACHINE="UNKNOWN:${OS}"
esac

print_info "Detected OS: ${MACHINE}"

# 1. Check and install Rust
print_info "Checking Rust installation..."
if ! command -v rustc &> /dev/null; then
    print_warn "Rust not found. Installing Rust..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
else
    print_info "Rust is already installed: $(rustc --version)"
fi

# Install the pinned toolchains with their components: rust-toolchain.toml at
# the root, and deposit-prover's own, older pin. The global default toolchain
# is left alone.
print_info "Installing the pinned Rust toolchains..."
rustup toolchain install
(cd deposit-prover && rustup toolchain install)

# 2. Check and install Foundry (for Solidity development)
print_info "Checking Foundry installation..."
if ! command -v forge &> /dev/null; then
    print_warn "Foundry not found. Installing Foundry..."
    curl -L https://foundry.paradigm.xyz | bash
    
    # Source foundry env
    if [ -f "$HOME/.foundry/bin/foundryup" ]; then
        "$HOME/.foundry/bin/foundryup"
        export PATH="$HOME/.foundry/bin:$PATH"
    else
        print_error "Foundry installation failed. Please install manually from https://book.getfoundry.sh/"
        exit 1
    fi
else
    print_info "Foundry is already installed: $(forge --version | head -n 1)"
fi

# Update Foundry
print_info "Updating Foundry..."
foundryup

# 3. Solidity compiler (solc)
print_info "Checking solc installation..."
if ! command -v solc &> /dev/null; then
    print_warn "solc not found. forge downloads the compiler it builds with; only regenerating"
    print_warn "verifiers and scripts/check_verifier_sources.sh need solc 0.8.19 on PATH."
else
    print_info "solc is already installed: $(solc --version | grep Version)"
fi

# 4. Node.js: contracts/ethereum takes poseidon-solidity from npm
print_info "Checking Node.js installation..."
if ! command -v npm &> /dev/null; then
    print_error "npm not found. contracts/ethereum needs it for poseidon-solidity."
    print_error "Install Node.js from: https://nodejs.org/"
    exit 1
fi
print_info "Node.js is already installed: $(node --version)"

# 5. Solidity dependencies — the same two steps as .woodpecker/solidity.yaml.
# Both land in gitignored directories; nothing tracked is touched.
print_info "Installing Solidity dependencies..."
(
    cd contracts/ethereum
    npm install
    test -d lib/forge-std || forge install --no-git foundry-rs/forge-std
)

# 6. Build Rust workspace to download dependencies
print_info "Building Rust workspace (this may take a while on first run)..."
cargo fetch

print_info "Running initial build check..."
if cargo check --workspace; then
    print_info "Rust workspace builds successfully"
else
    print_warn "Rust workspace has some issues, but dependencies are fetched"
fi

# 7. Install additional tools
print_info "Installing additional Rust tools..."

# cargo-nextest for better testing
if ! command -v cargo-nextest &> /dev/null; then
    print_info "Installing cargo-nextest..."
    cargo install cargo-nextest --locked
else
    print_info "cargo-nextest already installed"
fi

# cargo-watch for development
if ! command -v cargo-watch &> /dev/null; then
    print_info "Installing cargo-watch..."
    cargo install cargo-watch
else
    print_info "cargo-watch already installed"
fi

# cargo-audit for security audits
if ! command -v cargo-audit &> /dev/null; then
    print_info "Installing cargo-audit..."
    cargo install cargo-audit
else
    print_info "cargo-audit already installed"
fi

# 8. Setup git hooks (optional); an existing pre-commit hook is kept
if [ -d ".git" ] && [ -e ".git/hooks/pre-commit" ]; then
    print_warn "Keeping the existing .git/hooks/pre-commit"
elif [ -d ".git" ]; then
    print_info "Setting up git hooks..."
    mkdir -p .git/hooks
    
    cat > .git/hooks/pre-commit << 'EOF'
#!/bin/bash
# Pre-commit hook for Acki Nacki Bridge

echo "Running pre-commit checks..."

# Format check
echo "Checking Rust formatting..."
cargo fmt --all -- --check || {
    echo "Formatting issues found. Run 'cargo fmt --all' to fix."
    exit 1
}

# Clippy check
echo "Running clippy..."
cargo clippy --all-targets --all-features -- -D warnings || {
    echo "Clippy found issues. Please fix them."
    exit 1
}

# Solidity format check
echo "Checking Solidity formatting..."
cd contracts/ethereum
forge fmt --check || {
    echo "Solidity formatting issues found. Run 'forge fmt' to fix."
    exit 1
}
cd ../..

echo "Pre-commit checks passed!"
EOF
    
    chmod +x .git/hooks/pre-commit
    print_info "Git hooks installed"
fi

# 9. Print summary
echo ""
echo "========================================="
print_info "Setup completed successfully!"
echo "========================================="
echo ""
echo "Installed tools:"
echo "  - Rust: $(rustc --version)"
echo "  - Cargo: $(cargo --version)"
echo "  - Forge: $(forge --version | head -n 1)"
echo "  - Anvil: $(anvil --version)"
echo ""
echo "Next steps:"
echo "  1. Copy contracts/ethereum/.env.example to contracts/ethereum/.env and configure it"
echo "  2. Run './build.sh' to build the entire project"
echo "  3. Run './build.sh --test' to build and run all tests"
echo "  4. Run 'anvil' in a separate terminal to start a local Ethereum node"
echo "  5. Run 'make deploy-local' to deploy a test bridge to it"
echo "  6. Run 'make pre-push' before pushing"
echo ""
echo "Development commands:"
echo "  - 'cargo watch -x check' - Auto-rebuild on changes"
echo "  - 'cargo nextest run' - Run tests with nextest"
echo "  - 'cargo clippy' - Run linter"
echo "  - 'cargo fmt' - Format code"
echo "  - 'forge test -vvv' - Run Solidity tests with verbose output"
echo "  - 'anvil' - Start local Ethereum node for testing"
echo ""
print_info "Happy coding!"

