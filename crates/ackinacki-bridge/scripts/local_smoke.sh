#!/usr/bin/env bash
# Dry-run smoke test for `ackinacki-bridge withdraw`.
#
# WHAT IT DOES
# ------------
# Runs `ackinacki-bridge withdraw --dry-run` against the network
# profile pointed to by `$BRIDGE_CONFIG` (default: symlink to
# bridge_config.shellnet). `--dry-run` is preflight-only: it validates
# flags, key file perms, single-custodian check, USDCBridge resolution,
# and the ECC[3] balance, then stops. It does NOT compose the burn
# message, wait for the WithdrawalInitiated event, produce the
# Circuit-4 proof, or call `dry_run_withdraw` on the EVM side. Exit
# code 0 means "argument shape is sane and the source multisig is in a
# burnable state", not "every stage of a real run would have
# succeeded" — for that, drop `--dry-run` and use `live_smoke.sh`.
#
# All network config comes from `$BRIDGE_CONFIG`; this script only
# passes the five per-withdrawal intent flags. See `live_smoke.sh`
# header for the profile-switching one-liner.

set -euo pipefail

cd "$(dirname "$0")/.."   # crates/ackinacki-bridge/

export BRIDGE_CONFIG="${BRIDGE_CONFIG:-config/bridge_config}"
if [ ! -f "$BRIDGE_CONFIG" ]; then
  echo "!!! BRIDGE_CONFIG=$BRIDGE_CONFIG not found."
  echo "    Point BRIDGE_CONFIG at one of config/bridge_config.{shellnet,local,mainnet}"
  echo "    or restore the default symlink from git."
  exit 1
fi

: "${BURNER_PRIVATE_KEY:?export BURNER_PRIVATE_KEY=0x… (profile deliberately omits it — see README §Wallet setup)}"
: "${WITHDRAW_FROM:?export WITHDRAW_FROM='<dapp_id>::<account_id>' (or run scripts/deploy_msig_and_mint.sh)}"
: "${WITHDRAW_FROM_KEYS:?export WITHDRAW_FROM_KEYS=/path/to/owner.keys.json}"
: "${WITHDRAW_TO:?export WITHDRAW_TO=0xRecipient}"
: "${WITHDRAW_TO_CHAIN:?export WITHDRAW_TO_CHAIN=11155111}"
: "${WITHDRAW_AMOUNT:?export WITHDRAW_AMOUNT=1.000000}"

# --- snark_dir must be absolute (see live_smoke.sh for rationale) -------------
_snark_rel=$(awk -F= '/^[[:space:]]*BRIDGE_SNARK_DIR[[:space:]]*=/{v=$2} END{gsub(/^[[:space:]]+|[[:space:]]+$/,"",v); print v}' "$BRIDGE_CONFIG")
_snark_rel="${_snark_rel:-./work_dir/shplonk-snark}"
export BRIDGE_SNARK_DIR="$(python3 -c "import os; print(os.path.abspath('$_snark_rel'))")"
mkdir -p "$(python3 -c "import os; print(os.path.dirname('$BRIDGE_SNARK_DIR'))")"

TS=$(date +%Y%m%d_%H%M%S)
LOG_DIR="${BRIDGE_WORK_DIR:-./work_dir}"
mkdir -p "$LOG_DIR"
LOG="$LOG_DIR/withdraw_smoke_dry_${TS}.log"

echo "==> Dry-run smoke  BRIDGE_CONFIG=$BRIDGE_CONFIG"
echo "    from=$WITHDRAW_FROM to=$WITHDRAW_TO amount=$WITHDRAW_AMOUNT chain=$WITHDRAW_TO_CHAIN"
echo "    log=$LOG"

exec cargo run --release -p ackinacki-bridge \
  --manifest-path ../bridge-prover-libraries/Cargo.toml -- \
  withdraw --dry-run --yes \
    --from       "$WITHDRAW_FROM" \
    --from-keys  "$WITHDRAW_FROM_KEYS" \
    --to         "$WITHDRAW_TO" \
    --to-chain   "$WITHDRAW_TO_CHAIN" \
    --amount     "$WITHDRAW_AMOUNT" \
    2>&1 | tee "$LOG"
