#!/bin/bash

set -e

echo "🦀 Setting up Acki Nacki Bridge Frontend..."

# Check if Rust is installed
if ! command -v cargo &> /dev/null; then
    echo "❌ Rust is not installed. Installing..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    source $HOME/.cargo/env
fi

echo "✅ Rust is installed"

# Check if wasm32 target is installed
if ! rustup target list | grep -q "wasm32-unknown-unknown (installed)"; then
    echo "📦 Installing wasm32 target..."
    rustup target add wasm32-unknown-unknown
fi

echo "✅ wasm32 target is installed"

# Check if trunk is installed
if ! command -v trunk &> /dev/null; then
    echo "📦 Installing Trunk..."
    cargo install trunk
fi

echo "✅ Trunk is installed"

echo ""
echo "🎉 Setup complete!"
echo ""
echo "To start the development server:"
echo "  cd frontend"
echo "  trunk serve"
echo ""
echo "The app will be available at http://localhost:8080"

