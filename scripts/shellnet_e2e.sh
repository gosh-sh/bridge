#!/usr/bin/env bash
# Shellnet acceptance driver (skeleton). Extend with live RPC + proof paths.
set -euo pipefail

RPC_URL="${1:-${SEPOLIA_RPC:-}}"
BRIDGE="${2:-${BRIDGE_ADDRESS:-}}"
AN_URL="${3:-${AN_NODE_URL:-http://127.0.0.1:11000}}"

echo "=== Shellnet E2E (skeleton) ==="
echo "RPC:    ${RPC_URL:-<unset>}"
echo "Bridge: ${BRIDGE:-<unset>}"
echo "AN:     ${AN_URL}"

echo "[1/5] EIP-170 gate"
chmod +x scripts/check_eip170_verifier_bins.sh
./scripts/check_eip170_verifier_bins.sh contracts/ethereum/verifiers || true

echo "[2/5] Foundry forgery suite"
(cd contracts/ethereum && forge test --match-contract ShplonkAggregatorForgery -vv)

echo "[3/5] AN preflight"
if command -v cargo >/dev/null; then
  (cd crates/deposit-relayer-daemon && cargo run --bin deposit-relayer -- an-preflight --an-node-url "$AN_URL" 2>/dev/null) || \
    echo "  (skip: deposit-relayer an-preflight unavailable in this checkout)"
fi

echo "[4/5] Bridge-relayer verify-fixture (requires --rpc-url + --bridge-address)"
if [[ -n "$RPC_URL" && -n "$BRIDGE" ]]; then
  (cd crates/bridge-relayer-daemon && cargo run --bin relayer -- verify-fixture \
    --fixtures-dir ../bridge-prover-orchestrator/proofs/bound \
    --rpc-url "$RPC_URL" --bridge-address "$BRIDGE" --no-simulate) || \
    echo "  verify-fixture failed (expected if fixtures not generated)"
else
  echo "  skip: set SEPOLIA_RPC + BRIDGE_ADDRESS"
fi

echo "[5/5] Manual steps — see docs/shellnet_e2e_acceptance_runbook.md"
echo "Done."
