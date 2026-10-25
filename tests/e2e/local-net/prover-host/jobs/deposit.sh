#!/usr/bin/env bash
# Deposits AMOUNT USDC base units to AN account AN_ACCOUNT on the run's bridge, and funds
# the withdrawal's burner (a key derived from the deployer's) with gas.
# Input: the deployer's private key. Writes deposit.env.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load evm.env
pk=$(tr -d '[:space:]' </run/input)
from=$(cast wallet address --private-key "$pk")
send() { cast send --rpc-url "$ARB_RPC" --private-key "$pk" --json "$@"; }
log "approve $AMOUNT and deposit to $AN_ACCOUNT"
send "$ARB_USDC" "approve(address,uint256)" "$BRIDGE_ADDRESS" "$AMOUNT" >/dev/null
send "$BRIDGE_ADDRESS" "deposit(uint256,bytes32)" "$AMOUNT" "0x$AN_ACCOUNT" >"$WORK/deposit.json"
python3 - "$WORK/deposit.json" <<'PY' >>"$OUT/deposit.env"
import json, sys
r = json.load(open(sys.argv[1]))
assert r["status"] in ("0x1", 1), r
print(f"DEPOSIT_TX={r['transactionHash']}\nDEPOSIT_BLOCK={int(r['blockNumber'], 16)}\nDEPOSIT_BLOCK_HASH={r['blockHash']}")
PY
burner_pk=$(cast keccak "$(cast concat-hex "$pk" "$(cast from-utf8 withdraw-burner)")")
burner=$(cast wallet address --private-key "$burner_pk")
send "$burner" --value "${BURNER_GAS:-0.001ether}" >/dev/null
emit deposit.env "DEPOSITOR=$from" "BURNER=$burner"
log "treasury $(cast call "$BRIDGE_ADDRESS" "treasuryBalance()(uint256)" --rpc-url "$ARB_RPC")"
cat "$OUT/deposit.env"
