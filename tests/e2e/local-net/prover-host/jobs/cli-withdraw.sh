#!/usr/bin/env bash
# ackinacki-bridge withdraw proof, at once for every burn in BURNS ("<token>:<AN tx>+..."): each
# captures the burn, waits for the bundle that covers it, builds its Circuit 4 proof and sends
# withdrawByProof. The runner starts this job once daemon-withdraw-live has paid every one of them out,
# so each send finds its burn paid and the run ends in success, submit.status already_paid.
# Each run logs to /logs/cli-withdraw-<token>.log. The name is the one the host's ci-entry
# gives a prover's limits.
# Input: the deployer's private key. The CLI signs with a key derived from it, which holds no
# gas: a burn paid out is never sent again.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load evm.env
export BURNER_PRIVATE_KEY; BURNER_PRIVATE_KEY=$(derive_key "$(tr -d '[:space:]' </run/input)" withdraw-burner)
mkdir -p "$WORK/aggregator/target/release" && ln -sf /bin-ci/aggregate-proof "$WORK/aggregator/target/release/aggregate-proof"
export RPC_URL=$ARB_RPC BRIDGE_GQL_ENDPOINT=$GQL BRIDGE_ANCHOR_LAYER=2 BRIDGE_I_KNOW_THE_WAIT=true
export USDC_BRIDGE_ACCOUNT_ID=1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a
export BRIDGE_PARAMS_DIR=/params BRIDGE_PK_CACHE_DIR=/params/pk_cache BRIDGE_VERIFIERS_DIR=/src/contracts/ethereum/verifiers
export BRIDGE_AGGREGATOR_DIR=$WORK/aggregator
pids=()
IFS=+ read -r -a burns <<<"$BURNS"
for burn in "${burns[@]}"; do
  token=${burn%%:*} tx=${burn#*:}
  # One proof per snark directory: each burn has its own.
  mkdir -p "$WORK/withdraw-proof-$token/snark"
  log "withdraw proof --burn-tx $tx --token $token"
  BRIDGE_WORK_DIR=$WORK/withdraw-proof-$token BRIDGE_SNARK_DIR=$WORK/withdraw-proof-$token/snark \
    ackinacki-bridge --json --yes --non-interactive withdraw proof --burn-tx "$tx" --token "$token" \
    >>"/logs/cli-withdraw-$token.log" 2>&1 &
  pids+=("$!")
done
status=0
for pid in "${pids[@]}"; do
  wait "$pid" || { log "a withdraw proof exited $?"; status=1; }
done
exit "$status"
