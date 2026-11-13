#!/usr/bin/env bash
# The bridge's genesis at the network's own start (seq_no 0), at L2 as the live relayer runs: no
# key block to wait for. compute_bridge_anchors --seed-seqno 0 reads the BK set at 0 from the
# network's GraphQL (blockchain.latestBkSet and bkSetUpdates, checked against the block leaves)
# and prints its commitment with the zero anchor. daemon-live starts from it with
# BRIDGE_BOOTSTRAP_SEQNO=0, builds the same set from GraphQL, and proves its first bundle at
# 16384. Writes /out/genesis.env. No input.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
compute_bridge_anchors --endpoint "$GQL" --seed-seqno 0 --level 2 >"$WORK/genesis.out"
grep -E '^[A-Z_]+=' "$WORK/genesis.out" >"$OUT/genesis.env"
cat "$OUT/genesis.env"
