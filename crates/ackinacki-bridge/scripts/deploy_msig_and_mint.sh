#!/usr/bin/env bash
# Thin wrapper around scripts/deploy_msig_and_mint.py.
#
# Deploys a fresh single-custodian UpdateCustodianMultisigWallet on the
# target AN cluster (default: shellnet) and seeds it with 1 USDC on
# ECC[3] via USDCBridge.mintAndSend. Prints two eval-able env lines on
# stdout so the caller can do:
#
#   eval "$(scripts/deploy_msig_and_mint.sh)"
#   scripts/local_smoke.sh   # WITHDRAW_FROM + WITHDRAW_FROM_KEYS now set
#
# Everything else (tvm-cli output, tracer logs) goes to stderr, so the
# eval line does not pollute the shell.
#
# Env vars honored:
#   MODE                    "shellnet" (default) or "local"
#   NETWORK                 tvm-cli --url override (defaults per MODE)
#   GRAPHQL_URL             GQL endpoint override (defaults per MODE)
#   WORK_DIR                where the multisig keys.json + deployx
#                           artifacts land (default: ./work_dir under
#                           the CLI crate root)
#   USDC_BRIDGE_KEY_PATH    override the bundled bridge-owner key path
#                           (shellnet default: python/contracts/USDCBridge.shellnet.keys.json)

set -euo pipefail

cd "$(dirname "$0")/.."   # crates/bridge-withdraw-e2e-cli/

exec python3 scripts/deploy_msig_and_mint.py
