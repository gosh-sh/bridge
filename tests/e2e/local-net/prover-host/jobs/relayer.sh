#!/usr/bin/env bash
# relayer daemon-live: proves the local network's bundles from the genesis seed (L2
# anchoring, one bundle per 16384 seq_nos, as the live relayer runs) and submits verifyBlock
# to the run's bridge.
# Input: the deployer's private key; it signs with the relayer key derived from it (fund.sh
# gives that one gas). Runs until the runner stops the run.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load genesis.env; load evm.env
export RELAYER_PRIVATE_KEY; RELAYER_PRIVATE_KEY=$(derive_key "$(tr -d '[:space:]' </run/input)" relayer)
export RPC_URL=$ARB_RPC RPC_FAILOVER_URLS= BRIDGE_GQL_ENDPOINT=$GQL BRIDGE_BK_SET_CONFIG=$OUT/bk_set.json
export BRIDGE_BOOTSTRAP_SEQNO=$GENESIS_SEED_SEQNO BRIDGE_ANCHOR_LEVEL=2
export BRIDGE_PARAMS_DIR=/params BRIDGE_PK_CACHE_DIR=/params/pk_cache BRIDGE_STATE_DIR=$WORK/relayer-state
export BRIDGE_AGGREGATE_PROOF_BIN=/bin-ci/aggregate-proof BRIDGE_VERIFIERS_DIR=/src/contracts/ethereum/verifiers
mkdir -p "$BRIDGE_STATE_DIR"
exec relayer --state "$WORK/relayer-state.json" daemon-live
