#!/usr/bin/env bash
# CI helper: AN audit pytest (unit always; integration when fixtures + tools available).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

./scripts/setup_an_audit_tools.sh

# Contracts + fixtures from acki-nacki when not present locally.
if [[ -z "${SKIP_AN_SYNC:-}" ]]; then
  ./scripts/sync_an_contracts.sh
fi

cd audit/spec/an-contracts
chmod +x ./build.sh
./build.sh

cd "$ROOT/audit/spec/an"
if [[ "${AN_AUDIT_INTEGRATION:-1}" == "1" ]] && [[ -f fixtures/deposit_10proofs/proof_00/proof.bin ]]; then
  python3 -m pytest -q
else
  echo "Running unit tests only (no deposit fixtures or AN_AUDIT_INTEGRATION=0)"
  python3 -m pytest unit/ -q
fi
