#!/usr/bin/env bash
# WD-Q3 — refuse production wiring of the identity-stub Groth16 withdrawal adapter.
#
# Production must use BridgeWithdrawalAggregatorVerifier (SHPLONK) only.
# `contracts/ethereum/src/BridgeWithdrawalVerifier.sol` is an R15 identity stub
# and must never appear in deploy scripts or as a constructor argument source.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

fail=0

echo "── WD-Q3: withdrawal verifier must not be the Groth16 stub ──"

# Exact stub — do not match IBridgeWithdrawalVerifier or *Aggregator*.
STUB_NEW='new[[:space:]]+BridgeWithdrawalVerifier[[:space:]]*\('

while IFS= read -r -d '' f; do
  if grep -E "$STUB_NEW" "$f" >/dev/null 2>&1; then
    echo "FAIL: $f instantiates BridgeWithdrawalVerifier (identity stub)" >&2
    fail=1
  fi
  # Import path ending in /BridgeWithdrawalVerifier.sol (not I* / *Aggregator*).
  if grep -E 'import[[:space:]].*/BridgeWithdrawalVerifier\.sol"' "$f" >/dev/null 2>&1; then
    echo "FAIL: $f imports BridgeWithdrawalVerifier.sol (identity stub)" >&2
    fail=1
  fi
done < <(find contracts/ethereum/script -name '*.sol' -print0 2>/dev/null || true)

if ! grep -q 'new BridgeWithdrawalAggregatorVerifier' \
  contracts/ethereum/script/ShplonkDeployLib.sol; then
  echo "FAIL: ShplonkDeployLib must deploy BridgeWithdrawalAggregatorVerifier" >&2
  fail=1
fi
if grep -E "$STUB_NEW" contracts/ethereum/script/ShplonkDeployLib.sol >/dev/null 2>&1; then
  echo "FAIL: ShplonkDeployLib must not deploy BridgeWithdrawalVerifier stub" >&2
  fail=1
fi

# Stub source was removed (#13). Missing file cannot be wired; if it reappears
# it must keep the FORBIDDEN NatSpec banner.
if [[ -f contracts/ethereum/src/BridgeWithdrawalVerifier.sol ]]; then
  if ! grep -q 'FORBIDDEN for production' contracts/ethereum/src/BridgeWithdrawalVerifier.sol; then
    echo "FAIL: BridgeWithdrawalVerifier.sol missing FORBIDDEN NatSpec banner" >&2
    fail=1
  fi
fi

if (( fail )); then
  echo "WD-Q3 check FAILED" >&2
  exit 1
fi

echo "WD-Q3 OK: production deploy path uses SHPLONK aggregator only."
