#!/usr/bin/env bash
# deposit-relayer daemon: proves the run's Arbitrum deposits and finalizes them on the local
# network once their blocks are accepted. Input: a tvm keys JSON that signs finalizeDeposit.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load evm.env
cp /run/input "$WORK/an.keys.json"
p=$WORK/deposit-prover
mkdir -p "$p/configs" "$p/data" /cache/deposit-srs
for t in fetch_deposit_data export_vk_blob export_blake2b_proof; do ln -sf "/bin-ci/$t" "$p/$t"; done
cp /bin-ci/circuit_params.json "$p/configs/"
# The deposit SRS: Hermez k=18, cut from the k=21 file the way the release does it.
[ -s /cache/deposit-srs/kzg_params_18.srs ] ||
  downsize_srs --input /params/kzg_bn254_21.srs --output /cache/deposit-srs/kzg_params_18.srs --k 18
ln -sf /cache/deposit-srs/kzg_params_18.srs "$p/data/kzg_params_18.srs"
exec deposit-relayer --state "$OUT/deposit-relayer-state.json" daemon \
  --rpc-url "$ARB_RPC" --bridge-address "$BRIDGE_ADDRESS" --from-block "$BRIDGE_DEPLOY_BLOCK" \
  --confirmations 1 --start-deposit-id 0 --expect-chain-id 421614 \
  --deposit-prover-dir "$p" --fetch-deposit-data-bin "$p/fetch_deposit_data" \
  --export-vk-blob-bin "$p/export_vk_blob" --export-blake2b-proof-bin "$p/export_blake2b_proof" \
  --an-graphql-url "$GQL" --an-keys-path "$WORK/an.keys.json" --an-bridge-abi-path "$AN_BRIDGE_ABI" \
  --an-token-bridge "$AN_BRIDGE" --an-sender "$AN_BRIDGE" --allow-insecure-graphql
