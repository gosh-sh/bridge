#!/usr/bin/env bash
# Local dry-run smoke test for `bridge-withdraw-e2e-cli`.
#
# What it does:
#   Runs `bridge-withdraw-e2e-cli withdraw --dry-run` against a running
#   `daemon-live` on the shellnet. `--dry-run` is preflight-only: it
#   validates flags, key file perms, single-custodian check, USDCBridge
#   resolution, and the ECC[3] balance, then stops. It does NOT compose
#   the burn message, wait for the WithdrawalInitiated event, produce the
#   Circuit-4 proof, or call `dry_run_withdraw` on the EVM side. Exit
#   code 0 means "argument shape is sane and the source multisig is in a
#   burnable state", not "every stage of a real run would have
#   succeeded" — for that, drop `--dry-run` and use `live_smoke.sh`.
#
# Coordination with the running daemon:
#   The relayer daemon-live already owns bundle proving. The CLI itself
#   does NOT read the daemon's `prover_state.json` — it resurrects
#   BridgeState from the deployed contract via `--rpc-url` +
#   `--bridge-address` every invocation. The only reason the daemon
#   needs to be running is to keep the on-chain contract fed with fresh
#   `verifyBlock` transactions; the CLI never opens any daemon-private
#   file. UNLIKE the relayer's own `launch_withdraw_e2e_real.sh`, this
#   script does NOT need baseline-before-burn ordering — dry-run never
#   fires a burn, so the Case 2b (baseline excludes our own event)
#   problem does not apply.
#
# Prerequisites:
#   1. `daemon-live` running against the target deploy, caught up.
#   2. `L1_config/env` populated (see
#      `crates/an-bridge-prover/scripts/deploy_bridge_bundle.sh`).
#   3. Caller exports the per-withdrawal identity variables below.
#
# Caller-supplied env (no defaults — a wrong value spends real money):
#   WITHDRAW_FROM        — `<dapp_id>::<account_id>` of the source AN multisig
#   WITHDRAW_FROM_KEYS   — path to that multisig owner's keys.json (mode 0600)
#   WITHDRAW_TO          — EVM recipient (0x… or CAIP-10 eip155:<id>:0x…)
#   WITHDRAW_TO_CHAIN    — numeric EIP-155 chain id (11155111 for Sepolia)
#   WITHDRAW_AMOUNT      — decimal USDC (e.g. 1.000000), ≤ 6 fractional digits

set -euo pipefail
cd "$(dirname "$0")/../../.."   # bridge/

# --- Locate + source the per-mode env file ------------------------------------
CONFIG_DIR="${BRIDGE_CONFIG_DIR:-crates/an-bridge-prover/L1_config}"
ENV_FILE="$CONFIG_DIR/env"
if [ ! -f "$ENV_FILE" ]; then
  echo "!!! $ENV_FILE not found."
  echo "    Run crates/an-bridge-prover/scripts/deploy_bridge_bundle.sh first,"
  echo "    or point BRIDGE_CONFIG_DIR at an existing per-mode config dir."
  exit 1
fi
set -a && source "$ENV_FILE" && set +a

# --- Caller identity ----------------------------------------------------------
: "${WITHDRAW_FROM:?export WITHDRAW_FROM='<dapp_id>::<account_id>'}"
: "${WITHDRAW_FROM_KEYS:?export WITHDRAW_FROM_KEYS=/path/to/owner.keys.json}"
: "${WITHDRAW_TO:?export WITHDRAW_TO=0xRecipient}"
: "${WITHDRAW_TO_CHAIN:?export WITHDRAW_TO_CHAIN=11155111}"
: "${WITHDRAW_AMOUNT:?export WITHDRAW_AMOUNT=1.000000}"

# --- Per-withdrawal working dirs (dry-run still needs the aggregator cwd) ----
WORK_DIR="${WORK_DIR:-$CONFIG_DIR/work_dir}"
STATE_DIR="${BRIDGE_WITHDRAW_STATE_DIR:-$CONFIG_DIR/withdraw-state}"
mkdir -p "$WORK_DIR" "$STATE_DIR"

# aggregate-proof subprocess cd's into $BRIDGE_AGGREGATOR_DIR; --snark-dir must
# be absolute (same bug guarded in launch_withdraw_e2e_real.sh:37).
SNARK_DIR_ABS=$(python3 -c "import os; print(os.path.abspath('$WORK_DIR/shplonk-snark'))")

TS=$(date +%Y%m%d_%H%M%S)
LOG="$WORK_DIR/withdraw_smoke_dry_${TS}.log"

echo "==> Dry-run smoke: from=$WITHDRAW_FROM to=$WITHDRAW_TO amount=$WITHDRAW_AMOUNT chain=$WITHDRAW_TO_CHAIN"
echo "    log=$LOG"

exec cargo run --release -p bridge-withdraw-e2e-cli --manifest-path crates/an-bridge-prover/Cargo.toml -- \
  withdraw \
    --dry-run \
    --yes \
    --from        "$WITHDRAW_FROM" \
    --from-keys   "$WITHDRAW_FROM_KEYS" \
    --to          "$WITHDRAW_TO" \
    --to-chain    "$WITHDRAW_TO_CHAIN" \
    --amount      "$WITHDRAW_AMOUNT" \
    --gql-endpoint      "$BRIDGE_GQL_ENDPOINT" \
    --rpc-url           "$RPC_URL" \
    --bridge-address    "$BRIDGE_ADDRESS" \
    --eth-private-key   "$RELAYER_PRIVATE_KEY" \
    --aggregator-dir    "$BRIDGE_AGGREGATOR_DIR" \
    --verifiers-dir     "$BRIDGE_VERIFIERS_DIR" \
    --params-dir        "$BRIDGE_PARAMS_DIR" \
    --snark-dir         "$SNARK_DIR_ABS" \
    --pk-cache-dir      "$BRIDGE_PARAMS_DIR/pk_cache" \
    --work-dir          "$WORK_DIR" \
    --state-dir         "$STATE_DIR" \
    2>&1 | tee "$LOG"
