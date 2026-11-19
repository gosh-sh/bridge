#!/usr/bin/env bash
# ackinacki-bridge deposit send: AMOUNT of TOKEN (usdc unless set) from FROM on Arbitrum Sepolia
# to the Acki Nacki account TO, signed with the deployer's key (--qr-mode local-key). The run
# ends once the deposit is confirmed on Arbitrum Sepolia: the deposit relayer finalizes it, and
# cli-deposit-proof proves it again afterwards with the deposit.tx_hash of the --json summary.
# --json prints one line per step; this job's log keeps them for the runner's asserts.
# Input: the deployer's private key.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load evm.env
TOKEN=${TOKEN:-usdc}
export BRIDGE_DEPOSIT_PRIVATE_KEY; BRIDGE_DEPOSIT_PRIVATE_KEY=$(tr -d '[:space:]' </run/input)
export BRIDGE_DEPOSIT_FROM=$FROM RPC_URL=$ARB_DEPOSIT_RPC BRIDGE_ADDRESS BRIDGE_GQL_ENDPOINT=$GQL
export USDC_BRIDGE_ACCOUNT_ID=1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a
export BRIDGE_DEPOSIT_CONFIRMATIONS=${CONFIRMATIONS:-1}
export BRIDGE_DEPOSIT_STATE_DIR=$WORK/deposit-send-$TOKEN-state BRIDGE_WORK_DIR=$WORK/deposit-send-$TOKEN-work
mkdir -p "$BRIDGE_DEPOSIT_STATE_DIR" "$BRIDGE_WORK_DIR"
log "deposit send $AMOUNT $TOKEN from $FROM to $TO"
exec ackinacki-bridge --json --yes --non-interactive deposit send --network arbitrum-sepolia \
  --qr-mode local-key --token "$TOKEN" --amount "$AMOUNT" --to "$TO"
