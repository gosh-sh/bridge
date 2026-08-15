#!/usr/bin/env bash
# TD-53 / DEP-E2E-SHELL — mock E2E recovery smoke (no live RPC / halo2).
#
# Exercises prove-one → finalize-one → restart path via cargo test td_53,
# then `deposit-relayer status` on a temp state file (no RPC).
#
# Live shellnet E-AN-01 remains ops (deferred). Wired in check_deposit_audit_gates.sh.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CRATE="$ROOT/crates/deposit-relayer-daemon"

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  cat <<'EOF'
TD-53 — mock E2E recovery smoke.

  ./scripts/td_53_dry_run_recovery_smoke.sh

Steps:
  1. cargo test td_53 (deposit-relayer-daemon)
  2. deposit-relayer status --state <temp>/state.json (no RPC)

Env:
  TD53_SKIP_CLI_STATUS=1  — skip CLI status step (tests only; not for CI gates)

CI: must pass (binary build + status). Live daemon --dry-run loop = E-AN-01 ops.

See: audit/reports/td-53-shellnet-e2e-runbook-notes.md
EOF
  exit 0
fi

echo "== TD-53 mock E2E recovery (cargo test td_53) =="
cd "$CRATE"
cargo test td_53 -- --nocapture

if [[ "${TD53_SKIP_CLI_STATUS:-0}" == "1" ]]; then
  echo "SKIP TD-53 CLI status (TD53_SKIP_CLI_STATUS=1)"
  echo "TD-53 dry-run smoke: OK (mock tests only)"
  exit 0
fi

echo "== TD-53 deposit-relayer status (no RPC) =="
STATE_DIR="$(mktemp -d)"
STATE_FILE="$STATE_DIR/state.json"
BIN="${CARGO_BIN_EXE_deposit-relayer:-$CRATE/target/debug/deposit-relayer}"

if [[ ! -x "$BIN" ]]; then
  if ! cargo build --bin deposit-relayer -q; then
    echo "ERROR: deposit-relayer binary build failed — TD-53 smoke requires status CLI" >&2
    echo "  Fix build or set TD53_SKIP_CLI_STATUS=1 for local test-only (not CI)" >&2
    rm -rf "$STATE_DIR"
    exit 1
  fi
  BIN="$CRATE/target/debug/deposit-relayer"
fi

if [[ ! -x "$BIN" ]]; then
  echo "ERROR: deposit-relayer binary missing at $BIN" >&2
  rm -rf "$STATE_DIR"
  exit 1
fi

"$BIN" --state "$STATE_FILE" status
rm -rf "$STATE_DIR"

echo "OK — TD-53 mock E2E recovery smoke (cargo test td_53 + status CLI; live E-AN-01 deferred)"
