#!/usr/bin/env bash
# deposit-relayer daemon: proves and finalizes the run's deposits beside the CLI, which does
# the same for the one it makes; whichever finalizes second is a no-op (the voucher is
# deployed once). Runs from the deposit-relayer image, so its binary and prover tools are the
# image's. Input: a tvm keys JSON that signs finalizeDeposit (the bridge owner's, the sender
# being the bridge account, as on the stand). Runs until the runner stops the run.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load evm.env
cp /run/input "$WORK/deposit-relayer.keys.json"
# The prover directory the tools run in: configs/circuit_params.json beside a data/ with the
# deposit SRS (Hermez k=18, cut from the k=21 file the way the release does it, cached
# across runs). Assembled here: the image's own data/ points at a params mount this job
# does not have.
p=$WORK/deposit-relayer-prover
mkdir -p "$p/configs" "$p/data" /cache/deposit-srs
cp "${DEPOSIT_PROVER_DIR:-/bin-ci}/configs/circuit_params.json" "$p/configs/" 2>/dev/null ||
  cp /bin-ci/circuit_params.json "$p/configs/"
[ -s /cache/deposit-srs/kzg_params_18.srs ] ||
  downsize_srs --input /params/kzg_bn254_21.srs --output /cache/deposit-srs/kzg_params_18.srs --k 18
ln -sf /cache/deposit-srs/kzg_params_18.srs "$p/data/kzg_params_18.srs"
mkdir -p "$OUT/deposit-relayer-state" "$WORK/deposit-relayer-work" /logs/deposit-relayer
log "deposit-relayer daemon on bridge $BRIDGE_ADDRESS from block $BRIDGE_DEPLOY_BLOCK"
exec deposit-relayer daemon --rpc-url "$ARB_DEPOSIT_RPC" --expect-chain-id 421614 \
  --bridge-address "$BRIDGE_ADDRESS" --from-block "$BRIDGE_DEPLOY_BLOCK" --confirmations "${CONFIRMATIONS:-1}" \
  --state-dir "$OUT/deposit-relayer-state" --work-dir "$WORK/deposit-relayer-work" --log-dir /logs/deposit-relayer \
  --deposit-prover-dir "$p" \
  --fetch-deposit-data-bin "${DEPOSIT_FETCH_DEPOSIT_DATA_BIN:-/bin-ci/fetch_deposit_data}" \
  --export-vk-blob-bin "${DEPOSIT_EXPORT_VK_BLOB_BIN:-/bin-ci/export_vk_blob}" \
  --export-blake2b-proof-bin "${DEPOSIT_EXPORT_BLAKE2B_PROOF_BIN:-/bin-ci/export_blake2b_proof}" \
  --an-graphql-url "$GQL" --an-keys-path "$WORK/deposit-relayer.keys.json" \
  --an-token-bridge "$AN_BRIDGE" --an-sender "$AN_BRIDGE" --allow-insecure-graphql
