#!/usr/bin/env bash
# Gate: EthBeaconLightClient_rotate_decider.patch must match
# contracts/an/{EthBeaconLightClient,EthKeccak}.sol (pragma lines ignored).
# The patch is what lands in acki-nacki under contracts/exchange/; the
# contracts/an/ copies are the source of truth in this repo.
#
# Usage:
#   ./scripts/check_eth_beacon_lc_sources.sh          # compare
#   ./scripts/check_eth_beacon_lc_sources.sh --write  # regenerate the patch
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
AN="${ROOT}/contracts/an"
PATCH="${ROOT}/EthBeaconLightClient_rotate_decider.patch"

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  cat <<'EOF'
Assert EthBeaconLightClient_rotate_decider.patch matches
contracts/an/{EthBeaconLightClient,EthKeccak}.sol (pragma ignored).

  --write   overwrite the patch from contracts/an/
EOF
  exit 0
fi

python3 - "$AN" "$PATCH" "${1:-}" <<'PY'
import hashlib, sys
from pathlib import Path

an_dir, patch_path, mode = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3]
FILES = [
    ("EthBeaconLightClient.sol", "contracts/exchange/EthBeaconLightClient.sol"),
    ("EthKeccak.sol", "contracts/exchange/EthKeccak.sol"),
]


def fail(msg: str) -> None:
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def strip_pragma(text: str) -> str:
    return "\n".join(
        line for line in text.splitlines() if not line.strip().startswith("pragma ")
    )


def extract_from_patch(patch: str) -> dict[str, str]:
    out: dict[str, list[str]] = {}
    current = None
    for line in patch.splitlines():
        if line.startswith("diff --git "):
            current = None
            continue
        if line.startswith("+++ b/"):
            current = line[len("+++ b/") :]
            out[current] = []
            continue
        if current is None:
            continue
        if line.startswith("+") and not line.startswith("+++"):
            out[current].append(line[1:])
        elif line.startswith("\\"):
            continue
    return {k: "\n".join(v) + ("\n" if v else "") for k, v in out.items()}


def new_file_diff(src: Path, dest: str) -> str:
    text = src.read_text()
    lines = text.splitlines(keepends=True)
    if lines and not lines[-1].endswith("\n"):
        lines[-1] += "\n"
        text += "\n"
    n = len(lines)
    body = "".join("+" + ln for ln in lines)
    digest = hashlib.sha1(text.encode()).hexdigest()[:9]
    return (
        f"diff --git a/{dest} b/{dest}\n"
        f"new file mode 100644\n"
        f"index 000000000..{digest}\n"
        f"--- /dev/null\n"
        f"+++ b/{dest}\n"
        f"@@ -0,0 +1,{n} @@\n"
        f"{body}"
    )


if mode == "--write":
    parts = [new_file_diff(an_dir / name, dest) for name, dest in FILES]
    patch_path.write_text("".join(parts))
    print(f"OK: wrote {patch_path.name} from contracts/an/")
    sys.exit(0)

if not patch_path.is_file():
    fail(f"missing {patch_path}")

extracted = extract_from_patch(patch_path.read_text())
for name, dest in FILES:
    src = an_dir / name
    if not src.is_file():
        fail(f"missing {src}")
    want = src.read_text()
    got = extracted.get(dest)
    if got is None:
        fail(f"patch has no {dest}")
    if strip_pragma(want) != strip_pragma(got):
        fail(
            f"{dest} drifted from contracts/an/{name} "
            f"(beyond pragma). Re-run: ./scripts/check_eth_beacon_lc_sources.sh --write"
        )

print(
    "OK: patch matches contracts/an/{EthBeaconLightClient,EthKeccak}.sol "
    "(pragma ignored)"
)
PY
