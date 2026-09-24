#!/usr/bin/env bash
# Phase 0 production gates — run before any Sepolia/shellnet deploy.
#
# Usage:
#   ./scripts/production_preflight.sh
#   make production-preflight
set -euo pipefail

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

echo "--- [1/5] EIP-170 verifier sizes ---"
chmod +x scripts/check_eip170_verifier_bins.sh
if ! ./scripts/check_eip170_verifier_bins.sh "${VERIFIERS}"; then
  fail=1
fi

echo "--- [2/5] Required production verifyBlock artefacts (all SHPLONK) ---"
require_file "${VERIFIERS}/PrimaryAggregatorVerifier.bin" "Primary SHPLONK runtime"
require_file "${VERIFIERS}/FallbackAggregatorVerifier.bin" "Fallback SHPLONK runtime (K=21)"
require_file "${VERIFIERS}/LayerHashesAggregatorVerifier.bin" "LayerHashes SHPLONK runtime"
require_file "${VERIFIERS}/PrimaryAggregatorVerifier_calldata.bin" "Primary smoke calldata"
require_file "${VERIFIERS}/FallbackAggregatorVerifier_calldata.bin" "Fallback smoke calldata"
require_file "${VERIFIERS}/LayerHashesAggregatorVerifier_calldata.bin" "LayerHashes smoke calldata"

warn_missing "${VERIFIERS}/BridgeWithdrawalAggregatorVerifier.bin" "C4 withdrawal SHPLONK (Phase 2)"

echo "--- [3/5] Artefact manifest ---"
(
  cd "${VERIFIERS}"
  sha256sum *.bin 2>/dev/null || true
) | tee "${LOG_DIR}/verifiers.sha256"

echo "--- [4/5] Foundry production gate tests ---"
(
  cd contracts/ethereum
  forge test --match-contract "ShplonkAggregatorForgery|ShplonkDeployLib" -vv
  forge test --match-test test_productionPrimaryAttestation_isolated -vv
  forge test --match-test test_productionFallbackAttestation_isolated -vv
  # Full E2E (Primary 1A + Circuit 2 real SHPLONK aggregator proofs) is a hard gate since the
  # 2026-06-23 bound-witness fix; a regression must block deploy.
  forge test --match-test test_productionVerifyBlock_boundCalldata_advancesState -vv
  echo "OK: full production verifyBlock E2E (1A + 2 SHPLONK)"
)

echo "--- [5/5] Relayer unit tests ---"
(
  # The relayer builds only as a member of the prover workspace: its manifest
  # inherits dependencies from it, so cargo cannot run inside the crate itself.
  cd crates/bridge-prover-libraries
  cargo test --locked --quiet -p bridge-relayer-daemon
)

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
