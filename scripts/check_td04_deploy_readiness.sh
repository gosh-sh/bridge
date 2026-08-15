#!/usr/bin/env bash
# TD-04 — repo deploy readiness for owner (eccUSDCBridge + gates). Does NOT deploy to AN.
#
# Exit 0 = bridge repo ready for owner/partner to deploy paired eccUSDCBridge + DepositVoucher.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  cat <<'EOF'
TD-04 AN deploy readiness (repo only — no cluster deploy).

  ./scripts/check_td04_deploy_readiness.sh

Steps:
  1. check_deposit_audit_gates.sh (TD-68, TD-42, TD-43, TD-49, TD-53, TD-04 matrix)
  2. audit/spec/an-contracts build.sh → eccUSDCBridge.tvc + DepositVoucher.tvc
  3. check_voucher_abi_consistency.py (overlay sources, 6-arg DEP-N-5)
  4. cargo test td_04 (deposit-relayer-daemon)

Owner handoff: docs/operations/td-04-an-deploy-owner-handoff.md
EOF
  exit 0
fi

echo "== TD-04 readiness: deposit audit gates =="
bash "$ROOT/scripts/check_deposit_audit_gates.sh"

echo "== TD-04 readiness: overlay build =="
cd "$ROOT/audit/spec/an-contracts"
bash build.sh

echo "== TD-04 readiness: voucher ABI consistency (source-only) =="
python3 "$ROOT/scripts/check_voucher_abi_consistency.py" \
  --source "$ROOT/audit/spec/an-contracts/exchange" \
  --source-only

echo "== TD-04 readiness: td_04_an_patch_gate tests =="
cd "$ROOT/crates/deposit-relayer-daemon"
cargo test td_04 -- --nocapture

echo "OK — TD-04 repo deploy readiness (owner handoff: docs/operations/td-04-an-deploy-owner-handoff.md)"
echo "NOTE: live AN deploy + owner seed remain ops actions outside this script."
