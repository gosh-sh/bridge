#!/usr/bin/env bash
# Gas for the two senders derived from the deployer's key: the relayer (verifyBlock) and the
# withdrawal's burner (withdrawByProof). Input: the deployer's private key. Writes fund.env.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
pk=$(tr -d '[:space:]' </run/input)
for role in relayer withdraw-burner; do
  addr=$(cast wallet address --private-key "$(derive_key "$pk" "$role")")
  log "funding $role $addr with ${GAS_PER_SENDER:-0.002ether}"
  cast send --rpc-url "$ARB_RPC" --private-key "$pk" --json "$addr" --value "${GAS_PER_SENDER:-0.002ether}" >/dev/null
  emit fund.env "$(tr 'a-z-' 'A-Z_' <<<"$role")_ADDRESS=$addr" \
    "$(tr 'a-z-' 'A-Z_' <<<"$role")_BALANCE_WEI=$(cast balance "$addr" --rpc-url "$ARB_RPC")"
done
cat "$OUT/fund.env"
