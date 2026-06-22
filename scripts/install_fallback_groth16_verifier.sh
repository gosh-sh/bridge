#!/usr/bin/env bash
# Regenerate Fallback 1B gnark Groth16 verifier + proof from a Halo2 JSON export.
#
# Usage:
#   ./scripts/install_fallback_groth16_verifier.sh [halo2_proof.json]
#
# Default input: crates/bridge-prover-orchestrator/proofs/bound/fallback/halo2_proof.json
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PROOF_JSON="${1:-${ROOT}/crates/bridge-prover-orchestrator/proofs/bound/fallback/halo2_proof.json}"
WRAPPER="${ROOT}/crates/bridge-prover-orchestrator/gnark-wrappers/circuit-1b"
OUT_SOL="${ROOT}/contracts/ethereum/src/FallbackGroth16VerifierGenerated.sol"

if [[ ! -f "${PROOF_JSON}" ]]; then
  echo "missing Halo2 proof JSON: ${PROOF_JSON}" >&2
  echo "run export-bound-block-proofs first" >&2
  exit 1
fi

cd "${WRAPPER}"
go run . setup "${PROOF_JSON}"
go run . prove "${PROOF_JSON}"

sed 's/contract Verifier/contract FallbackGroth16VerifierGenerated/' Groth16Verifier.sol \
  | sed 's/pragma solidity 0.8.0;/pragma solidity ^0.8.19;/' \
  > "${OUT_SOL}"

cp groth16_proof.hex groth16_output.json "$(dirname "${PROOF_JSON}")/"

echo "OK: ${OUT_SOL}"
echo "OK: $(dirname "${PROOF_JSON}")/groth16_proof.hex"
