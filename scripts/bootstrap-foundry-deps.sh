#!/usr/bin/env bash
# Install Foundry/npm deps that are gitignored after clone (Linux + macOS).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ETH="$ROOT/contracts/ethereum"

if ! command -v forge &>/dev/null; then
  echo "forge not found — run ./setup.sh first" >&2
  exit 1
fi

cd "$ETH"

if [[ ! -d lib/forge-std ]]; then
  echo "[bootstrap] Installing forge-std into contracts/ethereum/lib/ ..."
  forge install --no-git foundry-rs/forge-std
else
  echo "[bootstrap] forge-std already present"
fi

if [[ -f package.json ]]; then
  if [[ ! -d node_modules/poseidon-solidity ]]; then
    if ! command -v npm &>/dev/null; then
      echo "WARN: npm not found — poseidon-solidity tests may fail" >&2
    else
      echo "[bootstrap] npm install (poseidon-solidity) ..."
      npm install
    fi
  else
    echo "[bootstrap] npm deps already present"
  fi
fi

echo "[bootstrap] Foundry deps OK"
