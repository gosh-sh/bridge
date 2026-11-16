#!/usr/bin/env bash
# ackinacki-bridge deposit: AMOUNT USDC from FROM on Arbitrum Sepolia to the Acki Nacki
# account TO, signed with the deployer's key (--qr-mode local-key). The CLI waits for the
# block anchor, builds the deposit's receipt proof, finalizes it and waits for the credit. --json prints one
# line per step; this job's log keeps them for the runner's asserts.
# Input: the deployer's private key.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load evm.env
tag=deposit token=() unit=USDC
if [ "${DEPOSIT_TOKEN:-}" = nackl ]; then
  tag=nackl-deposit token=(--token "$NACKL_ADDRESS") unit=NACKL
fi
export BRIDGE_DEPOSIT_PRIVATE_KEY; BRIDGE_DEPOSIT_PRIVATE_KEY=$(tr -d '[:space:]' </run/input)
export BRIDGE_DEPOSIT_FROM=$FROM RPC_URL=$ARB_DEPOSIT_RPC BRIDGE_ADDRESS BRIDGE_GQL_ENDPOINT=$GQL
export USDC_BRIDGE_ACCOUNT_ID=1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a
export BRIDGE_DEPOSIT_CONFIRMATIONS=${CONFIRMATIONS:-1}
export BRIDGE_DEPOSIT_STATE_DIR=$WORK/$tag-state BRIDGE_WORK_DIR=$WORK/$tag-work
mkdir -p "$BRIDGE_DEPOSIT_STATE_DIR" "$BRIDGE_WORK_DIR"
log "deposit $AMOUNT $unit from $FROM to $TO"
exec ackinacki-bridge --json --yes --non-interactive deposit --network arbitrum-sepolia --qr-mode local-key \
  "${token[@]}" --amount "$AMOUNT" --to "$TO" --anchor-timeout-s "${ANCHOR_TIMEOUT_S:-5400}"
