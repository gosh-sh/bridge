#!/usr/bin/env bash
# The bridge's genesis, built as crates/bridge-relayer-daemon/docs/live_relayer_bridge_runbook.md
# §3 builds it, at level 1: the seed S is the newest 1024-block boundary at or below the chain
# head (§3.1); the BK set that signs S is the network's genesis set replayed with
# seed_prover_bk_set up to S (§3.2); compute_bridge_anchors reads the anchors at S and refuses a
# set that does not match the commitment S records. Writes /out/bk_set.json (the set at S, for
# daemon-live) and /out/genesis.env.
# Input: the network's genesis BK set, {"<index>": "<BLS pubkey>"}: the bk_nodes the zerostate
# generator recorded in config/keys_config.json.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
cp /run/input "$WORK/genesis_bk_set.json"
deadline=$(( $(date +%s) + ${GENESIS_WAIT_SECS:-1800} ))
chain_head() {
  curl -fsS -m 20 "$GQL" -H 'Content-Type: application/json' \
    -d '{"query":"{ blockchain { blocks(last:1) { edges { node { seq_no } } } } }"}' |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["blockchain"]["blocks"]["edges"][-1]["node"]["seq_no"])'
}
until head=$(chain_head) && [ "$head" -ge 1024 ]; do
  [ "$(date +%s)" -lt "$deadline" ] || { log "the chain head did not reach 1024 in time"; exit 1; }
  log "chain head ${head:-unknown}: no 1024-block boundary yet"
  sleep 15
done
S=$(( head / 1024 * 1024 ))
log "chain head $head, seed S=$S"
BRIDGE_GQL_ENDPOINT=$GQL seed_prover_bk_set --cap-height "$S" --genesis "$WORK/genesis_bk_set.json" \
  --out "$WORK/prover_bk_set_$S.json"
python3 -c 'import json,sys; json.dump(json.load(open(sys.argv[1]))["pubkeys_hex"], open(sys.argv[2], "w"))' \
  "$WORK/prover_bk_set_$S.json" "$OUT/bk_set.json"
compute_bridge_anchors --endpoint "$GQL" --bk-set-config "$OUT/bk_set.json" --seed-seqno "$S" --level 1 \
  >"$WORK/genesis.out"
grep -E '^[A-Z_]+=' "$WORK/genesis.out" >"$OUT/genesis.env"
cat "$OUT/genesis.env"
