#!/usr/bin/env bash
# ackinacki-bridge withdraw send, one after another for every burn in SENDS
# ("<token>,<multisig dapp::account>,<amount> ..."): the multisig burns the amount of the token
# on Acki Nacki to TO on Arbitrum Sepolia, and the run stops once the bridge's
# WithdrawalInitiated is captured; nothing is proven or sent to Arbitrum Sepolia. daemon-live
# pays each burn out; cli-withdraw proves it again afterwards with the burn.an_tx of the --json
# summary. Each run logs to /logs/cli-withdraw-send-<token>.log.
# Input: one line per burn, in the order of SENDS: the multisig's tvm keys JSON.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load evm.env
export RPC_URL=$ARB_RPC BRIDGE_GQL_ENDPOINT=$GQL BRIDGE_ANCHOR_LAYER=2 BRIDGE_I_KNOW_THE_WAIT=true
export USDC_BRIDGE_ACCOUNT_ID=1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a
line=0
for send in $SENDS; do
  line=$((line + 1))
  IFS=, read -r token from amount <<<"$send"
  keys=$WORK/withdraw-send-$token.keys.json
  sed -n "${line}p" /run/input | tr -d '\n' >"$keys" && chmod 0400 "$keys"
  export BRIDGE_WITHDRAW_STATE_DIR=$WORK/withdraw-send-$token-state
  mkdir -p "$BRIDGE_WITHDRAW_STATE_DIR"
  log "withdraw send $amount $token from $from to $TO on 421614"
  ackinacki-bridge --json --yes --non-interactive withdraw send --token "$token" \
    --from "$from" --from-keys "$keys" --to "$TO" --to-chain 421614 --amount "$amount" \
    >>"/logs/cli-withdraw-send-$token.log" 2>&1
done
