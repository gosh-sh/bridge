#!/usr/bin/env bash
# TD-43 / DEP-MOCK-VS-REAL — CI smoke: MockProver green ≠ SHPLONK opcode triple.
#
# Runs deposit-prover/td_43_mock_vs_shplonk (6 tests, incl. T2-1 all fixtures). Requires TD-42 VkBlob pin
# (check_vk_srs_pin.sh) to run first in check_deposit_audit_gates.sh.
#
# Skip (exit 0) when neither fixture Blake2b triple nor Hermez SRS is available.
# Env: DEPOSIT_KZG_SRS overrides default data/kzg_params_18.srs path.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PROVER="${ROOT}/deposit-prover"
PROOF00="${PROVER}/fixtures/deposit_10proofs/proof_00/proof.bin"
PUBIN00="${PROVER}/fixtures/deposit_10proofs/proof_00/public_inputs.bin"
SRS="${DEPOSIT_KZG_SRS:-${PROVER}/data/kzg_params_18.srs}"

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  cat <<'EOF'
TD-43 — MockProver vs SHPLONK opcode triple smoke.

  ./scripts/check_mock_vs_shplonk_smoke.sh

Runs: cd deposit-prover && cargo test --test td_43_mock_vs_shplonk

Skip (exit 0) when proof_00 fixture triple is missing AND Hermez SRS is absent.
Fixture path (preferred): deposit-prover/fixtures/deposit_10proofs/proof_00/
  proof.bin + public_inputs.bin (384 B) — no SRS needed.

SRS fallback: deposit-prover/data/kzg_params_18.srs
  or DEPOSIT_KZG_SRS=/path/to/kzg_params_18.srs
  Bootstrap: scripts/bootstrap_hermez_srs_k18.sh

See: audit/reports/td-43-mock-vs-shplonk-notes.md
EOF
  exit 0
fi

have_fixture=0
if [[ -f "$PROOF00" && -f "$PUBIN00" ]]; then
  have_fixture=1
fi

have_srs=0
if [[ -f "$SRS" ]]; then
  have_srs=1
fi

if [[ "$have_fixture" -eq 0 && "$have_srs" -eq 0 ]]; then
  echo "SKIP TD-43 smoke: no proof_00 fixture and no KZG SRS"
  echo "  fixture: $PROOF00"
  echo "  SRS: $SRS (or set DEPOSIT_KZG_SRS)"
  echo "  bootstrap: scripts/bootstrap_hermez_srs_k18.sh"
  exit 0
fi

if [[ "$have_fixture" -eq 1 ]]; then
  echo "TD-43 smoke: using fixture proof_00 (SRS optional)"
else
  echo "TD-43 smoke: generating Blake2b triple (SRS $SRS)"
fi

cd "$PROVER"
cargo test --test td_43_mock_vs_shplonk -- --nocapture

echo "OK — TD-43 MockProver vs SHPLONK opcode triple smoke passed"
