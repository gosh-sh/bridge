#!/usr/bin/env bash
# Shellnet AN→ETH E2E on Sepolia: deploy → verifyBlock chain → withdrawByProof.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PROOFS_DIR="${PROOFS_DIR:-/home/ubuntu/bridge-e2e/acki-nacki-to-eth-bridge-halo2-prover/proofs}"
RELAYER="${RELAYER:-/home/ubuntu/bridge-e2e/bin/relayer}"
RPC_URL="${RPC_URL:?}"
BRIDGE_ADDRESS="${BRIDGE_ADDRESS:?}"
RELAYER_PRIVATE_KEY="${RELAYER_PRIVATE_KEY:?}"

# Shellnet proof chain: 1083392 → 1083904 → 1084416 (withdrawal finalRoot anchor).
VERIFY_CHAIN="${VERIFY_CHAIN:-1083392 1083904 1084416}"
EVENT_PROOF="${EVENT_PROOF:-$PROOFS_DIR/proof_event_000000.json}"

echo "== wrap verifyBlock proofs =="
LAST_SEEN=0
for SEQ in $VERIFY_CHAIN; do
  GNARK_LAST_SEEN_PI="$LAST_SEEN" python3 "$ROOT/scripts/wrap_partner_proof_groth16.py" \
    "$PROOFS_DIR/proof_${SEQ}.json"
  LAST_SEEN="$SEQ"
done

echo "== verifyBlock preflight =="
for SEQ in $VERIFY_CHAIN; do
  "$RELAYER" verify-prover-proof \
    --proofs-dir "$PROOFS_DIR" --block-seq-no "$SEQ" \
    --rpc-url "$RPC_URL" --bridge-address "$BRIDGE_ADDRESS"
done

echo "== submit verifyBlock chain =="
for SEQ in $VERIFY_CHAIN; do
  "$RELAYER" submit-verify-block \
    --proofs-dir "$PROOFS_DIR" --block-seq-no "$SEQ" \
    --rpc-url "$RPC_URL" --bridge-address "$BRIDGE_ADDRESS" \
    --private-key "$RELAYER_PRIVATE_KEY"
done

echo "== wrap + submit withdrawal =="
python3 "$ROOT/scripts/wrap_proof_event_groth16.py" "$EVENT_PROOF"
"$RELAYER" submit-withdraw \
  --proof-event "$EVENT_PROOF" \
  --rpc-url "$RPC_URL" --bridge-address "$BRIDGE_ADDRESS" \
  --private-key "$RELAYER_PRIVATE_KEY" --dry-run
"$RELAYER" submit-withdraw \
  --proof-event "$EVENT_PROOF" \
  --rpc-url "$RPC_URL" --bridge-address "$BRIDGE_ADDRESS" \
  --private-key "$RELAYER_PRIVATE_KEY"

echo "=== AN→ETH E2E SUCCESS ==="
