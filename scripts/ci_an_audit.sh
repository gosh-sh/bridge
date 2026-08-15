#!/usr/bin/env bash
# CI helper: AN audit pytest (unit always; integration when fixtures + tools available).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

chmod +x scripts/check_pi_count_docs.sh
./scripts/check_pi_count_docs.sh

./scripts/setup_an_audit_tools.sh
./scripts/bootstrap-an-audit-py.sh
./scripts/sync_audit_deposit_fixtures.sh

# Contracts + fixtures from acki-nacki (default: sync on each CI run).
if [[ -z "${SKIP_AN_SYNC:-}" ]]; then
  ./scripts/sync_an_contracts.sh
fi

cd audit/spec/an-contracts
chmod +x ./build.sh
./build.sh

# TD-42 / TD-04 — VkBlob pin + overlay patch matrix (runs even when AN_AUDIT_INTEGRATION=0).
chmod +x "$ROOT/scripts/check_vk_srs_pin.sh" "$ROOT/scripts/check_an_overlay_patch_matrix.sh"
"$ROOT/scripts/check_vk_srs_pin.sh"
"$ROOT/scripts/check_an_overlay_patch_matrix.sh"

cd "$ROOT/audit/spec/an"
if [[ "${AN_AUDIT_INTEGRATION:-1}" == "1" ]] && [[ -f fixtures/deposit_10proofs/proof_00/proof.bin ]]; then
  "$ROOT/.venv-an-audit/bin/python" -m pytest -q
else
  echo "Running unit tests only (no deposit fixtures or AN_AUDIT_INTEGRATION=0)"
  "$ROOT/.venv-an-audit/bin/python" -m pytest unit/ -q
fi
