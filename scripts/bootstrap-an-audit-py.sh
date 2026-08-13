#!/usr/bin/env bash
# Python venv for audit/spec/an pytest (no system pip required).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENV="$ROOT/.venv-an-audit"

if [[ ! -x "$VENV/bin/python" ]]; then
  echo "[bootstrap] creating $VENV"
  python3 -m venv "$VENV"
fi

"$VENV/bin/pip" install -q -U pip
"$VENV/bin/pip" install -q -r "$ROOT/audit/spec/an/requirements-dev.txt"
echo "[bootstrap] AN audit Python OK: $VENV/bin/python"
