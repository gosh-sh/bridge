#!/usr/bin/env bash
# ackinacki-bridge withdraw send: the multisig FROM burns AMOUNT of TOKEN (usdc or nackl) on Acki
# Nacki to TO on Arbitrum Sepolia, and the run stops once the bridge's WithdrawalInitiated is
# captured: nothing is proven or sent to Arbitrum Sepolia. daemon-live pays the burn out;
# cli-withdraw proves it again afterwards with the burn.an_tx of the --json summary.
# Input: the multisig's tvm keys JSON on one line.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load evm.env
keys=$WORK/withdraw-send-$TOKEN.keys.json
tr -d '\n' </run/input >"$keys" && chmod 0400 "$keys"
export RPC_URL=$ARB_RPC BRIDGE_GQL_ENDPOINT=$GQL BRIDGE_ANCHOR_LAYER=2 BRIDGE_I_KNOW_THE_WAIT=true
export USDC_BRIDGE_ACCOUNT_ID=1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a
export BRIDGE_WITHDRAW_STATE_DIR=$WORK/withdraw-send-$TOKEN-state
mkdir -p "$BRIDGE_WITHDRAW_STATE_DIR"
log "withdraw send $AMOUNT $TOKEN from $FROM to $TO on 421614"
exec ackinacki-bridge --json --yes --non-interactive withdraw send --token "$TOKEN" --from "$FROM" \
  --from-keys "$keys" --to "$TO" --to-chain 421614 --amount "$AMOUNT"
