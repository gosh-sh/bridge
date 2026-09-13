#!/usr/bin/env bash
# ETH-6: committed SHPLONK .bin + _calldata.bin must exist, match SHA256SUMS,
# and stay under EIP-170. Pairing is a separate Foundry gate
# (ShplonkArtefactPairing.t.sol).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERIFIERS="${ROOT}/contracts/ethereum/verifiers"
SUMS="${VERIFIERS}/SHA256SUMS"
MAX=24576

if [[ ! -f "${SUMS}" ]]; then
  echo "ETH-6 FAIL: missing ${SUMS}" >&2
  exit 1
fi

required=(
  BridgeWithdrawalAggregatorVerifier.bin
  BridgeWithdrawalAggregatorVerifier_calldata.bin
  FallbackAggregatorVerifier.bin
  FallbackAggregatorVerifier_calldata.bin
  LayerHashesAggregatorVerifier.bin
  LayerHashesAggregatorVerifier_calldata.bin
  PrimaryAggregatorVerifier.bin
  PrimaryAggregatorVerifier_calldata.bin
)

fail=0
for f in "${required[@]}"; do
  path="${VERIFIERS}/${f}"
  if [[ ! -s "${path}" ]]; then
    echo "ETH-6 FAIL: missing or empty ${path}" >&2
    fail=1
  fi
done
if (( fail )); then
  exit 1
fi

echo "--- SHA256SUMS ---"
(
  cd "${VERIFIERS}"
  sha256sum -c SHA256SUMS
)

echo "--- EIP-170 (runtime .bin only) ---"
for f in \
  BridgeWithdrawalAggregatorVerifier.bin \
  FallbackAggregatorVerifier.bin \
  LayerHashesAggregatorVerifier.bin \
  PrimaryAggregatorVerifier.bin
do
  path="${VERIFIERS}/${f}"
  size=$(wc -c <"${path}" | tr -d ' ')
  if (( size > MAX )); then
    echo "EIP-170 FAIL: ${f} is ${size} bytes (limit ${MAX})" >&2
    fail=1
  else
    echo "EIP-170 OK:   ${f} (${size} bytes)"
  fi
done

if (( fail )); then
  exit 1
fi

echo "ETH-6 artefact pin OK."
