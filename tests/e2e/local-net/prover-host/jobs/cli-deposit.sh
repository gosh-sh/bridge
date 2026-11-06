#!/usr/bin/env bash
# ackinacki-bridge deposit, twice: AMOUNT of USDC and AMOUNT of TOKEN from FROM on Arbitrum Sepolia
# to the Acki Nacki account TO, signed with the deployer's key (--qr-mode local-key). The second
# starts once the first's Deposit event is received, so the two never share a nonce. Each CLI waits
# for the block anchor, proves its deposit, finalizes it and waits for the credit. --json prints
# one line per step; this job's log keeps USDC's for the runner's asserts, and
# /logs/cli-deposit-token.log the second token's.
# ci-entry caps a job's memory by its name and only this name gets what a deposit proof needs, so
# both run in this job. They share the prover directory, so they share its proving key and the CLI
# proves one at a time.
# Input: the deployer's private key.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load evm.env
p=$WORK/deposit-prover
mkdir -p "$p/configs" "$p/data" /cache/deposit-srs
for t in fetch_deposit_data export_vk_blob export_blake2b_proof; do ln -sf "/bin-ci/$t" "$p/$t"; done
cp /bin-ci/circuit_params.json "$p/configs/"
# The deposit SRS: Hermez k=18, cut from the k=21 file the way the release does it.
[ -s /cache/deposit-srs/kzg_params_18.srs ] ||
  downsize_srs --input /params/kzg_bn254_21.srs --output /cache/deposit-srs/kzg_params_18.srs --k 18
ln -sf /cache/deposit-srs/kzg_params_18.srs "$p/data/kzg_params_18.srs"
export BRIDGE_DEPOSIT_PRIVATE_KEY; BRIDGE_DEPOSIT_PRIVATE_KEY=$(tr -d '[:space:]' </run/input)
export BRIDGE_DEPOSIT_FROM=$FROM RPC_URL=$ARB_DEPOSIT_RPC BRIDGE_ADDRESS BRIDGE_GQL_ENDPOINT=$GQL
export USDC_BRIDGE_ACCOUNT_ID=1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a
export BRIDGE_DEPOSIT_PROVER_DIR=$p BRIDGE_DEPOSIT_CONFIRMATIONS=${CONFIRMATIONS:-1}

# <state and work directory prefix> [token]
deposit() {
  export BRIDGE_DEPOSIT_STATE_DIR=$WORK/$1-state BRIDGE_WORK_DIR=$WORK/$1-work
  mkdir -p "$BRIDGE_DEPOSIT_STATE_DIR" "$BRIDGE_WORK_DIR"
  log "deposit $AMOUNT ${2:-USDC} from $FROM to $TO"
  exec ackinacki-bridge --json --yes --non-interactive deposit --network arbitrum-sepolia --qr-mode local-key \
    --amount "$AMOUNT" ${2:+--token "$2"} --to "$TO" --anchor-timeout-s "${ANCHOR_TIMEOUT_S:-5400}"
}
deposit cli-deposit &
usdc=$!
until grep -q '"step":"evm_confirm","state":"done"' /logs/cli-deposit.log; do
  kill -0 "$usdc" 2>/dev/null || { wait "$usdc"; exit; }
  sleep 5
done
deposit cli-deposit-token "$TOKEN" >/logs/cli-deposit-token.log 2>&1 &
for _ in 1 2; do
  wait -n || { code=$?; log "a deposit exited $code; stopping the other"; kill $(jobs -p) 2>/dev/null || true; exit "$code"; }
done
