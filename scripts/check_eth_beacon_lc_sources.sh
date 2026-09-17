#!/usr/bin/env bash
# Gate: the patches this repo hands to acki-nacki must be safe to apply to the
# deployed light client.
#
# It used to assert something else — that
# EthBeaconLightClient_rotate_decider.patch reproduced
# contracts/an/{EthBeaconLightClient,EthKeccak}.sol verbatim. That patch created
# contracts/exchange/EthBeaconLightClient.sol wholesale from this repo's copy,
# which stopped being an upgrade the moment acki-nacki began maintaining its own
# (shellnet runs the constant-sink variant with the constructor sender check;
# this repo's has a settable `_usdcBridge` and an extra field in the
# `updateCode` migration cell). Applying it would have unwired the sink and
# broken `onCodeUpgrade` decoding, and the gate would have called that correct.
#
# So the invariant is now about scope rather than equality:
#
#   1. every patch touches only contracts/exchange/ paths;
#   2. no patch carries this repo's local sink wiring into that tree, in either
#      direction (added or removed);
#   3. the EthKeccak patch moves their copy toward contracts/an/EthKeccak.sol —
#      every line it adds is in our file, every line it removes is not;
#   4. the notes patch adds comments and nothing else, which is what makes it
#      applicable without a redeploy (verified: the code hash does not move).
#
# Usage: ./scripts/check_eth_beacon_lc_sources.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'
  exit 0
fi

python3 - "$ROOT" <<'PY'
import sys
from pathlib import Path

root = Path(sys.argv[1])
KECCAK_PATCH = root / "EthKeccak_sold_fixes.patch"
NOTES_PATCH = root / "EthBeaconLightClient_encoding_and_gas_notes.patch"
AN_KECCAK = root / "contracts/an/EthKeccak.sol"

# Names that exist only because this repo can run the light client standalone.
# In the acki-nacki tree the sink is a constant and the constructor checks the
# sender, so a patch that mentions either side of that split is rewriting
# deployed wiring rather than delivering a fix.
LOCAL_ONLY = ("IAcceptedBlockHashSink", "_usdcBridge", "setUsdcBridge", '"0.1.0"')
THEIRS_ONLY = ("USDC_BRIDGE_ADDRESS", "eccUSDCBridge")

failures: list[str] = []


def fail(msg: str) -> None:
    failures.append(msg)


def hunks(patch_text: str):
    """(path, added, removed) per file in a unified diff."""
    path, added, removed = None, [], []
    for line in patch_text.splitlines():
        if line.startswith("+++ b/"):
            if path is not None:
                yield path, added, removed
                added, removed = [], []
            path = line[len("+++ b/") :]
        elif path is None or line.startswith(("--- ", "diff --git ", "index ", "@@", "\\")):
            continue
        elif line.startswith("+"):
            added.append(line[1:])
        elif line.startswith("-"):
            removed.append(line[1:])
    if path is not None:
        yield path, added, removed


for patch in (KECCAK_PATCH, NOTES_PATCH):
    if not patch.is_file():
        fail(f"missing {patch.name}")

if failures:
    for f in failures:
        print(f"FAIL: {f}", file=sys.stderr)
    sys.exit(1)

# 1 + 2: scope, and no wiring in either direction.
for patch in (KECCAK_PATCH, NOTES_PATCH):
    for path, added, removed in hunks(patch.read_text()):
        if not path.startswith("contracts/exchange/"):
            fail(f"{patch.name}: touches {path}, outside contracts/exchange/")
        for line in added + removed:
            for token in LOCAL_ONLY + THEIRS_ONLY:
                if token in line:
                    fail(f"{patch.name}: carries sink wiring ({token}): {line.strip()[:70]}")

# 3: the keccak patch moves their library toward ours.
ours = AN_KECCAK.read_text().splitlines()
ours_set = {ln.strip() for ln in ours if ln.strip()}
for path, added, removed in hunks(KECCAK_PATCH.read_text()):
    if path != "contracts/exchange/EthKeccak.sol":
        fail(f"EthKeccak_sold_fixes.patch: unexpected target {path}")
        continue
    for line in added:
        s = line.strip()
        # The pragma is theirs to keep; the two trees pin different floors.
        if s and not s.startswith("pragma ") and s not in ours_set:
            fail(f"adds a line contracts/an/EthKeccak.sol does not have: {s[:70]}")
    for line in removed:
        s = line.strip()
        if s and not s.startswith("pragma ") and s in ours_set:
            fail(f"removes a line contracts/an/EthKeccak.sol still has: {s[:70]}")

# 4: the notes patch is comments only.
for path, added, removed in hunks(NOTES_PATCH.read_text()):
    if removed:
        fail(f"notes patch deletes {len(removed)} line(s); it must only add comments")
    for line in added:
        s = line.strip()
        if s and not s.startswith("///") and not s.startswith("//"):
            fail(f"notes patch adds a non-comment line: {s[:70]}")

if failures:
    for f in failures:
        print(f"FAIL: {f}", file=sys.stderr)
    sys.exit(1)

print(
    "OK: both acki-nacki patches stay inside contracts/exchange/, carry no sink "
    "wiring, the keccak patch tracks contracts/an/EthKeccak.sol, and the notes "
    "patch is comments only."
)
PY
