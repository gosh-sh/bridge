#!/usr/bin/env bash
# Fails if the fixture path cannot work on this machine. Run before
# scripts/deploy_msig_and_mint.sh; also runnable in CI.
set -euo pipefail

cli="${CLI_NAME:-$(command -v tvm-cli || true)}"
if [ -z "$cli" ]; then
  echo "FAIL: no tvm-cli on PATH and CLI_NAME is unset." >&2
  echo "      Install tvm-cli, or export CLI_NAME=/path/to/tvm-cli." >&2
  exit 1
fi
if ! "$cli" version >/dev/null 2>&1; then
  echo "FAIL: $cli is not executable on this platform:" >&2
  file "$cli" >&2 || true
  echo "      Export CLI_NAME=/path/to/a/native/tvm-cli." >&2
  exit 1
fi
echo "OK: tvm-cli = $cli ($("$cli" version 2>&1 | head -1))"
