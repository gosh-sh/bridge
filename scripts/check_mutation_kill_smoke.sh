#!/usr/bin/env bash
# TD-49 / DEP-MUTATION-TESTING — CI smoke: pinned mutation kill score 13/13.
#
# Runs relayer + L1 overlay + circuit mutation kill tests.
# Wire after TD-43 in check_deposit_audit_gates.sh (META gates block).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  cat <<'EOF'
TD-49 — mutation kill score smoke (pinned KILLED 13/13).

  ./scripts/check_mutation_kill_smoke.sh

Runs:
  crates/deposit-relayer-daemon — td_49_mutation_kill_matrix
  audit/spec/ethereum — DepositMutationKill.t.sol (FOUNDRY_PROFILE=audit)
  deposit-prover — td_49_mutation_kill

Score: 13 kill mutants (7 relayer bound + 4 L1 + 2 circuit); 3 QC survivors documented.
See: audit/reports/td-49-mutation-score-notes.md
EOF
  exit 0
fi

echo "TD-49 smoke: relayer check_binds_to kill matrix"
cd "$ROOT/crates/deposit-relayer-daemon"
cargo test --test td_49_mutation_kill_matrix -- --nocapture

echo "TD-49 smoke: L1 DepositMutationKill overlay"
cd "$ROOT/audit/spec/ethereum"
FOUNDRY_PROFILE=audit forge test --match-path '*MutationKill*' -q

echo "TD-49 smoke: deposit-prover PI slot kills"
cd "$ROOT/deposit-prover"
cargo test --test td_49_mutation_kill -- --nocapture

echo "OK — TD-49 mutation kill smoke (KILLED 13/13 on kill set; 3 QC survivors documented)"
