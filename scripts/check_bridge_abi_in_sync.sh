#!/usr/bin/env bash
# Guard: the on-AN USDCBridge ABI ships in two runtime copies.
#
#   crates/bridge-prover-libraries/python/contracts/USDCBridge.abi.json   (tvm-cli)
#   crates/ackinacki-bridge/abi/USDCBridge.abi.json         (include_str!)
#
# Both must stay byte-identical and in sync with the deployed shellnet
# contract (acki-nacki @ cf664666b). This script only checks the two
# runtime copies agree with each other; drift versus the .sol is caught
# by scripts/check_voucher_abi_consistency.py.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
PY="$REPO/crates/bridge-prover-libraries/python/contracts/USDCBridge.abi.json"
RUST="$REPO/crates/ackinacki-bridge/abi/USDCBridge.abi.json"
cmp -s "$PY" "$RUST" || {
  echo "USDCBridge.abi.json drift between:"
  echo "  $PY"
  echo "  $RUST"
  diff -u "$PY" "$RUST" || true
  exit 1
}
echo "ok: python and Rust CLI USDCBridge.abi.json are byte-identical"
