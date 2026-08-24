#!/usr/bin/env bash
# Coordinated withdraw-E2E launcher — REAL on-chain submit.
#
# Same ordering discipline as launch_withdraw_e2e.sh (baseline BEFORE burn
# → avoid Case 2b permanent-timeout), but without --dry-run. The Rust
# orchestrator (bridge-relayer-daemon/src/withdraw_e2e) will subprocess
# bridge-event-halo2-prover for Circuit 4 and then submit withdrawByProof
# to Sepolia in the same process.
#
# ORDER MATTERS:
#   1. withdraw-e2e first  → baseline snapshot excludes NO events yet from burn
#   2. burn second         → new WithdrawalInitiated msg_id not in baseline
#   3. withdraw-e2e picks up the new event, enriches, proves, submits
#
# Prereq: daemon-live must be running and inside its bundle 1 proving
# window. If daemon-live has been up for >10 min already, the covering
# bundle for a burn-at-head is bundle 2+ and enrich_witness will block
# waiting for the layer_hashes[K] entry to land in prover_state.json.

set -euo pipefail
cd "$(dirname "$0")/.."

TS=$(date +%Y%m%d_%H%M%S)
WITHDRAW_LOG="logs/withdraw_real_${TS}.log"
BURN_LOG="logs/withdrawal_burn_${TS}.log"

if [ ! -f L1_config/env ]; then
  echo "!!! L1_config/env not found — see docs/live_verifyBlock_runbook.md"
  exit 1
fi
set -a && source L1_config/env && set +a

# aggregate-proof subprocess cd's into $BRIDGE_AGGREGATOR_DIR, so --snark-dir
# must be an ABSOLUTE path (a relative "work_dir/..." would resolve against
# the aggregator's cwd and miss the intermediate .snark file — same bug
# `replay_withdraw_shplonk.sh:59` guards against).
SNARK_DIR_ABS=$(python3 -c "import os,sys; print(os.path.abspath('L1_config/work_dir/shplonk-snark'))")

echo "==> Step 1/3  launch withdraw-e2e REAL SUBMIT (baseline snapshot before burn)"
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
echo "      + capture event         (~30s from burn)"
echo "      + enrich witness        (blocks until covering bundle in prover_state.json)"
echo "      + Circuit 4 prove       (~5 min warm PK, ~20 min cold)"
echo "      + submit withdrawByProof + confirm (~30-60s)"
echo "    Success signal:"
echo "      grep 'withdrawByProof confirmed tx=' $WITHDRAW_LOG"
echo "    Verify on-chain:"
echo "      cast logs --address \$BRIDGE_ADDRESS --rpc-url \$RPC_URL \\"
echo "        'event WithdrawalExecuted(uint256,address,uint256,uint256)' --from-block -100"
echo ""
echo "    If enrich_witness stalls with 'anchor not in state' longer than"
echo "    ~15 min, the daemon is falling behind. Check daemon log for"
echo "    'verified seq_no=' progress. Kill withdraw-e2e (kill $WITHDRAW_PID)"
echo "    then re-run this launcher AFTER daemon catches up (fresh baseline"
echo "    will exclude the just-fired event — Case 2b — so you must fire a"
echo "    NEW burn on the next attempt)."
