#!/usr/bin/env bash
# Restore audit-overlay VK_BLOB in AN bridge Solidity after sync from acki-nacki.
# Target: USDCBridge.sol (audit overlay) or eccUSDCBridge.sol (acki-nacki @ contracts/bridge).
#
# Audit fixtures pin @ sha256 9dacd998… (12 PI, 5006 B, Hermez k=18).
# Upstream may ship 304c1c4e… or legacy audit overlay 724687a4… — TD-42 gate rejects drift.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
EXCHANGE="$ROOT/audit/spec/an-contracts/exchange"
if [[ -f "$EXCHANGE/USDCBridge.sol" ]]; then
  DEST="$EXCHANGE/USDCBridge.sol"
elif [[ -f "$EXCHANGE/eccUSDCBridge.sol" ]]; then
  DEST="$EXCHANGE/eccUSDCBridge.sol"
else
  echo "Error: no USDCBridge.sol or eccUSDCBridge.sol in $EXCHANGE" >&2
  exit 1
fi
PIN="$ROOT/deposit-prover/fixtures/deposit_10proofs/deposit_vk_blob.bin"
BACKUP="${AUDIT_VK_BLOB_BACKUP:-$ROOT/audit/spec/an-contracts/.audit_vk_blob.sol.snip}"

if [[ ! -f "$BACKUP" ]]; then
  if [[ ! -f "$PIN" ]]; then
    echo "Error: no VK backup ($BACKUP) and no $PIN" >&2
    exit 1
  fi
  echo "[preserve] creating backup snippet from current $(basename "$DEST") (first run)"
  python3 - "$DEST" "$BACKUP" <<'PY'
import re, sys
text = open(sys.argv[1], encoding="utf-8").read()
    m = re.search(
        r"(    // ZK verifying key.*?\n    bytes constant VK_BLOB =\s*\n(?:\s*hex\"[0-9a-fA-F]+\"\n)+\s*hex\"[0-9a-fA-F]+\";\n)",
        text,
        re.S,
    )
if not m:
    sys.exit("VK_BLOB block not found in bridge .sol")
open(sys.argv[2], "w", encoding="utf-8").write(m.group(1))
PY
fi

python3 - "$DEST" "$BACKUP" <<'PY'
import re, sys
dest, backup = sys.argv[1], sys.argv[2]
text = open(dest, encoding="utf-8").read()
snippet = open(backup, encoding="utf-8").read()
new, n = re.subn(
    r"    // ZK verifying key.*?\n    bytes constant VK_BLOB =\s*\n(?:\s*hex\"[0-9a-fA-F]+\"\n)+\s*hex\"[0-9a-fA-F]+\";\n",
    snippet,
    text,
    count=1,
    flags=re.S,
)
if n != 1:
    sys.exit(f"VK_BLOB replace failed (matches={n})")
open(dest, "w", encoding="utf-8").write(new)
PY

sha=$(sha256sum "$PIN" 2>/dev/null | awk '{print $1}' || true)
echo "[preserve] audit VK_BLOB restored in $(basename "$DEST")${sha:+ (pin file sha256=${sha:0:16}…)}"
