#!/usr/bin/env bash
# Phase 0 production gates — run before any Sepolia/shellnet deploy.
#
# Usage:
#   ./scripts/production_preflight.sh
#   ./scripts/production_preflight.sh --help
#   make production-preflight
set -euo pipefail

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  cat <<'EOF'
Production preflight — EIP-170, verifier artefacts, Foundry gates, relayer tests.

TD-16 negative (must fail):
  CHAIN_ID=11155111 PROFILE=prod ./scripts/td_16_prod_no_sepolia.sh

See also: scripts/td_16_prod_no_sepolia.sh --help
EOF
  exit 0
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "${ROOT}"

VERIFIERS="${ROOT}/contracts/ethereum/verifiers"
LOG_DIR="${ROOT}/logs/production_preflight_$(date +%Y%m%d_%H%M%S)"
mkdir -p "${LOG_DIR}"

exec > >(tee -a "${LOG_DIR}/preflight.log") 2>&1

echo "=== Production preflight $(date -Is) ==="
echo "Log: ${LOG_DIR}/preflight.log"

fail=0
warn=0

require_file() {
  local f="$1"
  local label="$2"
  if [[ ! -f "${f}" ]]; then
    echo "FAIL: missing ${label}: ${f}" >&2
    fail=1
  else
    echo "OK: ${label} (${f})"
  fi
}

warn_missing() {
  local f="$1"
  local label="$2"
  if [[ ! -f "${f}" ]]; then
    echo "WARN: ${label} not present (expected later phase): ${f}" >&2
    warn=1
  else
    echo "OK: ${label}"
  fi
}

echo "--- [1/5] ETH-6 SHPLONK artefact pin (hashes + EIP-170) ---"
chmod +x scripts/check_shplonk_artefacts.sh
if ! ./scripts/check_shplonk_artefacts.sh; then
  fail=1
fi

echo "--- [2/5] Required production verifyBlock artefacts (covered by SHA256SUMS) ---"
require_file "${VERIFIERS}/PrimaryAggregatorVerifier.bin" "Primary SHPLONK runtime"
require_file "${VERIFIERS}/FallbackAggregatorVerifier.bin" "Fallback SHPLONK runtime (K=21)"
require_file "${VERIFIERS}/LayerHashesAggregatorVerifier.bin" "LayerHashes SHPLONK runtime"
require_file "${VERIFIERS}/PrimaryAggregatorVerifier_calldata.bin" "Primary smoke calldata"
require_file "${VERIFIERS}/FallbackAggregatorVerifier_calldata.bin" "Fallback smoke calldata"
require_file "${VERIFIERS}/LayerHashesAggregatorVerifier_calldata.bin" "LayerHashes smoke calldata"
require_file "${VERIFIERS}/BridgeWithdrawalAggregatorVerifier.bin" "C4 withdrawal SHPLONK"
require_file "${VERIFIERS}/BridgeWithdrawalAggregatorVerifier_calldata.bin" "C4 smoke calldata"

echo "--- [3/5] Foundry production pairing gate (ETH-6 Circuit 4; 1A/1B/C2 quarantined) ---"
if ! (
  cd contracts/ethereum
  forge test --match-contract ShplonkArtefactPairing -vv
); then
  fail=1
fi
# Do not run ShplonkArtefactPairingPendingN14 here: those three pairs are
# desynced until n14 regen. SHA256SUMS still pins the files; WIRE_VERIFY_BLOCK
# on mainnet requires that contract green first.

echo "--- [4/5] Relayer unit tests ---"
(
  cd crates/bridge-relayer-daemon
  cargo test --quiet
)

echo "--- [5/5] TD-16 prod deposit chain policy (prod ∩ testnet = ∅) ---"
chmod +x scripts/td_16_prod_no_sepolia.sh
if ! ./scripts/td_16_prod_no_sepolia.sh; then
  fail=1
fi

echo "=== Preflight summary ==="
if (( fail )); then
  echo "RESULT: FAIL (blocking issues — do not deploy)"
  exit 1
fi
if (( warn )); then
  echo "RESULT: PASS WITH WARNINGS (verifyBlock deploy OK; withdrawal/deposit phases incomplete)"
  exit 0
fi
echo "RESULT: PASS (all gates green)"
exit 0
