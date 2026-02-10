#!/bin/bash
set -e

echo "=========================================="
echo "Installing Dependencies for Groth16 Wrapper"
echo "=========================================="

# Check if running with sudo
if [ "$EUID" -ne 0 ]; then 
    echo "Please run with sudo: sudo ./install_dependencies.sh"
    exit 1
fi

# Detect OS
if [ -f /etc/os-release ]; then
    . /etc/os-release
    OS=$ID
else
    echo "Cannot detect OS"
    exit 1
fi

echo "Detected OS: $OS"

# Install Go
echo ""
echo "Installing Go..."
if command -v go &> /dev/null; then
    GO_VERSION=$(go version | awk '{print $3}')
    echo "Go is already installed: $GO_VERSION"
else
    case $OS in
        ubuntu|debian)
            apt update
            apt install -y golang-go
            ;;
        fedora|rhel|centos)
            dnf install -y golang
            ;;
        arch)
            pacman -S --noconfirm go
            ;;
        *)
            echo "Unsupported OS: $OS"
            echo "Please install Go manually from https://go.dev/dl/"
            exit 1
            ;;
    esac
fi

# Verify Go installation
if command -v go &> /dev/null; then
    GO_VERSION=$(go version)
    echo "✓ Go installed successfully: $GO_VERSION"
else
    echo "✗ Go installation failed"
    exit 1
fi

# Install build-essential (for CGO)
echo ""
echo "Installing build tools for CGO..."
case $OS in
    ubuntu|debian)
        apt install -y build-essential
        ;;
    fedora|rhel|centos)
        dnf groupinstall -y "Development Tools"
        ;;
    arch)
        pacman -S --noconfirm base-devel
        ;;
esac

echo ""
echo "=========================================="
echo "✓ All dependencies installed successfully!"
echo "=========================================="
echo ""
echo "Next steps:"
echo "1. Run: ./setup_gnark.sh (as regular user, not sudo)"
echo "2. Run: cargo build --release"

