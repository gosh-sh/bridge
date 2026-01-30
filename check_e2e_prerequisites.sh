#!/bin/bash
# Check Prerequisites for End-to-End Test

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONTRACTS_DIR="$SCRIPT_DIR/contracts/ethereum"

echo -e "${BLUE}╔════════════════════════════════════════════════════════════╗${NC}"
echo -e "${BLUE}║  Acki Nacki Bridge - Prerequisites Check                  ║${NC}"
echo -e "${BLUE}╚════════════════════════════════════════════════════════════╝${NC}"
echo ""

ERRORS=0
WARNINGS=0

# Check Rust
echo -n "Checking Rust... "
if command -v rustc &> /dev/null; then
    RUST_VERSION=$(rustc --version | awk '{print $2}')
    echo -e "${GREEN}✓${NC} (version $RUST_VERSION)"
else
    echo -e "${RED}✗ Not found${NC}"
    echo "  Install from: https://rustup.rs/"
    ((ERRORS++))
fi

# Check Cargo
echo -n "Checking Cargo... "
if command -v cargo &> /dev/null; then
    CARGO_VERSION=$(cargo --version | awk '{print $2}')
    echo -e "${GREEN}✓${NC} (version $CARGO_VERSION)"
else
    echo -e "${RED}✗ Not found${NC}"
    ((ERRORS++))
fi

# Check Foundry (forge)
echo -n "Checking Foundry (forge)... "
if command -v forge &> /dev/null; then
    FORGE_VERSION=$(forge --version | head -n1 | awk '{print $2}')
    echo -e "${GREEN}✓${NC} (version $FORGE_VERSION)"
else
    echo -e "${RED}✗ Not found${NC}"
    echo "  Install from: https://getfoundry.sh/"
    ((ERRORS++))
fi

# Check cast
echo -n "Checking cast... "
if command -v cast &> /dev/null; then
    echo -e "${GREEN}✓${NC}"
else
    echo -e "${RED}✗ Not found${NC}"
    echo "  Install Foundry from: https://getfoundry.sh/"
    ((ERRORS++))
fi

# Check jq
echo -n "Checking jq... "
if command -v jq &> /dev/null; then
    JQ_VERSION=$(jq --version)
    echo -e "${GREEN}✓${NC} ($JQ_VERSION)"
else
    echo -e "${RED}✗ Not found${NC}"
    echo "  Install: sudo apt-get install jq (Ubuntu) or brew install jq (macOS)"
    ((ERRORS++))
fi

# Check curl
echo -n "Checking curl... "
if command -v curl &> /dev/null; then
    echo -e "${GREEN}✓${NC}"
else
    echo -e "${RED}✗ Not found${NC}"
    ((ERRORS++))
fi

echo ""

# Check .env file
echo -n "Checking .env file... "
if [ -f "$CONTRACTS_DIR/.env" ]; then
    echo -e "${GREEN}✓${NC} Found"
    
    # Check required variables
    source "$CONTRACTS_DIR/.env"
    
    echo -n "  SEPOLIA_RPC_URL... "
    if [ -z "$SEPOLIA_RPC_URL" ]; then
        echo -e "${RED}✗ Not set${NC}"
        ((ERRORS++))
    else
        echo -e "${GREEN}✓${NC}"
    fi
    
    echo -n "  PRIVATE_KEY... "
    if [ -z "$PRIVATE_KEY" ]; then
        echo -e "${RED}✗ Not set${NC}"
        ((ERRORS++))
    else
        echo -e "${GREEN}✓${NC}"
        
        # Check if private key has 0x prefix
        if [[ ! "$PRIVATE_KEY" =~ ^0x ]]; then
            echo -e "    ${YELLOW}⚠${NC} Private key should start with 0x"
            ((WARNINGS++))
        fi
    fi
    
    echo -n "  ETHERSCAN_API_KEY... "
    if [ -z "$ETHERSCAN_API_KEY" ]; then
        echo -e "${YELLOW}⚠${NC} Not set (optional for testing)"
        ((WARNINGS++))
    else
        echo -e "${GREEN}✓${NC}"
    fi
else
    echo -e "${RED}✗ Not found${NC}"
    echo "  Create .env file in contracts/ethereum/ with:"
    echo "    SEPOLIA_RPC_URL=https://eth-sepolia.g.alchemy.com/v2/YOUR_KEY"
    echo "    PRIVATE_KEY=0x..."
    echo "    ETHERSCAN_API_KEY=YOUR_KEY"
    ((ERRORS++))
fi

echo ""

# Check RPC connectivity
if [ ! -z "$SEPOLIA_RPC_URL" ]; then
    echo -n "Checking Sepolia RPC connectivity... "
    BLOCK_NUMBER=$(cast block-number --rpc-url "$SEPOLIA_RPC_URL" 2>/dev/null || echo "")
    if [ ! -z "$BLOCK_NUMBER" ]; then
        echo -e "${GREEN}✓${NC} (block: $BLOCK_NUMBER)"
    else
        echo -e "${RED}✗ Failed to connect${NC}"
        echo "  Check your RPC URL and network connectivity"
        ((ERRORS++))
    fi
fi

# Check wallet balance
if [ ! -z "$PRIVATE_KEY" ] && [ ! -z "$SEPOLIA_RPC_URL" ]; then
    echo -n "Checking wallet balance... "
    WALLET_ADDRESS=$(cast wallet address "$PRIVATE_KEY" 2>/dev/null || echo "")
    if [ ! -z "$WALLET_ADDRESS" ]; then
        BALANCE=$(cast balance "$WALLET_ADDRESS" --rpc-url "$SEPOLIA_RPC_URL" 2>/dev/null || echo "0")
        BALANCE_ETH=$(cast --from-wei "$BALANCE" 2>/dev/null || echo "0")
        
        echo -e "${GREEN}✓${NC} $BALANCE_ETH ETH"
        echo "  Address: $WALLET_ADDRESS"
        
        # Check if balance is sufficient
        BALANCE_FLOAT=$(echo "$BALANCE_ETH" | awk '{print $1}')
        if (( $(echo "$BALANCE_FLOAT < 0.15" | bc -l) )); then
            echo -e "  ${YELLOW}⚠${NC} Low balance! Recommended: 0.15+ ETH for testing"
            echo "    Get Sepolia ETH from:"
            echo "    - https://sepoliafaucet.com/"
            echo "    - https://www.alchemy.com/faucets/ethereum-sepolia"
            ((WARNINGS++))
        fi
    else
        echo -e "${RED}✗ Failed to get wallet address${NC}"
        ((ERRORS++))
    fi
fi

echo ""

# Check Rust dependencies
echo -n "Checking deposit-prover dependencies... "
cd "$SCRIPT_DIR/deposit-prover"
if cargo check --quiet 2>/dev/null; then
    echo -e "${GREEN}✓${NC}"
else
    echo -e "${YELLOW}⚠${NC} Dependencies need to be built"
    echo "  This will happen automatically on first run"
    ((WARNINGS++))
fi

cd "$SCRIPT_DIR"

echo ""

# Check disk space
echo -n "Checking disk space... "
AVAILABLE_GB=$(df -BG . | tail -1 | awk '{print $4}' | sed 's/G//')
if [ "$AVAILABLE_GB" -gt 10 ]; then
    echo -e "${GREEN}✓${NC} ${AVAILABLE_GB}GB available"
else
    echo -e "${YELLOW}⚠${NC} ${AVAILABLE_GB}GB available (10GB+ recommended)"
    ((WARNINGS++))
fi

# Check RAM
echo -n "Checking RAM... "
if command -v free &> /dev/null; then
    TOTAL_RAM_GB=$(free -g | awk '/^Mem:/{print $2}')
    if [ "$TOTAL_RAM_GB" -ge 16 ]; then
        echo -e "${GREEN}✓${NC} ${TOTAL_RAM_GB}GB total"
    else
        echo -e "${YELLOW}⚠${NC} ${TOTAL_RAM_GB}GB total (16GB+ recommended for proof generation)"
        ((WARNINGS++))
    fi
elif command -v sysctl &> /dev/null; then
    # macOS
    TOTAL_RAM_BYTES=$(sysctl -n hw.memsize)
    TOTAL_RAM_GB=$((TOTAL_RAM_BYTES / 1024 / 1024 / 1024))
    if [ "$TOTAL_RAM_GB" -ge 16 ]; then
        echo -e "${GREEN}✓${NC} ${TOTAL_RAM_GB}GB total"
    else
        echo -e "${YELLOW}⚠${NC} ${TOTAL_RAM_GB}GB total (16GB+ recommended for proof generation)"
        ((WARNINGS++))
    fi
else
    echo -e "${YELLOW}⚠${NC} Unable to check"
fi

echo ""
echo -e "${BLUE}════════════════════════════════════════════════════════════${NC}"

# Summary
if [ $ERRORS -eq 0 ] && [ $WARNINGS -eq 0 ]; then
    echo -e "${GREEN}✅ All prerequisites met!${NC}"
    echo ""
    echo "You can now run the end-to-end test:"
    echo "  ./test_e2e.sh"
elif [ $ERRORS -eq 0 ]; then
    echo -e "${YELLOW}⚠ Prerequisites met with $WARNINGS warning(s)${NC}"
    echo ""
    echo "You can run the test, but be aware of the warnings above:"
    echo "  ./test_e2e.sh"
else
    echo -e "${RED}✗ $ERRORS error(s) and $WARNINGS warning(s) found${NC}"
    echo ""
    echo "Please fix the errors above before running the test."
    exit 1
fi

echo ""

