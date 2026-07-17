#!/usr/bin/env bash
# Generate BC-AN-01 PoC fixtures: two opcode-valid proofs for the SAME deposit
# witness (proof_00/input.json) with different dappId tags.
#
# Prerequisites (Hermez migration — see audit/knowledge/hermez_kzg_pins.md):
#   cd deposit-prover && ./download_trusted_setup.sh
#   → data/kzg_params_18.srs (Hermez; matches audit USDCBridge VK_BLOB 724687a4…)
#
# Output (gitignored):
#   audit/spec/an/fixtures/bc_an_01/dapp_a/{proof.bin,public_inputs.bin}
#   audit/spec/an/fixtures/bc_an_01/dapp_b/{proof.bin,public_inputs.bin}
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PROVER="$ROOT/deposit-prover"
SRC_INPUT="$PROVER/fixtures/deposit_10proofs/proof_00/input.json"
OUT="$ROOT/audit/spec/an/fixtures/bc_an_01"
HERMEZ_SRS="$PROVER/data/kzg_params_18.srs"
DAPP_A="${BC_AN_01_DAPP_A:-0x1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a}"
DAPP_B="${BC_AN_01_DAPP_B:-0x2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b}"

if [[ ! -f "$SRC_INPUT" ]]; then
  echo "Error: $SRC_INPUT not found" >&2
  exit 1
fi

if [[ ! -f "$HERMEZ_SRS" ]] || [[ ! -s "$HERMEZ_SRS" ]]; then
  echo "Hermez SRS missing — bootstrap (Google Cloud .ptau + convert)..."
  "$ROOT/scripts/bootstrap_hermez_srs_k18.sh"
fi

if [[ ! -f "$HERMEZ_SRS" ]]; then
  echo "Error: $HERMEZ_SRS still missing after download" >&2
  exit 1
fi

mkdir -p "$OUT"/{dapp_a,dapp_b,work}
WORK="$OUT/work"

prove_one() {
  local tag="$1" dapp_hex="$2" dest="$3"
  local inp="$WORK/input_${tag}.json"
  python3 - "$SRC_INPUT" "$inp" "$dapp_hex" <<'PY'
import json, sys
src, dst, dapp = sys.argv[1:4]
d = json.load(open(src))
h = dapp[2:] if dapp.startswith("0x") else dapp
d["dapp_id"] = list(bytes.fromhex(h.zfill(64)))
json.dump(d, open(dst, "w"))
PY
  echo "Proving $tag (dappId=$dapp_hex) ..."
  (
    cd "$PROVER"
    cargo run --release --example export_blake2b_proof -- \
      --input "$inp" \
      --proof-out "$dest/proof.bin" \
      --pubin-out "$dest/public_inputs.bin" \
      --degree 18 --max-data-byte-len 256 --max-log-num 20
  )
}

prove_one a "$DAPP_A" "$OUT/dapp_a"
prove_one b "$DAPP_B" "$OUT/dapp_b"

echo "BC-AN-01 fixtures ready under $OUT"
echo "Run: make audit-an-test"
