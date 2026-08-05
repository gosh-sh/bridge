#!/usr/bin/env bash
# Regenerate paired LayerHashes SHPLONK .bin + _calldata.bin from Poseidon snark.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SNARK="${1:-${ROOT}/crates/bridge-snark-utils/proofs/bound/poseidon-snark/layer_hashes.snark}"
OUT="${ROOT}/contracts/ethereum/verifiers"

if [[ ! -f "${SNARK}" ]]; then
  echo "missing ${SNARK} — run export-bound-poseidon-snarks first" >&2
  exit 1
fi

# gen_srs reads cwd-relative params/kzg_bn254_{k}.srs (K=22 for layer).
cd "${ROOT}/crates/bridge-evm-aggregator"
cargo +nightly run --release --locked --bin export-inner-aggregator -- \
  --inner-snark "${SNARK}" \
  --out-dir "${OUT}" \
  --name LayerHashesAggregatorVerifier

echo "OK: ${OUT}/LayerHashesAggregatorVerifier.bin + _calldata.bin"
