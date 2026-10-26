#!/usr/bin/env bash
# The bridge's genesis at the network's own start (seq_no 0), at L2 as the live relayer runs: no
# key block to wait for. The BK set at 0 is the zerostate's; seed_prover_bk_set replays nothing
# up to 0 and writes it in the prover's form, and compute_bridge_anchors --seed-seqno 0 prints
# the zero anchor without reading the chain. daemon-live starts from it with
# BRIDGE_BOOTSTRAP_SEQNO=0 and proves its first bundle at 16384. Writes /out/bk_set.json (the set,
# for daemon-live) and /out/genesis.env.
# Input: the network's genesis BK set, {"<index>": "<BLS pubkey>"}: the bk_nodes the zerostate
# generator recorded in config/keys_config.json.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
cp /run/input "$WORK/genesis_bk_set.json"
BRIDGE_GQL_ENDPOINT=$GQL seed_prover_bk_set --cap-height 0 --genesis "$WORK/genesis_bk_set.json" \
  --out "$WORK/prover_bk_set_0.json"
python3 -c 'import json,sys; json.dump(json.load(open(sys.argv[1]))["pubkeys_hex"], open(sys.argv[2], "w"))' \
  "$WORK/prover_bk_set_0.json" "$OUT/bk_set.json"
compute_bridge_anchors --endpoint "$GQL" --bk-set-config "$OUT/bk_set.json" --seed-seqno 0 --level 2 \
  >"$WORK/genesis.out"
grep -E '^[A-Z_]+=' "$WORK/genesis.out" >"$OUT/genesis.env"
cat "$OUT/genesis.env"
