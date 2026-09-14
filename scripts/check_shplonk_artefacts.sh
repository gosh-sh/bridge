#!/usr/bin/env bash
# ETH-6: committed SHPLONK .bin + _calldata.bin must exist, match SHA256SUMS,
# and stay under EIP-170. Pairing is a separate Foundry gate
# (ShplonkArtefactPairing.t.sol).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERIFIERS="${ROOT}/contracts/ethereum/verifiers"
SUMS="${VERIFIERS}/SHA256SUMS"
SIZES="${VERIFIERS}/SIZES"
MAX=24576
# ETH-21: warn well before the cliff. A verifier past EIP-170 does not fail
# loudly — `CREATE` returns the zero address and `deployYulFromBin` reverts
# `YulDeployFailed` — so the useful signal is growth, not the breach. Layer
# hashes sits at 94% after the k_outer=21 regen while the other three keep
# triple the margin.
WARN_PCT=90

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
warn=0
for f in \
  BridgeWithdrawalAggregatorVerifier.bin \
  FallbackAggregatorVerifier.bin \
  LayerHashesAggregatorVerifier.bin \
  PrimaryAggregatorVerifier.bin
do
  path="${VERIFIERS}/${f}"
  size=$(wc -c <"${path}" | tr -d ' ')
  pct=$(( size * 100 / MAX ))
  if (( size > MAX )); then
    echo "EIP-170 FAIL: ${f} is ${size} bytes (limit ${MAX})" >&2
    fail=1
  elif (( pct >= WARN_PCT )); then
    echo "EIP-170 WARN: ${f} (${size} bytes, ${pct}% of ${MAX}, $(( MAX - size )) B left)" >&2
    warn=1
  else
    echo "EIP-170 OK:   ${f} (${size} bytes, ${pct}%)"
  fi
done

# ETH-21: sizes are recorded so growth lands in a diff a reviewer sees, rather
# than in a deploy that reverts. Regenerating an artefact means updating SIZES
# in the same commit, next to SHA256SUMS.
echo "--- recorded sizes ---"
if [[ ! -f "${SIZES}" ]]; then
  echo "ETH-21 FAIL: missing ${SIZES}" >&2
  fail=1
else
  actual=$(cd "${VERIFIERS}" && wc -c *.bin | grep -v ' total$' | awk '{printf "%s  %s\n", $1, $2}' | sort -k2)
  recorded=$(grep -v '^\s*#' "${SIZES}" | grep -v '^\s*$' | awk '{printf "%s  %s\n", $1, $2}' | sort -k2)
  if [[ "${actual}" != "${recorded}" ]]; then
    echo "ETH-21 FAIL: sizes drifted from ${SIZES##*/}; update it in this commit" >&2
    diff <(echo "${recorded}") <(echo "${actual}") | sed 's/^/  /' >&2 || true
    fail=1
  else
    echo "SIZES matches all $(echo "${actual}" | wc -l) artefacts."
  fi
fi

if (( fail )); then
  exit 1
fi

if (( warn )); then
  echo "ETH-6 artefact pin OK (with EIP-170 warnings above)."
else
  echo "ETH-6 artefact pin OK."
fi
