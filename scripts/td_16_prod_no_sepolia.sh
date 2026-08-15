#!/usr/bin/env bash
# TD-16 — production deposit deployments must not target Sepolia (testnet-only).
#
# Usage:
#   ./scripts/td_16_prod_no_sepolia.sh
#   CHAIN_ID=11155111 PROFILE=prod ./scripts/td_16_prod_no_sepolia.sh   # expect fail
#   CHAIN_ID=11155111 PROFILE=shellnet ./scripts/td_16_prod_no_sepolia.sh  # expect pass
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "${ROOT}"

PROFILE="${PROFILE:-}"
CHAIN_ID="${CHAIN_ID:-}"

usage() {
  cat <<'EOF'
TD-16 — prod deposit chain policy gate.

Environment:
  PROFILE   prod|production → Sepolia (11155111) forbidden
            shellnet|dev|testnet|audit → Sepolia allowed
  CHAIN_ID  optional explicit check (e.g. 11155111)

Runs deposit-chain-ids / relayer td_16 unit tests when env check passes.
EOF
}

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  usage
  exit 0
fi

if [[ "${PROFILE}" == "prod" || "${PROFILE}" == "production" ]]; then
  if [[ -n "${CHAIN_ID}" && "${CHAIN_ID}" == "11155111" ]]; then
    echo "FAIL: TD-16 Sepolia (chainId=11155111) is forbidden for PROFILE=${PROFILE}" >&2
    echo "      Production mint must not use testnet USDC (ops policy DEP-T16)." >&2
    exit 1
  fi
  echo "OK: PROFILE=${PROFILE} — Sepolia not selected via CHAIN_ID"
fi

if [[ "${PROFILE}" == "shellnet" && "${CHAIN_ID}" == "11155111" ]]; then
  echo "OK: PROFILE=shellnet with Sepolia chainId (dev/shellnet path)"
fi

(
  cd crates/deposit-chain-ids
  cargo test td_16 --quiet
)
(
  cd crates/deposit-relayer-daemon
  cargo test td_16 --quiet
)

echo "OK: TD-16 prod ∩ testnet-only = ∅ (crate tests green)"
