#!/usr/bin/env bash
# eth-lc-relayer arbitrum-anchors: gets the run's Arbitrum deposit blocks accepted through
# ArbitrumBlockVerifier.proveBlock, once the light client has the assertion's Sepolia block.
# Its state is /out/anchors-state/arbitrum-anchors-state.json.
# Input: line 1, a tvm keys JSON that signs the proveBlock messages; line 2, when there, the
# ARB_RPC secret: an Arbitrum RPC that keeps a minute of state. It comes on stdin, never as a
# job argument, because ci-entry logs every command line.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load evm.env
sed -n 1p /run/input >"$WORK/an.keys.json"
rpc=$(sed -n 2p /run/input | tr -d '[:space:]')
[ -z "$rpc" ] || ARB_RPC=$rpc
mkdir -p "$WORK/anchors-work" /logs/anchors
exec eth-lc-relayer arbitrum-anchors \
  --parent-rpc-url "$SEPOLIA_RPC" --arbitrum-rpc-url "$ARB_RPC" --arbitrum-log-span 10 \
  --bridge-address "$BRIDGE_ADDRESS" --from-block "$BRIDGE_DEPLOY_BLOCK" \
  --an-graphql-url "$GQL" --an-keys-path "$WORK/an.keys.json" --an-token-bridge "$AN_BRIDGE" \
  --state-dir "$OUT/anchors-state" --work-dir "$WORK/anchors-work" --log-dir /logs/anchors \
  --allow-insecure-graphql
