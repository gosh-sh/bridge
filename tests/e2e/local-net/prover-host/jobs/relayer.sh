#!/usr/bin/env bash
# relayer daemon-live: proves the local network's bundles from the genesis seed (L1
# anchoring, one bundle per 1024 seq_nos) and submits verifyBlock to the run's bridge.
# Input: the relayer's private key. Runs until the runner stops the run.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load genesis.env; load evm.env
export RELAYER_PRIVATE_KEY; RELAYER_PRIVATE_KEY=$(tr -d '[:space:]' </run/input)
export RPC_URL=$ARB_RPC RPC_FAILOVER_URLS= BRIDGE_GQL_ENDPOINT=$GQL BRIDGE_BK_SET_CONFIG=$OUT/bk_set.json
export BRIDGE_BOOTSTRAP_SEQNO=$GENESIS_SEED_SEQNO BRIDGE_ANCHOR_LEVEL=1
export BRIDGE_PARAMS_DIR=/params BRIDGE_PK_CACHE_DIR=/params/pk_cache BRIDGE_STATE_DIR=$WORK/relayer-state
export BRIDGE_AGGREGATE_PROOF_BIN=/bin-ci/aggregate-proof BRIDGE_VERIFIERS_DIR=/src/contracts/ethereum/verifiers
mkdir -p "$BRIDGE_STATE_DIR"
exec relayer --state "$WORK/relayer-state.json" daemon-live
