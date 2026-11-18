#!/usr/bin/env bash
# ackinacki-bridge withdraw: burns AMOUNT USDC from the multisig FROM on Acki Nacki and has the
# run's bridge pay it to TO on Arbitrum Sepolia. Waits for daemon-live to verify the burn's
# bundle, proves Circuit 4 and submits withdrawByProof from the burner key (derived from the
# deployer's; fund.sh gives it gas). Stage lines go to this job's log on stderr.
# Input: two lines, the deployer's private key and the multisig's tvm keys JSON on one line.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load evm.env
export BURNER_PRIVATE_KEY; BURNER_PRIVATE_KEY=$(derive_key "$(sed -n 1p /run/input | tr -d '[:space:]')" withdraw-burner)
sed -n 2p /run/input >"$WORK/msig.keys.json" && chmod 0400 "$WORK/msig.keys.json"
mkdir -p "$WORK/aggregator/target/release" && ln -sf /bin-ci/aggregate-proof "$WORK/aggregator/target/release/aggregate-proof"
export RPC_URL=$ARB_RPC BRIDGE_GQL_ENDPOINT=$GQL BRIDGE_ANCHOR_LAYER=2 BRIDGE_I_KNOW_THE_WAIT=true
export USDC_BRIDGE_ACCOUNT_ID=1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a
export BRIDGE_PARAMS_DIR=/params BRIDGE_PK_CACHE_DIR=/params/pk_cache BRIDGE_VERIFIERS_DIR=/src/contracts/ethereum/verifiers
export BRIDGE_AGGREGATOR_DIR=$WORK/aggregator BRIDGE_SNARK_DIR=$WORK/cli/shplonk-snark
mkdir -p "$BRIDGE_SNARK_DIR"
nackl_pid=
if [ -n "${NACKL_FROM:-}" ]; then
  sed -n 3p /run/input >"$WORK/msig-nackl.keys.json" && chmod 0400 "$WORK/msig-nackl.keys.json"
  mkdir -p "$WORK/cli-nackl" "$WORK/cli-state-nackl"
  log "withdraw $NACKL_AMOUNT NACKL from $NACKL_FROM to $TO on 421614"
  BRIDGE_WORK_DIR=$WORK/cli-nackl BRIDGE_WITHDRAW_STATE_DIR=$WORK/cli-state-nackl \
    BURNER_PRIVATE_KEY=$(derive_key "$(sed -n 1p /run/input | tr -d '[:space:]')" withdraw-nackl-burner) \
    ackinacki-bridge --json --yes --non-interactive withdraw --token-id 1 --from "$NACKL_FROM" \
    --from-keys "$WORK/msig-nackl.keys.json" --to "$TO" --to-chain 421614 --amount "$NACKL_AMOUNT" \
    >>/logs/cli-withdraw-nackl.log 2>&1 &
  nackl_pid=$!
fi
export BRIDGE_WORK_DIR=$WORK/cli BRIDGE_WITHDRAW_STATE_DIR=$WORK/cli-state
mkdir -p "$BRIDGE_WORK_DIR" "$BRIDGE_WITHDRAW_STATE_DIR"
log "withdraw $AMOUNT USDC from $FROM to $TO on 421614"
status=0
ackinacki-bridge --json --yes --non-interactive withdraw --from "$FROM" --from-keys "$WORK/msig.keys.json" \
  --to "$TO" --to-chain 421614 --amount "$AMOUNT" || status=$?
if [ -n "$nackl_pid" ]; then
  wait "$nackl_pid" || { log "the NACKL withdrawal exited $?"; [ "$status" -ne 0 ] || status=1; }
fi
exit "$status"
