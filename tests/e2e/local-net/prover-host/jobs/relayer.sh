#!/usr/bin/env bash
# relayer daemon-live: proves the local network's bundles from the genesis seed (L2
# anchoring, one bundle per 16384 seq_nos, as the live relayer runs) and submits verifyBlock
# to the run's bridge. Beside it, as a process of its own with its own key, relayer
# daemon-withdraw-live finds every burn on the zerostate bridge that names the run's bridge,
# proves it (Circuit 4) and sends withdrawByProof. In compose they are two containers; here
# they share this job's container, which the prover host gives the prover memory limit.
# Input: the deployer's private key; each daemon signs with a key derived from it, relayer
# and withdraw (fund.sh gives both gas). Runs until the runner stops the run.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load genesis.env; load evm.env
deployer=$(tr -d '[:space:]' </run/input)
export RELAYER_PRIVATE_KEY; RELAYER_PRIVATE_KEY=$(derive_key "$deployer" relayer)
export WITHDRAW_PRIVATE_KEY; WITHDRAW_PRIVATE_KEY=$(derive_key "$deployer" withdraw)
export RPC_URL=$ARB_RPC RPC_FAILOVER_URLS= BRIDGE_GQL_ENDPOINT=$GQL
export BRIDGE_BOOTSTRAP_SEQNO=$GENESIS_SEED_SEQNO BRIDGE_ANCHOR_LEVEL=2
export BRIDGE_PARAMS_DIR=/params BRIDGE_PK_CACHE_DIR=/params/pk_cache
# The three directories (bridge-audit#132): relayer-state.json sits in the state directory
# with the prover state; the work directory holds the proof in flight; the log directory
# the daemon's own log beside this one.
# The state directory is /out itself for both daemons, so that the runner reads the
# withdrawal payer's record (withdraw-claims.json) with `ci-entry get`; its file names differ
# from daemon-live's.
export BRIDGE_STATE_DIR=$OUT BRIDGE_WORK_DIR=$WORK/relayer-work BRIDGE_LOG_DIR=/logs/relayer
# The image's aggregate-proof when running from it, /bin-ci's otherwise.
export BRIDGE_AGGREGATE_PROOF_BIN; BRIDGE_AGGREGATE_PROOF_BIN=$(command -v aggregate-proof)
export BRIDGE_VERIFIERS_DIR=/src/contracts/ethereum/verifiers
mkdir -p "$BRIDGE_STATE_DIR" "$BRIDGE_WORK_DIR" "$BRIDGE_LOG_DIR"
BRIDGE_WORK_DIR=$WORK/withdraw-work BRIDGE_LOG_DIR=/logs/withdraw-live \
  relayer daemon-withdraw-live &
relayer daemon-live &
# The job ends, with that daemon's status, as soon as either daemon stops: a payer that died
# shows at once instead of as a withdrawal never paid.
trap 'kill -TERM $(jobs -p) 2>/dev/null' TERM INT
status=0
wait -n || status=$?
kill -TERM $(jobs -p) 2>/dev/null || true
exit "$status"
