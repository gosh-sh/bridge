#!/usr/bin/env bash
# ackinacki-bridge withdraw, twice at once: burns AMOUNT USDC from the multisig FROM and AMOUNT of
# the ECC currency TOKEN_ID from the multisig TOKEN_FROM on Acki Nacki, and has the run's bridge
# pay both to TO on Arbitrum Sepolia. Each waits for daemon-live to verify its burn's bundle,
# proves Circuit 4 and submits withdrawByProof from its own burner key (derived from the deployer's;
# fund.sh gives it gas). Stage lines go to this job's log on stderr, the second token's to
# /logs/cli-withdraw-token.log.
# ci-entry caps a job's memory by its name and only this name gets a prover's, so both run in this
# job, burning for the same bundle. They share the snark directory, whose lock proves one at a time.
# Input: three lines, the deployer's private key and each multisig's tvm keys JSON on one line.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load evm.env
deployer=$(sed -n 1p /run/input | tr -d '[:space:]')
sed -n 2p /run/input >"$WORK/msig.keys.json" && chmod 0400 "$WORK/msig.keys.json"
sed -n 3p /run/input >"$WORK/msig-token.keys.json" && chmod 0400 "$WORK/msig-token.keys.json"
mkdir -p "$WORK/aggregator/target/release" && ln -sf /bin-ci/aggregate-proof "$WORK/aggregator/target/release/aggregate-proof"
export RPC_URL=$ARB_RPC BRIDGE_GQL_ENDPOINT=$GQL BRIDGE_ANCHOR_LAYER=2 BRIDGE_I_KNOW_THE_WAIT=true
export USDC_BRIDGE_ACCOUNT_ID=1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a
export BRIDGE_PARAMS_DIR=/params BRIDGE_PK_CACHE_DIR=/params/pk_cache BRIDGE_VERIFIERS_DIR=/src/contracts/ethereum/verifiers
export BRIDGE_AGGREGATOR_DIR=$WORK/aggregator BRIDGE_SNARK_DIR=$WORK/shplonk-snark
mkdir -p "$BRIDGE_SNARK_DIR"

# <burner role> <work directory name> <multisig> <its keys> [CLI flags]
withdraw() {
  export BURNER_PRIVATE_KEY; BURNER_PRIVATE_KEY=$(derive_key "$deployer" "$1")
  export BRIDGE_WORK_DIR=$WORK/$2 BRIDGE_WITHDRAW_STATE_DIR=$WORK/$2-state
  mkdir -p "$BRIDGE_WORK_DIR" "$BRIDGE_WITHDRAW_STATE_DIR"
  log "withdraw $AMOUNT from $3 to $TO on 421614 ${*:5}"
  exec ackinacki-bridge --json --yes --non-interactive withdraw --from "$3" --from-keys "$4" \
    --to "$TO" --to-chain 421614 --amount "$AMOUNT" "${@:5}"
}
withdraw withdraw-burner cli "$FROM" "$WORK/msig.keys.json" &
withdraw withdraw-burner-token cli-token "$TOKEN_FROM" "$WORK/msig-token.keys.json" --token-id "$TOKEN_ID" \
  >/logs/cli-withdraw-token.log 2>&1 &
for _ in 1 2; do
  wait -n || { code=$?; log "a withdrawal exited $code; stopping the other"; kill $(jobs -p) 2>/dev/null || true; exit "$code"; }
done
