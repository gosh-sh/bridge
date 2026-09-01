#!/usr/bin/env bash
# Dry-run smoke test for `ackinacki-bridge withdraw`.
#
# What it does:
#   Runs `ackinacki-bridge withdraw --dry-run` against the
#   pre-deployed shellnet bridge (or whatever `BRIDGE_ADDRESS` you set in
#   `config/bridge_config`). `--dry-run` is preflight-only: it validates
#   flags, key file perms, single-custodian check, USDCBridge resolution,
#   and the ECC[3] balance, then stops. It does NOT compose the burn
#   message, wait for the WithdrawalInitiated event, produce the Circuit-4
#   proof, or call `dry_run_withdraw` on the EVM side. Exit code 0 means
#   "argument shape is sane and the source multisig is in a burnable
#   state", not "every stage of a real run would have succeeded" — for
#   that, drop `--dry-run` and use `live_smoke.sh`.
#
# Coordination with the running daemon:
#   Some daemon-live is expected to be running against the target deploy
#   (anywhere on the internet — not necessarily this host). The CLI does
#   NOT read the daemon's `prover_state.json` — it resurrects BridgeState
#   from the deployed contract via `--rpc-url` + `--bridge-address` every
#   invocation. Dry-run never fires a burn, so daemon liveness is not a
#   hard requirement for this script; sanity-check with the runbook
#   §"Quick resume checklist" step 1 before graduating to `live_smoke.sh`.
#
# Prerequisites:
#   1. `config/bridge_config` populated (checked in with the shellnet L2
#      reference deploy pinned; edit `BRIDGE_ADDRESS` if you deployed
#      your own).
#   2. `BURNER_PRIVATE_KEY` exported in your shell (the file deliberately
#      omits it — see the runbook §"Wallet setup and bridge config").
#   3. Caller exports the per-withdrawal identity variables below.
#      `scripts/deploy_msig_and_mint.sh` produces `WITHDRAW_FROM` and
#      `WITHDRAW_FROM_KEYS` in the right form; the other three are yours.
#
# Escape hatch (advanced):
#   If you already have a relayer-style `L{1,2}_config/env` populated
#   (via `../an-bridge-prover/scripts/deploy_bridge_bundle.sh`) and want
#   to reuse its `BRIDGE_ADDRESS` / `RPC_URL` / etc., export
#   `BRIDGE_CONFIG_DIR=/absolute/path/to/L1_config` (or `L2_config`)
#   before running. The script will source `$BRIDGE_CONFIG_DIR/env`
#   instead of the standalone `config/bridge_config`.
#
# Caller-supplied env (no defaults — a wrong value spends real money):
#   WITHDRAW_FROM        — `<dapp_id>::<account_id>` of the source AN multisig
#   WITHDRAW_FROM_KEYS   — path to that multisig owner's keys.json (mode 0600)
#   WITHDRAW_TO          — EVM recipient (0x… or CAIP-10 eip155:<id>:0x…)
#   WITHDRAW_TO_CHAIN    — numeric EIP-155 chain id (11155111 for Sepolia)
#   WITHDRAW_AMOUNT      — decimal USDC (e.g. 1.000000), ≤ 6 fractional digits

set -euo pipefail

# Anchor at the CLI crate root so `config/bridge_config`'s relative paths
# (BRIDGE_PARAMS_DIR=../an-bridge-prover/params, etc.) resolve correctly.
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
  if [ -n "${BRIDGE_CONFIG_DIR:-}" ]; then
    echo "    Run crates/an-bridge-prover/scripts/deploy_bridge_bundle.sh first,"
    echo "    or unset BRIDGE_CONFIG_DIR to use the standalone config/bridge_config."
  else
    echo "    Restore it from git, or point BRIDGE_CONFIG_DIR at a"
    echo "    relayer-style L{1,2}_config directory."
  fi
  exit 1
fi
set -a && source "$ENV_FILE" && set +a

# --- BURNER_PRIVATE_KEY comes from the caller ---------------------------------
: "${BURNER_PRIVATE_KEY:?export BURNER_PRIVATE_KEY=0x… (bridge_config deliberately omits it — see runbook §Wallet setup)}"

# --- Caller identity ----------------------------------------------------------
: "${WITHDRAW_FROM:?export WITHDRAW_FROM='<dapp_id>::<account_id>' (or run scripts/deploy_msig_and_mint.sh)}"
: "${WITHDRAW_FROM_KEYS:?export WITHDRAW_FROM_KEYS=/path/to/owner.keys.json}"
: "${WITHDRAW_TO:?export WITHDRAW_TO=0xRecipient}"
: "${WITHDRAW_TO_CHAIN:?export WITHDRAW_TO_CHAIN=11155111}"
: "${WITHDRAW_AMOUNT:?export WITHDRAW_AMOUNT=1.000000}"

# --- Per-withdrawal working dirs ---------------------------------------------
# Under the CLI crate root by default so the standalone tree is self-contained.
WORK_DIR="${WORK_DIR:-./work_dir}"
STATE_DIR="${BRIDGE_WITHDRAW_STATE_DIR:-./withdraw-state}"
mkdir -p "$WORK_DIR" "$STATE_DIR"

# aggregate-proof subprocess cd's into $BRIDGE_AGGREGATOR_DIR; --snark-dir must
# be absolute (same bug guarded in launch_withdraw_e2e_real.sh).
SNARK_DIR_ABS=$(python3 -c "import os; print(os.path.abspath('$WORK_DIR/shplonk-snark'))")

TS=$(date +%Y%m%d_%H%M%S)
LOG="$WORK_DIR/withdraw_smoke_dry_${TS}.log"

echo "==> Dry-run smoke ($ENV_KIND)"
echo "    from=$WITHDRAW_FROM to=$WITHDRAW_TO amount=$WITHDRAW_AMOUNT chain=$WITHDRAW_TO_CHAIN"
echo "    bridge=$BRIDGE_ADDRESS  rpc=$RPC_URL"
echo "    log=$LOG"

# Manifest lives in the CLI crate's sub-workspace parent.
exec cargo run --release -p ackinacki-bridge \
  --manifest-path ../an-bridge-prover/Cargo.toml -- \
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
    --eth-private-key   "$BURNER_PRIVATE_KEY" \
    --aggregator-dir    "$BRIDGE_AGGREGATOR_DIR" \
    --verifiers-dir     "$BRIDGE_VERIFIERS_DIR" \
    --params-dir        "$BRIDGE_PARAMS_DIR" \
    --snark-dir         "$SNARK_DIR_ABS" \
    --pk-cache-dir      "${BRIDGE_PK_CACHE_DIR:-$BRIDGE_PARAMS_DIR/pk_cache}" \
    --work-dir          "$WORK_DIR" \
    --state-dir         "$STATE_DIR" \
    2>&1 | tee "$LOG"
