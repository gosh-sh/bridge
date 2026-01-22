#!/bin/bash
set -e

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

# Update to latest nightly
print_info "Updating to latest Rust nightly..."
rustup default nightly
rustup update

# Add required components
print_info "Adding Rust components..."
rustup component add rustfmt clippy

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

# 3. Install Solidity compiler (solc)
print_info "Checking solc installation..."
if ! command -v solc &> /dev/null; then
    print_warn "solc not found. Installing via Foundry..."
    # Foundry's forge will handle solc installation
else
    print_info "solc is already installed: $(solc --version | grep Version)"
fi

# 4. Check Node.js (optional, for some tooling)
print_info "Checking Node.js installation..."
if ! command -v node &> /dev/null; then
    print_warn "Node.js not found. It's optional but recommended for some tooling."
    print_warn "Install from: https://nodejs.org/"
else
    print_info "Node.js is already installed: $(node --version)"
fi

# 5. Initialize Foundry project for Ethereum contracts
print_info "Initializing Foundry project for Ethereum contracts..."
cd contracts/ethereum

if [ ! -d "lib" ]; then
    forge init --no-git --force .
    print_info "Foundry project initialized"
else
    print_info "Foundry project already initialized"
fi

# Install OpenZeppelin contracts
print_info "Installing OpenZeppelin contracts..."
if [ ! -d "lib/openzeppelin-contracts" ]; then
    forge install OpenZeppelin/openzeppelin-contracts --no-git
else
    print_info "OpenZeppelin contracts already installed"
fi

# Install forge-std (should be there from init, but ensure it's updated)
print_info "Ensuring forge-std is installed..."
if [ ! -d "lib/forge-std" ]; then
    forge install foundry-rs/forge-std --no-git
else
    cd lib/forge-std && git pull origin master || true
    cd ../..
fi

cd ../..

# 6. Create necessary directories
print_info "Creating project directories..."
mkdir -p crates/{eth-frontend,crypto,merkle-tree,zk-proofs,acki-nacki-interface}/src
mkdir -p contracts/ethereum/{src,test,script}
mkdir -p test/integration
mkdir -p docs

# 7. Build Rust workspace to download dependencies
print_info "Building Rust workspace (this may take a while on first run)..."
cargo fetch

print_info "Running initial build check..."
if cargo check --workspace; then
    print_info "Rust workspace builds successfully"
else
    print_warn "Rust workspace has some issues, but dependencies are fetched"
fi

# 8. Install additional tools
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

# 9. Setup git hooks (optional)
if [ -d ".git" ]; then
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

# 10. Create .env.example file
print_info "Creating .env.example file..."
cat > .env.example << 'EOF'
# Ethereum Configuration
ETH_RPC_URL=http://localhost:8545
ETH_CHAIN_ID=1337
ETH_PRIVATE_KEY=

# Contract Addresses (will be filled after deployment)
BRIDGE_CONTRACT_ADDRESS=

# Acki Nacki Configuration (placeholder)
ACKI_NACKI_RPC_URL=
ACKI_NACKI_CONTRACT_ADDRESS=

# Logging
RUST_LOG=info
EOF

print_info ".env.example created"

# 11. Print summary
echo ""
echo "========================================="
print_info "Setup completed successfully!"
echo "========================================="
echo ""
echo "Installed tools:"
echo "  - Rust: $(rustc --version)"
echo "  - Cargo: $(cargo --version)"
echo "  - Forge: $(forge --version | head -n 1)"
echo ""
echo "Next steps:"
echo "  1. Copy .env.example to .env and configure your settings"
echo "  2. Run 'cargo build' to build the Rust workspace"
echo "  3. Run 'cd contracts/ethereum && forge build' to build Solidity contracts"
echo "  4. Run 'cargo test' to run Rust tests"
echo "  5. Run 'cd contracts/ethereum && forge test' to run Solidity tests"
echo ""
echo "Development commands:"
echo "  - 'cargo watch -x check' - Auto-rebuild on changes"
echo "  - 'cargo nextest run' - Run tests with nextest"
echo "  - 'cargo clippy' - Run linter"
echo "  - 'cargo fmt' - Format code"
echo "  - 'forge test -vvv' - Run Solidity tests with verbose output"
echo ""
print_info "Happy coding!"

