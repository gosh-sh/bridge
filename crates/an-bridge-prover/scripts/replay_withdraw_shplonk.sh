#!/usr/bin/env bash
# Replay a Circuit-4 withdrawal against a retained enriched witness JSON, using
# the fixed SHPLONK pipeline (Circuit4ShplonkPipeline via `prove-withdraw-shplonk`).
#
# WHY THIS EXISTS
#   Yesterday's `withdraw-e2e` run submitted a raw halo2 proof (5888 bytes) and
#   reverted with `WithdrawalProofRejected() = 0x11d9a627`. driver.rs was calling
#   `SubprocessWithdrawalProver` (raw halo2) instead of `Circuit4ShplonkPipeline`
#   (22-instance SHPLONK aggregator calldata the deployed Yul verifier accepts).
#   Commit b22f6c7 wired driver.rs to the correct pipeline.
#
#   Rather than fire a fresh burn (daemon-live is stopped; new events' bundles
#   would not be in `prover_state.json` so `enrich_witness` would fail with
#   "anchor not in state"), we reuse yesterday's enriched witness and re-prove
#   through the fixed pipeline. The covering anchor (layer 1, seqno 8923136)
#   is still in `_layerWindows` (dataLen ~7 of 128), so the submit path is
#   fully exercisable.
#
# USAGE
#   scripts/replay_withdraw_shplonk.sh                # prove + dry-run only (safe)
#   scripts/replay_withdraw_shplonk.sh --live         # prove + dry-run + live submit
#   WITNESS=... scripts/replay_withdraw_shplonk.sh    # override witness path

set -euo pipefail
cd "$(dirname "$0")/.."

WITNESS="${WITNESS:-work_dir/event_234700_witness.json}"
LIVE=0
for arg in "$@"; do
  case "$arg" in
    --live)        LIVE=1 ;;
    --witness=*)   WITNESS="${arg#--witness=}" ;;
    -h|--help)
      grep -E '^#( |$)' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "!!! unknown arg: $arg" >&2; exit 2 ;;
  esac
done

if [ ! -f "$WITNESS" ]; then
  echo "!!! witness not found: $WITNESS" >&2
  exit 1
fi

TS=$(date +%Y%m%d_%H%M%S)
mkdir -p logs proofs work_dir/shplonk-snark-replay

set -a && source .env.shellnet && set +a

SEQ="${TS: -6}"
OUT="proofs/proof_event_replay_${TS}.json"
LOG_PROVE="logs/replay_prove_${TS}.log"
LOG_SUBMIT="logs/replay_submit_${TS}.log"

# The aggregate-proof subprocess `cd`s into $BRIDGE_AGGREGATOR_DIR, so it must
# receive an ABSOLUTE snark-dir path (a relative "work_dir/..." would resolve
# against the aggregator's cwd and miss the intermediate `.snark` file).
SNARK_DIR_ABS=$(python3 -c "import os,sys; print(os.path.abspath('work_dir/shplonk-snark-replay'))")

echo "==> Step 1/3  prove SHPLONK  (witness=$WITNESS, seq=$SEQ)"
echo "    (first run cold: outer PK keygen ~a few min; warm: ~30-60s)"
./target/release/relayer prove-withdraw-shplonk \
  --witness       "$WITNESS" \
  --aggregator-dir "$BRIDGE_AGGREGATOR_DIR" \
  --verifiers-dir  "$BRIDGE_VERIFIERS_DIR" \
  --params-dir     "$BRIDGE_PARAMS_DIR" \
  --snark-dir      "$SNARK_DIR_ABS" \
  --out            "$OUT" \
  --seq-no         "$SEQ" \
  --pk-cache-dir   "$BRIDGE_PARAMS_DIR/pk_cache" \
  2>&1 | tee "$LOG_PROVE"

if [ ! -f "$OUT" ]; then
  echo "!!! prove step did not produce $OUT — see $LOG_PROVE" >&2
  exit 1
fi
CALLDATA_BYTES=$(python3 -c "import json,sys; d=json.load(open('$OUT')); print(len(bytes.fromhex(d['proof_hex'])))")
echo "    proof_event written: $OUT   (calldata=${CALLDATA_BYTES}B — sanity: SHPLONK ≈ several KB, raw halo2 was 5888B)"

echo ""
echo "==> Step 2/3  dry-run submit (eth_call, no gas)"
./target/release/relayer submit-withdraw \
  --proof-event    "$OUT" \
  --rpc-url        "$RPC_URL" \
  --bridge-address "$BRIDGE_ADDRESS" \
  --private-key    "$RELAYER_PRIVATE_KEY" \
  --dry-run \
  2>&1 | tee "$LOG_SUBMIT"

if [ "$LIVE" -ne 1 ]; then
  echo ""
  echo "==> dry-run OK.  Re-run with --live to submit for real:"
  echo "     scripts/replay_withdraw_shplonk.sh --live --witness=$WITNESS"
  exit 0
fi

echo ""
echo "==> Step 3/3  live submit (spends gas)"
./target/release/relayer submit-withdraw \
  --proof-event    "$OUT" \
  --rpc-url        "$RPC_URL" \
  --bridge-address "$BRIDGE_ADDRESS" \
  --private-key    "$RELAYER_PRIVATE_KEY" \
  2>&1 | tee -a "$LOG_SUBMIT"

echo ""
echo "==> success signal:  grep 'withdrawByProof paid out' $LOG_SUBMIT"
echo "==> verify on-chain:"
echo "     cast logs --rpc-url \$RPC_URL --address \$BRIDGE_ADDRESS \\"
echo "       'WithdrawalExecuted(uint256,address,uint256,uint256)' --from-block -200"
