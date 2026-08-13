#!/usr/bin/env bash
# Compile AN bridge contracts listed in contracts_manifest.json.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BUILD_DIR="$SCRIPT_DIR/build"
TOOLS_DIR="$SCRIPT_DIR/tools"
MANIFEST="$SCRIPT_DIR/contracts_manifest.json"

SOLD="${SOLD:-$TOOLS_DIR/sold}"
TVM_VERSION="${TVM_VERSION:-gosh}"

if [[ ! -x "$SOLD" ]]; then
  echo "Error: sold not found at $SOLD (run scripts/setup_an_audit_tools.sh)" >&2
  exit 1
fi

mkdir -p "$BUILD_DIR"

python3 - "$MANIFEST" <<'PY'
import json, subprocess, sys
from pathlib import Path

manifest_path = Path(sys.argv[1])
data = json.loads(manifest_path.read_text())
contracts = data.get("contracts") or []
if not contracts:
    print("No contracts in manifest — run scripts/sync_an_contracts.sh first.")
    sys.exit(0)

root = manifest_path.parent
build = root / "build"
sold = Path(__import__("os").environ.get("SOLD", root / "tools" / "sold"))
tvm_version = __import__("os").environ.get("TVM_VERSION", "gosh")
fail = 0

for item in contracts:
    if isinstance(item, str):
        name, sub = item, ""
    else:
        name, sub = item["name"], item.get("source", "")
    src = root / sub / f"{name}.sol" if sub else root / f"{name}.sol"
    print(f"Compiling {name} ({src})...")
    cmd = [
        str(sold), "--tvm-version", tvm_version,
        "--base-path", str(root),
        str(src), "-o", str(build),
    ]
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode == 0:
        rm = build / f"{name}.debug.json"
        if rm.exists():
            rm.unlink()
        print("  OK")
    else:
        print("  FAILED")
        print(r.stderr)
        fail = 1

sys.exit(fail)
PY
