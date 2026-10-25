#!/usr/bin/env bash
# ackinacki-bridge withdraw: burns AMOUNT from the multisig FROM on the local network and has
# the run's bridge on Arbitrum Sepolia pay it to TO. Waits for daemon-live to verify the
# burn's bundle, proves Circuit 4 and submits withdrawByProof with the burner key.
# Input: two lines, the deployer's private key (the burner key is derived from it) and the
# multisig's tvm keys JSON on one line.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load evm.env
pk=$(sed -n 1p /run/input | tr -d '[:space:]')
export BURNER_PRIVATE_KEY; BURNER_PRIVATE_KEY=$(cast keccak "$(cast concat-hex "$pk" "$(cast from-utf8 withdraw-burner)")")
sed -n 2p /run/input >"$WORK/msig.keys.json" && chmod 0400 "$WORK/msig.keys.json"
mkdir -p "$WORK/aggregator/target/release" && ln -sf /bin-ci/aggregate-proof "$WORK/aggregator/target/release/aggregate-proof"
export RPC_URL=$ARB_RPC BRIDGE_GQL_ENDPOINT=$GQL BRIDGE_ANCHOR_LAYER=1 BRIDGE_I_KNOW_THE_WAIT=true
export USDC_BRIDGE_ACCOUNT_ID=1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a
export BRIDGE_PARAMS_DIR=/params BRIDGE_PK_CACHE_DIR=/params/pk_cache BRIDGE_VERIFIERS_DIR=/src/contracts/ethereum/verifiers
export BRIDGE_AGGREGATOR_DIR=$WORK/aggregator BRIDGE_WORK_DIR=$WORK/cli BRIDGE_SNARK_DIR=$WORK/cli/shplonk-snark
export BRIDGE_WITHDRAW_STATE_DIR=$WORK/cli-state
mkdir -p "$BRIDGE_WORK_DIR" "$BRIDGE_WITHDRAW_STATE_DIR"
ackinacki-bridge withdraw --from "$FROM" --from-keys "$WORK/msig.keys.json" --to "$TO" --to-chain 421614 \
  --amount "$AMOUNT" --yes --non-interactive --json | tee "$OUT/withdraw.json"
