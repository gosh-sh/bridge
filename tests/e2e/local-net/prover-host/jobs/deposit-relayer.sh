#!/usr/bin/env bash
# deposit-relayer daemon: finalizes the run's deposits beside the CLI, which does the same
# for the one it makes; whichever finalizes second is a no-op (the voucher is deployed once).
# Runs from the deposit-relayer image, so its binary is the image's. Input: a tvm keys JSON that signs finalizeDeposit (the bridge owner's, the sender
# being the bridge account, as on the stand). Runs until the runner stops the run.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load evm.env
cp /run/input "$WORK/deposit-relayer.keys.json"
mkdir -p "$OUT/deposit-relayer-state" /logs/deposit-relayer
log "deposit-relayer daemon on bridge $BRIDGE_ADDRESS from block $BRIDGE_DEPLOY_BLOCK"
exec deposit-relayer daemon --rpc-url "$ARB_DEPOSIT_RPC" --expect-chain-id 421614 \
  --bridge-address "$BRIDGE_ADDRESS" --from-block "$BRIDGE_DEPLOY_BLOCK" --confirmations "${CONFIRMATIONS:-1}" \
  --state-dir "$OUT/deposit-relayer-state" --log-dir /logs/deposit-relayer \
  --an-graphql-url "$GQL" --an-keys-path "$WORK/deposit-relayer.keys.json" \
  --an-token-bridge "$AN_BRIDGE" --an-sender "$AN_BRIDGE" --allow-insecure-graphql
