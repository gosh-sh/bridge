#!/usr/bin/env bash
# TD-65 / DEP-INTERFACE-REVERT-LOOP — mock G3 revert classification smoke (no RPC / shellnet).
#
# Wired in check_deposit_audit_gates.sh after TD-53 (RELAYER block).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CRATE="$ROOT/crates/deposit-relayer-daemon"

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  cat <<'EOF'
TD-65 — G3 Revert loop mock smoke (QC-OFF-06).

  ./scripts/check_td65_g3_smoke.sh

Runs:
  cargo test td_65 (8 tests — exit 51 → AlreadyFinalized, generic Revert → HOL)

Optional cross-ref (default on, <1s):
  cargo test --test f10_interface_reverted

Env:
  TD65_SKIP_F10_INTERFACE=1  — skip f10_interface_reverted cross-ref

Live shellnet / is_finalized API = ops (E-AN-01 deferred).

See: audit/reports/td-65-g3-live-notes.md
EOF
  exit 0
fi

echo "== TD-65 G3 mock smoke (cargo test td_65) =="
cd "$CRATE"
cargo test td_65 -- --nocapture

if [[ "${TD65_SKIP_F10_INTERFACE:-0}" != "1" ]]; then
  echo "== TD-65 cross-ref: f10_interface_reverted =="
  cargo test --test f10_interface_reverted -- --nocapture
else
  echo "SKIP f10_interface_reverted (TD65_SKIP_F10_INTERFACE=1)"
fi

echo "OK — TD-65 mock G3 classification smoke (live is_finalized / shellnet = ops)"
