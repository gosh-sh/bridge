#!/usr/bin/env bash
# Coordinated withdraw-E2E launcher.
#
# ORDER MATTERS:
#   1. withdraw-e2e first  → baseline snapshot excludes NO events yet from burn
#   2. burn second         → new WithdrawalInitiated msg_id not in baseline
#   3. withdraw-e2e picks up the new event, enriches, proves, dry-runs
#
# Reversing the order lands in Case 2b of the runbook (baseline includes
# past event → withdraw-e2e times out waiting).

set -euo pipefail
cd "$(dirname "$0")/.."

TS=$(date +%Y%m%d_%H%M%S)
WITHDRAW_LOG="logs/withdraw_dry_${TS}.log"
BURN_LOG="logs/withdrawal_burn_${TS}.log"

if [ ! -f L1_config/env ]; then
  echo "!!! L1_config/env not found — see crates/bridge-relayer-daemon/docs/live_relayer_bridge_verifyBlock_runbook.md"
  echo "    (per-mode config layout: L{1,2}_config/{env,state,proofs})"
  exit 1
fi
set -a && source L1_config/env && set +a

# aggregate-proof subprocess cd's into $BRIDGE_AGGREGATOR_DIR, so --snark-dir
# must be an ABSOLUTE path (a relative "work_dir/..." would resolve against
# the aggregator's cwd and miss the intermediate .snark file — same bug
# `replay_withdraw_shplonk.sh:59` guards against).
SNARK_DIR_ABS=$(python3 -c "import os,sys; print(os.path.abspath('L1_config/work_dir/shplonk-snark'))")

echo "==> Step 1/3  launch withdraw-e2e (baseline snapshot before burn)"
nohup ./target/release/relayer withdraw-e2e \
  --gql-endpoint "$BRIDGE_GQL_ENDPOINT" \
  --prover-state-path L1_config/state/prover_state.json \
  --window-size 128 \
  --bridge-account-id 1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a \
  --bridge-dapp-id    0000000000000000000000000000000000000000000000000000000000000000 \
  --anchor-layer auto \
  --work-dir L1_config/work_dir \
  --aggregator-dir "$BRIDGE_AGGREGATOR_DIR" \
  --verifiers-dir  "$BRIDGE_VERIFIERS_DIR" \
  --params-dir     "$BRIDGE_PARAMS_DIR" \
  --snark-dir      "$SNARK_DIR_ABS" \
  --pk-cache-dir   "$BRIDGE_PARAMS_DIR/pk_cache" \
  --prover-out-dir L1_config/proofs \
  --prover-seq-no "${TS: -6}" \
  --event-wait-s 900 \
  --dry-run \
  > "$WITHDRAW_LOG" 2>&1 &
WITHDRAW_PID=$!
echo "    PID=$WITHDRAW_PID  log=$WITHDRAW_LOG"

echo "==> Step 2/3  wait 15s for baseline snapshot to complete"
sleep 15
if ! kill -0 "$WITHDRAW_PID" 2>/dev/null; then
  echo "!!! withdraw-e2e exited during baseline (see $WITHDRAW_LOG)"
  tail -30 "$WITHDRAW_LOG"
  exit 1
fi
if ! grep -q 'waiting for WithdrawalInitiated' "$WITHDRAW_LOG"; then
  echo "!!! withdraw-e2e did not reach capture stage yet — tailing log then aborting"
  tail -30 "$WITHDRAW_LOG"
  kill "$WITHDRAW_PID" 2>/dev/null || true
  exit 1
fi
echo "    baseline snapshot OK; withdraw-e2e now polling"

echo "==> Step 3/3  fire burn (python)"
MODE=shellnet python3 python/test_deploy_and_withdraw_only.py 2>&1 | tee "$BURN_LOG"
BURN_STATUS=${PIPESTATUS[0]}
if [ "$BURN_STATUS" -ne 0 ]; then
  echo "!!! burn script exited $BURN_STATUS; withdraw-e2e still running (PID=$WITHDRAW_PID)"
  echo "!!! kill it manually if you want to abort: kill $WITHDRAW_PID"
  exit "$BURN_STATUS"
fi

echo "==> All launched.  withdraw-e2e PID=$WITHDRAW_PID"
echo "    Follow it with:  tail -f $WITHDRAW_LOG"
echo "    Expected timeline:"
echo "      + capture event      (~30s from burn)"
echo "      + enrich witness     (fails if state hasn't caught up yet)"
echo "      + Circuit 4 prove    (~4 min, first time; ~2 min warm)"
echo "      + dry-run eth_call   (<1s)"
echo "    If enrich_witness fails with 'anchor not in state', wait for the"
echo "    daemon to land the covering bundle then re-run this script"
echo "    (fresh baseline will exclude the just-fired event → Case 2b)."
