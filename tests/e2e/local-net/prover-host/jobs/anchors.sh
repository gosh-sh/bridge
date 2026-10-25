#!/usr/bin/env bash
# deposit-relayer arbitrum-anchors: gets the run's Arbitrum deposit blocks accepted through
# ArbitrumBlockVerifier.proveBlock, once the light client has the assertion's Sepolia block.
# Its state is /out/anchors-state.json.
# Input: a tvm keys JSON that signs the proveBlock messages.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load evm.env
cp /run/input "$WORK/an.keys.json"
exec deposit-relayer arbitrum-anchors \
  --parent-rpc-url "$SEPOLIA_RPC" --arbitrum-rpc-url "$ARB_RPC" \
  --bridge-address "$BRIDGE_ADDRESS" --from-block "$BRIDGE_DEPLOY_BLOCK" \
  --an-graphql-url "$GQL" --an-keys-path "$WORK/an.keys.json" --an-bridge-abi-path "$AN_BRIDGE_ABI" \
  --an-verifier-abi-path /src/contracts/an/0.81.0_compiled/exchange/ArbitrumBlockVerifier.abi.json \
  --an-token-bridge "$AN_BRIDGE" --anchor-state "$OUT/anchors-state.json" --allow-insecure-graphql
