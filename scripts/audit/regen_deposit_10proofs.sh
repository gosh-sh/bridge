#!/usr/bin/env bash
# QC-PROV-02 — regen deposit_10proofs inputs + proofs on heavy host (n14).
# Requires: chain SRS at deposit-prover/params/kzg_bn254_18.srs (opcode-aligned).
set -euo pipefail
ROOT="$(cd "$(dirname "${0}")/../.." && pwd)"
PROVER="$ROOT/deposit-prover"
COUNT="${DEPOSIT_PROOF_COUNT:-10}"
SET_DIR="${DEPOSIT_PROOF_SET_DIR:-fixtures/deposit_10proofs}"

cd "$PROVER"

echo "── axiom-eth pin (Cargo.toml rev) ──"
grep -A1 'name = "axiom-eth"' Cargo.lock | head -3 || true

echo "── regen input.json (synthetic legacy RLP) ──"
cargo run --release --example regen_deposit_10proofs_inputs -- \
  --set-dir "$SET_DIR" --count "$COUNT"

if [[ ! -f params/kzg_bn254_18.srs ]]; then
  echo "WARN: params/kzg_bn254_18.srs missing — trying Hermez fallback data/kzg_params_18.srs"
  if [[ ! -f data/kzg_params_18.srs ]]; then
    "$ROOT/scripts/bootstrap_hermez_srs_k18.sh"
  fi
fi

echo "── export proof set (k=18; may take several minutes) ──"
cargo run --release --example export_deposit_proof_set -- \
  --set-dir "$SET_DIR" --count "$COUNT" \
  --degree 18 --max-data-byte-len 256 --max-log-num 20

echo "── sync to audit overlay ──"
AUDIT_FIX="$ROOT/audit/spec/an/fixtures/deposit_10proofs"
mkdir -p "$AUDIT_FIX"
rsync -a --delete "$SET_DIR/" "$AUDIT_FIX/"

echo "── invalidate stale USDCBridge.tvc (VK_BLOB embedded at compile time) ──"
rm -f "$ROOT/audit/spec/an-contracts/build/USDCBridge.tvc" \
      "$ROOT/audit/spec/an-contracts/build/USDCBridge.abi.json"

echo "── smoke: MockProver on proof_00 ──"
cargo test real_fixture_satisfies_mock_prover -- --nocapture

echo "[OK] $SET_DIR regenerated ($COUNT proofs)"
