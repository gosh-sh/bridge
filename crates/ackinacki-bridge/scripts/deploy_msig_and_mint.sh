#!/usr/bin/env bash
# Thin wrapper around scripts/deploy_msig_and_mint.py.
#
# Deploys a fresh single-custodian UpdateCustodianMultisigWallet on the
# AN cluster declared by $BRIDGE_CONFIG (default: config/bridge_config,
# a symlink to bridge_config.shellnet) and seeds it with 1 USDC on
# ECC[3] via USDCBridge.mintAndSend. Prints two eval-able env lines on
# stdout so the caller can do:
#
#   eval "$(scripts/deploy_msig_and_mint.sh)"
#   scripts/local_smoke.sh   # WITHDRAW_FROM + WITHDRAW_FROM_KEYS now set
#
# Everything else (tvm-cli output, tracer logs) goes to stderr, so the
# eval line does not pollute the shell.
#
# Switching networks is a one-liner:
#   BRIDGE_CONFIG=config/bridge_config.local  scripts/deploy_msig_and_mint.sh
#
# Env vars honored:
#   BRIDGE_CONFIG           profile file (default: config/bridge_config
#                           symlink → bridge_config.shellnet). Supplies
#                           NETWORK, BRIDGE_GQL_ENDPOINT,
#                           USDC_BRIDGE_KEY_PATH.
#   BRIDGE_WORK_DIR         override where the multisig keys.json +
#                           deployx artifacts land (default: ./work_dir
#                           under the CLI crate root).

set -euo pipefail

cd "$(dirname "$0")/.."   # crates/ackinacki-bridge/

export BRIDGE_CONFIG="${BRIDGE_CONFIG:-config/bridge_config}"

exec python3 scripts/deploy_msig_and_mint.py
