#!/usr/bin/env bash
# Live shellnet smoke test for `bridge-withdraw-e2e-cli` — REAL submit.
#
# Broadcasts the AN burn AND the EVM `withdrawByProof`. Only use on
# shellnet or a testnet where the value is disposable.
#
# Differs from `local_smoke.sh` only in that `--dry-run` is dropped. All
# other coordination applies: the daemon must be running and caught up
# so the on-chain contract is being fed fresh `verifyBlock`
# transactions. The CLI itself does not read the daemon's
# `prover_state.json` — it resurrects BridgeState from the contract at
# every invocation.
#
# The CLI orchestrator uses `replay_latest = true` for capture, so
# baseline-before-burn ordering (as in the relayer's
# `launch_withdraw_e2e_real.sh`) is not required here — the CLI fires
# its own burn and picks up the youngest matching event unambiguously.
#
# Exit codes:  0 success / 2 preflight refused / 3 duplicate refused /
#              10 burn outcome unknown / 11 capture timeout /
#              12 proof failed / 13 EVM submit failed
# See README.md for the full table.

set -euo pipefail
cd "$(dirname "$0")/../../.."   # bridge/

CONFIG_DIR="${BRIDGE_CONFIG_DIR:-crates/an-bridge-prover/L1_config}"
ENV_FILE="$CONFIG_DIR/env"
if [ ! -f "$ENV_FILE" ]; then
  echo "!!! $ENV_FILE not found. Run deploy_bridge_bundle.sh first."
  exit 1
fi
set -a && source "$ENV_FILE" && set +a

: "${WITHDRAW_FROM:?export WITHDRAW_FROM='<dapp_id>::<account_id>'}"
: "${WITHDRAW_FROM_KEYS:?export WITHDRAW_FROM_KEYS=/path/to/owner.keys.json}"
: "${WITHDRAW_TO:?export WITHDRAW_TO=0xRecipient}"
: "${WITHDRAW_TO_CHAIN:?export WITHDRAW_TO_CHAIN=11155111}"
: "${WITHDRAW_AMOUNT:?export WITHDRAW_AMOUNT=1.000000}"

WORK_DIR="${WORK_DIR:-$CONFIG_DIR/work_dir}"
STATE_DIR="${BRIDGE_WITHDRAW_STATE_DIR:-$CONFIG_DIR/withdraw-state}"
mkdir -p "$WORK_DIR" "$STATE_DIR"

SNARK_DIR_ABS=$(python3 -c "import os; print(os.path.abspath('$WORK_DIR/shplonk-snark'))")

TS=$(date +%Y%m%d_%H%M%S)
LOG="$WORK_DIR/withdraw_smoke_live_${TS}.log"

echo "==> LIVE submit: from=$WITHDRAW_FROM to=$WITHDRAW_TO amount=$WITHDRAW_AMOUNT chain=$WITHDRAW_TO_CHAIN"
echo "    log=$LOG"
echo "    (this WILL broadcast an AN burn and an EVM withdrawByProof tx)"

exec cargo run --release -p bridge-withdraw-e2e-cli --manifest-path crates/an-bridge-prover/Cargo.toml -- \
  withdraw \
    --yes \
    --from        "$WITHDRAW_FROM" \
    --from-keys   "$WITHDRAW_FROM_KEYS" \
    --to          "$WITHDRAW_TO" \
    --to-chain    "$WITHDRAW_TO_CHAIN" \
    --amount      "$WITHDRAW_AMOUNT" \
    --gql-endpoint      "$BRIDGE_GQL_ENDPOINT" \
    --rpc-url           "$RPC_URL" \
    --bridge-address    "$BRIDGE_ADDRESS" \
    --eth-private-key   "$BURNER_PRIVATE_KEY" \
    --aggregator-dir    "$BRIDGE_AGGREGATOR_DIR" \
    --verifiers-dir     "$BRIDGE_VERIFIERS_DIR" \
    --params-dir        "$BRIDGE_PARAMS_DIR" \
    --snark-dir         "$SNARK_DIR_ABS" \
    --pk-cache-dir      "$BRIDGE_PARAMS_DIR/pk_cache" \
    --work-dir          "$WORK_DIR" \
    --state-dir         "$STATE_DIR" \
    2>&1 | tee "$LOG"
