#!/usr/bin/env bash
# What the jobs see of the local network through the proxies: GraphQL behind nginx0 and the
# REST port of every node. Writes probe.env: GRAPHQL_OK, NODE0_8600 ... NODE4_8600.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
answer=$(curl -s -m 20 -H 'content-type: application/json' -d '{"query":"{ info { version } }"}' "$GQL" || true)
log "GraphQL: $answer"
case "$answer" in *'"data"'*) emit probe.env GRAPHQL_OK=true ;; *) emit probe.env GRAPHQL_OK=false ;; esac
for n in 0 1 2 3 4; do
  if timeout 5 bash -c "exec 3<>/dev/tcp/node$n/8600" 2>/dev/null; then emit probe.env "NODE${n}_8600=open"; else emit probe.env "NODE${n}_8600=closed"; fi
done
cat "$OUT/probe.env"
