#!/bin/sh
# Guard: the on-AN USDCBridge ABI ships in two runtime copies.
#
#   crates/bridge-prover-libraries/python/contracts/USDCBridge.abi.json   (tvm-cli)
#   crates/ackinacki-bridge/abi/USDCBridge.abi.json         (include_str!)
#
# Both copies must match the ABI generated for eccBridge.
set -eu
REPO="$(cd "$(dirname "$0")/.." && pwd)"
PY="$REPO/crates/bridge-prover-libraries/python/contracts/USDCBridge.abi.json"
RUST="$REPO/crates/ackinacki-bridge/abi/USDCBridge.abi.json"
COMPILED="$REPO/contracts/an/0.80.1_compiled/exchange/eccBridge.abi.json"
for copy in "$PY" "$RUST"; do
  diff -u "$COMPILED" "$copy"
done
echo "ok: Python and Rust bridge ABIs match the compiled eccBridge ABI"
