#!/usr/bin/env bash
# The bridge's genesis anchors at the local network's latest 1024-block boundary.
# Input: the network's BK set as bk_set.json ({"0": "<BLS pubkey>", ...}).
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
cp /run/input "$OUT/bk_set.json"
deadline=$(( $(date +%s) + ${GENESIS_WAIT_SECS:-1800} ))
until compute_bridge_anchors --endpoint "$GQL" --bk-set-config "$OUT/bk_set.json" --at-head --level 1 \
    >"$WORK/genesis.out" 2>"$WORK/genesis.err"; do
  [ "$(date +%s)" -lt "$deadline" ] || { cat "$WORK/genesis.err"; exit 1; }
  log "no seed yet (the network needs 1024 blocks): $(tail -1 "$WORK/genesis.err")"
  sleep 30
done
grep -E '^[A-Z_]+=' "$WORK/genesis.out" >"$OUT/genesis.env"
cat "$OUT/genesis.env"
