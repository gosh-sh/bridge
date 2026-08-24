#!/usr/bin/env bash
# Coordinated withdraw-E2E launcher — L2 anchoring variant.
#
# Mirrors scripts/launch_withdraw_e2e.sh but for a bridge deployed with
# BRIDGE_ANCHOR_LEVEL=2. Key differences from the L1 launcher:
#
#   * Sources .env.shellnet.l2 (recommend keeping L2 env separate from L1
#     so operator can't accidentally cross-fire).
#   * Passes --anchor-layer 2 --i-know-the-wait to force strict L2
#     resolution. Do NOT use --anchor-layer auto here: auto probes L1
#     first and silently downgrades whenever the event falls inside the
#     L1 window following the covering T₂.
#   * --event-wait-s bumped to 7200 (2 h) to cover L2's worst-case
#     ~101 min single-bundle wait + slack. The daemon-side
#     ENRICH_TIMEOUT is 120 min per driver.rs:172; --event-wait-s here
#     bounds the CAPTURE stage, not the enrich stage.
#   * Baseline-check log message adjusted for the longer capture budget.
#
# See docs/live_withdrawByProof_runbook.md Case 8 (fresh L2 deploy)
# and Case 9 (2–3 sequential stress-loop) for the full operator flow.
#
# ORDER MATTERS (identical to L1 launcher):
#   1. withdraw-e2e first  → baseline snapshot excludes NO events from burn yet
#   2. burn second         → new WithdrawalInitiated msg_id not in baseline
#   3. withdraw-e2e picks up the new event, enriches, proves, dry-runs

set -euo pipefail
cd "$(dirname "$0")/.."

TS=$(date +%Y%m%d_%H%M%S)
WITHDRAW_LOG="logs/withdraw_l2_dry_${TS}.log"
BURN_LOG="logs/withdrawal_burn_l2_${TS}.log"

# L2-specific config dir. Keeping L1/L2 env separate prevents accidental
# cross-fire (e.g. L1 BRIDGE_ADDRESS vs L2 deploy). `L2_config/env` sources
# `shellnet.common` for the shared vars, then adds L2-only overrides
# (BRIDGE_ADDRESS, BRIDGE_BOOTSTRAP_SEQNO, BRIDGE_ANCHOR_LEVEL,
# BRIDGE_CONFIG_DIR=./L2_config).
if [ ! -f L2_config/env ]; then
  echo "!!! L2_config/env not found — see docs/live_verifyBlock_runbook.md"
  echo "    (per-mode config layout: L{1,2}_config/{env,state,proofs})"
  exit 1
fi
set -a && source L2_config/env && set +a

# Guard: make sure we're actually running against an L2 deploy.
if [ "${BRIDGE_ANCHOR_LEVEL:-1}" != "2" ]; then
  echo "!!! BRIDGE_ANCHOR_LEVEL=${BRIDGE_ANCHOR_LEVEL:-<unset>} but this launcher"
  echo "!!! is L2-only. Use scripts/launch_withdraw_e2e.sh for L1."
  exit 1
fi

# aggregate-proof subprocess cd's into $BRIDGE_AGGREGATOR_DIR, so --snark-dir
# must be an ABSOLUTE path (a relative "work_dir/..." would resolve against
# the aggregator's cwd and miss the intermediate .snark file — same bug
# `replay_withdraw_shplonk.sh:59` and the L1 launcher guard against).
SNARK_DIR_ABS=$(python3 -c "import os,sys; print(os.path.abspath('L2_config/work_dir/shplonk-snark'))")

echo "==> Step 1/3  launch withdraw-e2e (baseline snapshot before burn, L2)"
nohup ./target/release/relayer withdraw-e2e \
  --gql-endpoint "$BRIDGE_GQL_ENDPOINT" \
  --prover-state-path L2_config/state/prover_state.json \
  --window-size 128 \
  --bridge-account-id 1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a \
  --bridge-dapp-id    0000000000000000000000000000000000000000000000000000000000000000 \
  --anchor-layer 2 \
  --i-know-the-wait \
  --work-dir L2_config/work_dir \
  --aggregator-dir "$BRIDGE_AGGREGATOR_DIR" \
  --verifiers-dir  "$BRIDGE_VERIFIERS_DIR" \
  --params-dir     "$BRIDGE_PARAMS_DIR" \
  --snark-dir      "$SNARK_DIR_ABS" \
  --pk-cache-dir   "$BRIDGE_PARAMS_DIR/pk_cache" \
  --prover-out-dir L2_config/proofs \
  --prover-seq-no "${TS: -6}" \
  --event-wait-s 7200 \
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
echo "    baseline snapshot OK; withdraw-e2e now polling (up to 2 h capture budget)"

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
echo "    Expected L2 timeline (bundle stride = W² = 16384 seq_nos):"
echo "      + capture event      (~30s from burn)"
echo "      + enrich witness     (retries until covering T₂ lands; up to ~101 min"
echo "                            chain-time + ~10 min prover; 120 min hard budget)"
echo "      + Circuit 4 prove    (~4 min, first time; ~2 min warm)"
echo "      + dry-run eth_call   (<1s)"
echo "    Ground-truth L2 confirmation in the log:"
echo "      grep 'layer_idx=1' $WITHDRAW_LOG"
echo "    If enrich_witness times out at 120 min, the daemon never landed a"
echo "    covering L2 bundle — inspect daemon-live logs, not this pipeline."
