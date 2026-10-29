#!/usr/bin/env bash
# eth-lc-relayer daemon: rolls the Sepolia light client forward from the committee lc-bootstrap
# set, one finalized checkpoint per epoch, and with --submit-ancestry the blocks between two
# consecutive checkpoints, which is where an Arbitrum assertion's Sepolia block gets accepted.
# No rotate and no owner hop: the run must not cross a sync-committee period
# (test_02 checks it). Input: any tvm keys JSON; submitUpdate and submitAncestry are
# permissionless. Args: LC, the light client's address. Runs until the runner stops the run.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
cp /run/input "$WORK/lc-sender.keys.json"
mkdir -p "$WORK/lc-prover"
export STEP_KEY_DIR=$LC_STEP_KEY_DIR   # the prover inherits it: proves with lc-keygen's key
exec eth-lc-relayer daemon --beacon-url "$BEACON_URL" --state "$OUT/lc-state.json" \
  --step-prover-bin /bin-ci/export_step_vk_blob --prover-dir "$WORK/lc-prover" \
  --srs-path /params/kzg_bn254_19.srs --no-rotate --no-flip-owner \
  --eth-rpc-url "$SEPOLIA_RPC" --submit-ancestry \
  --an-graphql-url "$GQL" --an-keys-path "$WORK/lc-sender.keys.json" --an-lc-abi-path "$LC_ABI" \
  --an-light-client "$LC" --an-sender "$LC" --allow-insecure-graphql
