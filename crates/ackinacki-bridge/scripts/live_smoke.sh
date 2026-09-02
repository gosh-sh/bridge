#!/usr/bin/env bash
# Live shellnet smoke test for `ackinacki-bridge withdraw` — REAL submit.
#
# Broadcasts the AN burn AND the EVM `withdrawByProof`. Only use on
# shellnet or a testnet where the value is disposable.
#
# HOW IT WORKS
# ------------
# All network endpoints, bridge address, params/aggregator/verifiers
# dirs, and the L2 anchor selection are resolved by the CLI from
# `$BRIDGE_CONFIG`. This script therefore passes only the five
# per-withdrawal intent flags (--from / --from-keys / --to / --to-chain /
# --amount) to the binary — nothing else.
#
# Switching to local devnet is a one-liner:
#   BRIDGE_CONFIG=config/bridge_config.local  scripts/live_smoke.sh
#
# COORDINATION WITH THE RUNNING DAEMON
# ------------------------------------
# The daemon must be alive and caught up so the on-chain contract is
# being fed fresh `verifyBlock` transactions — stage 4b will block
# until `storedLastSeenBlockSeqNo` advances past the covering L2 bundle
# for our burn's block. If the daemon is stalled the CLI will hang
# there; sanity-check with the runbook §"Quick resume checklist" step 1
# before launching this script.
#
# EXIT CODES
# ----------
# 0 success / 2 preflight refused / 3 duplicate refused /
# 10 burn outcome unknown / 11 capture timeout /
# 12 proof failed / 13 EVM submit failed. See README.md for the table.

set -euo pipefail

cd "$(dirname "$0")/.."   # crates/ackinacki-bridge/

# --- Locate the profile file --------------------------------------------------
# `config/bridge_config` (unsuffixed) is a symlink → bridge_config.shellnet.
export BRIDGE_CONFIG="${BRIDGE_CONFIG:-config/bridge_config}"
if [ ! -f "$BRIDGE_CONFIG" ]; then
  echo "!!! BRIDGE_CONFIG=$BRIDGE_CONFIG not found."
  echo "    Point BRIDGE_CONFIG at one of config/bridge_config.{shellnet,local,mainnet}"
  echo "    or restore the default symlink from git."
  exit 1
fi

# --- Caller-supplied env (no defaults — a wrong value spends real money) ------
: "${BURNER_PRIVATE_KEY:?export BURNER_PRIVATE_KEY=0x… (profile deliberately omits it — see README §Wallet setup)}"
: "${WITHDRAW_FROM:?export WITHDRAW_FROM='<dapp_id>::<account_id>' (or run scripts/deploy_msig_and_mint.sh)}"
: "${WITHDRAW_FROM_KEYS:?export WITHDRAW_FROM_KEYS=/path/to/owner.keys.json}"
: "${WITHDRAW_TO:?export WITHDRAW_TO=0xRecipient}"
: "${WITHDRAW_TO_CHAIN:?export WITHDRAW_TO_CHAIN=11155111}"
: "${WITHDRAW_AMOUNT:?export WITHDRAW_AMOUNT=1.000000}"

# --- snark_dir must be absolute -----------------------------------------------
# The aggregate-proof subprocess cd's into $BRIDGE_AGGREGATOR_DIR, so a
# relative BRIDGE_SNARK_DIR resolves against the wrong CWD. Canonicalize
# to an absolute path and override the profile value before invoking.
# Read the profile-declared value without polluting the shell env: awk
# out the last matching KEY=VALUE line and strip surrounding whitespace.
_snark_rel=$(awk -F= '/^[[:space:]]*BRIDGE_SNARK_DIR[[:space:]]*=/{v=$2} END{gsub(/^[[:space:]]+|[[:space:]]+$/,"",v); print v}' "$BRIDGE_CONFIG")
_snark_rel="${_snark_rel:-./work_dir/shplonk-snark}"
export BRIDGE_SNARK_DIR="$(python3 -c "import os; print(os.path.abspath('$_snark_rel'))")"
mkdir -p "$(python3 -c "import os,sys; print(os.path.dirname('$BRIDGE_SNARK_DIR'))")"

TS=$(date +%Y%m%d_%H%M%S)
LOG_DIR="${BRIDGE_WORK_DIR:-./work_dir}"
mkdir -p "$LOG_DIR"
LOG="$LOG_DIR/withdraw_smoke_live_${TS}.log"

echo "==> LIVE submit  BRIDGE_CONFIG=$BRIDGE_CONFIG"
echo "    from=$WITHDRAW_FROM to=$WITHDRAW_TO amount=$WITHDRAW_AMOUNT chain=$WITHDRAW_TO_CHAIN"
echo "    snark_dir=$BRIDGE_SNARK_DIR  log=$LOG"
echo "    (this WILL broadcast an AN burn and an EVM withdrawByProof tx)"

exec cargo run --release -p ackinacki-bridge \
  --manifest-path ../bridge-prover-libraries/Cargo.toml -- \
  withdraw --yes \
    --from       "$WITHDRAW_FROM" \
    --from-keys  "$WITHDRAW_FROM_KEYS" \
    --to         "$WITHDRAW_TO" \
    --to-chain   "$WITHDRAW_TO_CHAIN" \
    --amount     "$WITHDRAW_AMOUNT" \
    2>&1 | tee "$LOG"
