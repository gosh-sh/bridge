#!/usr/bin/env bash
# Gas for the two senders derived from the deployer's key: the relayer (verifyBlock) and the
# withdrawal's burner (withdrawByProof). The keys are the same every run, so each is topped up to
# GAS_PER_SENDER (default 0.002 ether), not sent that much again. Input: the deployer's private
# key. Writes fund.env.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
pk=$(tr -d '[:space:]' </run/input)
want=$(cast to-wei "${GAS_PER_SENDER:-0.002ether}")
for role in relayer withdraw-burner; do
  addr=$(cast wallet address --private-key "$(derive_key "$pk" "$role")")
  have=$(cast balance "$addr" --rpc-url "$ARB_RPC")
  if python3 -c 'import sys; sys.exit(int(sys.argv[1]) >= int(sys.argv[2]))' "$have" "$want"; then
    top=$(python3 -c 'import sys; print(int(sys.argv[2]) - int(sys.argv[1]))' "$have" "$want")
    log "topping $role $addr up by $top wei to $want"
    cast send --rpc-url "$ARB_RPC" --private-key "$pk" --json "$addr" --value "$top" >/dev/null
  else
    log "$role $addr has $have wei, at least $want"
  fi
  emit fund.env "$(tr 'a-z-' 'A-Z_' <<<"$role")_ADDRESS=$addr" \
    "$(tr 'a-z-' 'A-Z_' <<<"$role")_BALANCE_WEI=$(cast balance "$addr" --rpc-url "$ARB_RPC")"
done
cat "$OUT/fund.env"
