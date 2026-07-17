#!/usr/bin/env bash
# F9 night gate — audit ETH overlay @ FOUNDRY_PROFILE=ci (5000 fuzz / 1000 invariant).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT/contracts/ethereum"
npm install --silent 2>/dev/null || npm install
test -d lib/forge-std || forge install --no-git foundry-rs/forge-std
cd "$ROOT/audit/spec/ethereum"
echo "── ci_eth_audit_night: FOUNDRY_PROFILE=ci forge test ──"
FOUNDRY_PROFILE=ci forge test -vvv
