#!/usr/bin/env bash
# Live shellnet smoke test for `ackinacki-bridge withdraw` — REAL submit.
#
# Broadcasts the AN burn AND the EVM `withdrawByProof`. Only use on
# shellnet or a testnet where the value is disposable.
#
# Differs from `local_smoke.sh` only in that `--dry-run` is dropped and
# the L2 anchor flags (`--anchor-layer 2 --i-know-the-wait`) are added.
# The pinned `BRIDGE_ADDRESS` in `config/bridge_config` is an L2-anchored
# deploy — the CLI refuses to talk to it without the anchor flag, and
# `--i-know-the-wait` acknowledges the ~91 min per-bundle L2 cadence
# that stage 4b (wait-for-coverage) may block on. Advanced users pointing
# at their own L1 deploy via `BRIDGE_CONFIG_DIR=…/L1_config` should
# override `ANCHOR_LAYER=1` in their shell before running (and drop the
# `--i-know-the-wait` requirement — L1 is 512-block stride, seconds not
# minutes).
#
# Coordination with the running daemon:
#   The daemon must be alive and caught up so the on-chain contract is
#   being fed fresh `verifyBlock` transactions — stage 4b will block
#   until `storedLastSeenBlockSeqNo` advances past the covering L2
#   bundle for our burn's block. If the daemon is stalled the CLI will
#   hang there; sanity-check with the runbook §"Quick resume checklist"
#   step 1 before launching this script.
#
# The CLI orchestrator uses `replay_latest = true` for capture, so
# baseline-before-burn ordering (as in the relayer's
# `launch_withdraw_e2e_real.sh`) is not required here — the CLI fires
# its own burn and chain-follows the specific `an_tx_hash` through GQL,
# so concurrent operators cannot capture each other's events.
#
# Exit codes:  0 success / 2 preflight refused / 3 duplicate refused /
#              10 burn outcome unknown / 11 capture timeout /
#              12 proof failed / 13 EVM submit failed
# See README.md for the full table.
#
# Escape hatch: same as `local_smoke.sh` — set `BRIDGE_CONFIG_DIR=…`
# before running to use a relayer-style `L{1,2}_config/env` instead of
# the standalone `config/bridge_config`.

set -euo pipefail

cd "$(dirname "$0")/.."   # crates/ackinacki-bridge/

# --- Locate + source the env file ---------------------------------------------
if [ -n "${BRIDGE_CONFIG_DIR:-}" ]; then
  ENV_FILE="$BRIDGE_CONFIG_DIR/env"
  ENV_KIND="relayer-style ($BRIDGE_CONFIG_DIR/env)"
else
  ENV_FILE="config/bridge_config"
  ENV_KIND="standalone (config/bridge_config)"
fi
if [ ! -f "$ENV_FILE" ]; then
  echo "!!! $ENV_FILE not found."
  exit 1
fi
set -a && source "$ENV_FILE" && set +a

: "${BURNER_PRIVATE_KEY:?export BURNER_PRIVATE_KEY=0x… (bridge_config deliberately omits it — see runbook §Wallet setup)}"
: "${WITHDRAW_FROM:?export WITHDRAW_FROM='<dapp_id>::<account_id>' (or run scripts/deploy_msig_and_mint.sh)}"
: "${WITHDRAW_FROM_KEYS:?export WITHDRAW_FROM_KEYS=/path/to/owner.keys.json}"
: "${WITHDRAW_TO:?export WITHDRAW_TO=0xRecipient}"
: "${WITHDRAW_TO_CHAIN:?export WITHDRAW_TO_CHAIN=11155111}"
: "${WITHDRAW_AMOUNT:?export WITHDRAW_AMOUNT=1.000000}"

# The shellnet reference deploy in `config/bridge_config` is L2-anchored;
# advanced users on an L1 deploy should `export ANCHOR_LAYER=1`.
ANCHOR_LAYER="${ANCHOR_LAYER:-2}"
if [ "$ANCHOR_LAYER" = "2" ]; then
  ANCHOR_FLAGS=(--anchor-layer 2 --i-know-the-wait)
else
  ANCHOR_FLAGS=(--anchor-layer "$ANCHOR_LAYER")
fi

WORK_DIR="${WORK_DIR:-./work_dir}"
STATE_DIR="${BRIDGE_WITHDRAW_STATE_DIR:-./withdraw-state}"
mkdir -p "$WORK_DIR" "$STATE_DIR"

SNARK_DIR_ABS=$(python3 -c "import os; print(os.path.abspath('$WORK_DIR/shplonk-snark'))")

TS=$(date +%Y%m%d_%H%M%S)
LOG="$WORK_DIR/withdraw_smoke_live_${TS}.log"

echo "==> LIVE submit ($ENV_KIND, anchor=L$ANCHOR_LAYER)"
echo "    from=$WITHDRAW_FROM to=$WITHDRAW_TO amount=$WITHDRAW_AMOUNT chain=$WITHDRAW_TO_CHAIN"
echo "    bridge=$BRIDGE_ADDRESS  rpc=$RPC_URL"
echo "    log=$LOG"
echo "    (this WILL broadcast an AN burn and an EVM withdrawByProof tx)"

exec cargo run --release -p ackinacki-bridge \
  --manifest-path ../bridge-prover-libraries/Cargo.toml -- \
  withdraw \
    --yes \
    "${ANCHOR_FLAGS[@]}" \
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
    --pk-cache-dir      "${BRIDGE_PK_CACHE_DIR:-$BRIDGE_PARAMS_DIR/pk_cache}" \
    --work-dir          "$WORK_DIR" \
    --state-dir         "$STATE_DIR" \
    2>&1 | tee "$LOG"
