#!/usr/bin/env bash
# TD-41 — Ethereum L1 mainnet (chainId=1) is not a deposit source by design.
#
# Usage:
#   ./scripts/td_41_no_mainnet_chain.sh
#   CHAIN_ID=1 PROFILE=prod ./scripts/td_41_no_mainnet_chain.sh   # expect fail
#   CHAIN_ID=8453 PROFILE=prod ./scripts/td_41_no_mainnet_chain.sh # expect pass
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "${ROOT}"

PROFILE="${PROFILE:-}"
CHAIN_ID="${CHAIN_ID:-}"

usage() {
  cat <<'EOF'
TD-41 — mainnet L1 chainId policy gate.

Environment:
  PROFILE   prod|production → chainId=1 always forbidden (not in allowlist)
            shellnet|dev|testnet|audit → chainId=1 still forbidden (six L2 + Sepolia only)
  CHAIN_ID  optional explicit check (e.g. 1)

Runs deposit-chain-ids / relayer td_41 unit tests when env check passes.
EOF
}

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  usage
  exit 0
fi

if [[ -n "${CHAIN_ID}" && "${CHAIN_ID}" == "1" ]]; then
  echo "FAIL: TD-41 Ethereum L1 mainnet (chainId=1) is not a supported deposit source" >&2
  echo "      Allowed: six L2 mainnets + Sepolia (shellnet only)." >&2
  exit 1
fi

if [[ "${PROFILE}" == "prod" || "${PROFILE}" == "production" ]]; then
  if [[ -n "${CHAIN_ID}" && "${CHAIN_ID}" == "1" ]]; then
    echo "FAIL: TD-41 chainId=1 forbidden for PROFILE=${PROFILE}" >&2
    exit 1
  fi
  echo "OK: PROFILE=${PROFILE} — chainId=1 not selected"
fi

(
  cd crates/deposit-chain-ids
  cargo test td_41 --quiet
)
(
  cd crates/deposit-relayer-daemon
  cargo test td_41 --quiet
)

echo "OK: TD-41 chainId=1 ∉ deposit allowlist (crate tests green)"
