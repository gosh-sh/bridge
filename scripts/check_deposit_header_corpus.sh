#!/usr/bin/env bash
# TD-64 — deposit header corpus inventory (7 supported chains).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HDR="$ROOT/deposit-prover/fixtures/headers"

EXPECTED=(
  op_mainnet.json
  world_chain_mainnet.json
  mantle_mainnet.json
  base_mainnet.json
  arbitrum_one.json
  blast_mainnet.json
  sepolia_prague.json
)

missing=0
for f in "${EXPECTED[@]}"; do
  if [[ ! -f "$HDR/$f" ]]; then
    echo "TD-64 check: missing fixture $f"
    missing=$((missing + 1))
    continue
  fi
  hash=$(jq -r '.hash // empty' "$HDR/$f")
  number=$(jq -r '.number // empty' "$HDR/$f")
  if [[ -z "$hash" || -z "$number" ]]; then
    echo "TD-64 check: $f missing hash or number"
    missing=$((missing + 1))
  fi
done

if [[ "$missing" -ne 0 ]]; then
  echo "TD-64 check: FAIL ($missing fixture problems)"
  exit 1
fi

count="${#EXPECTED[@]}"
echo "TD-64 check: OK — $count deposit-chain header fixtures present with hash+number"
