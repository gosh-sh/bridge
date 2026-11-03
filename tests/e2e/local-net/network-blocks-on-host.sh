#!/usr/bin/env bash
# Hand the network to the tests only once its GraphQL serves the chain from its start:
# blocks 1 and 100 of the default thread, each within WAIT_SECS of the network coming up.
# Piped over ssh by .woodpecker/local-net-e2e.yaml (`bash -s`), right after net-up.
#
# The block manager indexes blocks only from the moment it attaches, and up.sh starts it
# once every node is running. A node that is slow to start lets the other four run ahead,
# and the blocks made meanwhile never reach GraphQL. daemon-live needs them for its first
# bundle (the layer-2 tree at block 16384 reads every 128th block from 128), so such a
# network would stall the Arbitrum run in test_70, ninety minutes later.
set -euo pipefail
GRAPHQL_URL=${GRAPHQL_URL:-http://127.0.0.1/graphql}
WAIT_SECS=${WAIT_SECS:-300}
THREAD=00000000000000000000000000000000000000000000000000000000000000000000

gql() {
  curl -s --max-time 5 "$GRAPHQL_URL" -H 'Content-Type: application/json' --data "{\"query\":\"$1\"}"
}
has_block() {
  gql "{ blockchain { blockByHeight(thread_id: \\\"$THREAD\\\", height: $1) { seq_no } } }" \
    | jq -e --argjson h "$1" '.data.blockchain.blockByHeight.seq_no == $h' >/dev/null 2>&1
}

deadline=$(( $(date +%s) + WAIT_SECS ))
for h in 1 100; do
  until has_block "$h"; do
    if [ "$(date +%s)" -ge "$deadline" ]; then
      last=$(gql '{ blockchain { blocks(last: 1) { edges { node { seq_no } } } } }' \
        | jq -r '.data.blockchain.blocks.edges[0].node.seq_no // "none"' 2>/dev/null || echo none)
      echo "!! GraphQL does not serve block $h within ${WAIT_SECS}s; its latest block is $last" >&2
      echo "   (a latest block above $h means the block manager attached after the network moved on)" >&2
      exit 1
    fi
    sleep 5
  done
  echo "GraphQL serves block $h"
done
