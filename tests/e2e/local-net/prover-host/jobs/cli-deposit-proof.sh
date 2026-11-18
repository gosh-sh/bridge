#!/usr/bin/env bash
# ackinacki-bridge deposit proof, one after another for every deposit in DEPOSITS
# ("<token>:<Arbitrum Sepolia tx>+..."): each reads the deposit from its receipt, waits for its
# block's anchor, builds the receipt proof and, before sending finalizeDeposit, finds the deposit
# finalized. The runner starts this job once the deposit relayer has finalized them, so each run
# sends nothing and ends in success, finalize: already_finalized. Each run logs to
# /logs/cli-deposit-proof-<token>.log.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load evm.env
export RPC_URL=$ARB_DEPOSIT_RPC BRIDGE_ADDRESS BRIDGE_GQL_ENDPOINT=$GQL
export USDC_BRIDGE_ACCOUNT_ID=1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a
export BRIDGE_DEPOSIT_CONFIRMATIONS=${CONFIRMATIONS:-1}
status=0
IFS=+ read -r -a deposits <<<"$DEPOSITS"
for deposit in "${deposits[@]}"; do
  token=${deposit%%:*} tx=${deposit#*:}
  export BRIDGE_DEPOSIT_STATE_DIR=$WORK/deposit-proof-$token-state BRIDGE_WORK_DIR=$WORK/deposit-proof-$token-work
  mkdir -p "$BRIDGE_DEPOSIT_STATE_DIR" "$BRIDGE_WORK_DIR"
  log "deposit proof --tx-hash $tx --token $token"
  ackinacki-bridge --json --yes --non-interactive deposit proof --network arbitrum-sepolia \
    --tx-hash "$tx" --token "$token" --anchor-timeout-s "${ANCHOR_TIMEOUT_S:-900}" \
    >>"/logs/cli-deposit-proof-$token.log" 2>&1 || { log "the $token deposit proof exited $?"; status=1; }
done
exit "$status"
