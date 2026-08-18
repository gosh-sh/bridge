#!/usr/bin/env bash
# Shellnet / Sepolia acceptance driver — Phase 0 gates + optional live RPC checks.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "${ROOT}"

RPC_URL="${1:-${SEPOLIA_RPC:-}}"
BRIDGE="${2:-${BRIDGE_ADDRESS:-}}"
AN_URL="${3:-${AN_NODE_URL:-http://127.0.0.1:11000}}"

echo "=== Shellnet E2E ==="
echo "RPC:    ${RPC_URL:-<unset>}"
echo "Bridge: ${BRIDGE:-<unset>}"
echo "AN:     ${AN_URL}"

echo "[1/4] Production preflight (Phase 0 gates)"
chmod +x scripts/production_preflight.sh
./scripts/production_preflight.sh

echo "[2/4] AN preflight"
if command -v cargo >/dev/null; then
  (cd crates/deposit-relayer-daemon && cargo run --bin deposit-relayer -- an-preflight --an-node-url "$AN_URL" 2>/dev/null) || \
    echo "  (skip: deposit-relayer an-preflight unavailable or AN unreachable)"
fi

echo "[3/4] Bridge-relayer verify-fixture (SHPLONK calldata auto-detected)"
if [[ -n "$RPC_URL" && -n "$BRIDGE" ]]; then
  (cd crates/bridge-relayer-daemon && cargo run --bin relayer -- verify-fixture \
    --fixtures-dir ../bridge-snark-utils/proofs/bound \
    --rpc-url "$RPC_URL" --bridge-address "$BRIDGE" --no-simulate) || \
    echo "  verify-fixture failed (deploy bridge first or check anchors)"
else
  echo "  skip: set SEPOLIA_RPC + BRIDGE_ADDRESS for live anchor check"
fi

echo "[4/4] Manual steps — see the deploy section of docs/ETH-contracts-spec.md"
echo "Done."
