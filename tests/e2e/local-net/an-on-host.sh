#!/usr/bin/env bash
# Runs crates/bridge-prover-libraries/python/arbitrum_e2e_an.py <args> ON the local-network
# host, in the checkout run-test-on-host.sh left at BRIDGE_SHA (the run-test step runs first).
# Piped over ssh (`bash -s -- <args>`); prints only the script's KEY=VALUE answer on stdout.
set -euo pipefail
BRIDGE_SHA=${BRIDGE_SHA:?BRIDGE_SHA is required}
ACKI_DIR=${ACKI_DIR:?ACKI_DIR is required}
WORK_ROOT=${WORK_ROOT:-$HOME/bridge-e2e}
DIR=$WORK_ROOT/bridge
[ "$(git -C "$DIR" rev-parse HEAD)" = "$BRIDGE_SHA" ] || { echo "FATAL: $DIR is not at $BRIDGE_SHA" >&2; exit 66; }
export PATH="$WORK_ROOT/bin:$PATH" MODE=local NETWORK=http://127.0.0.1:80 GRAPHQL_URL=http://127.0.0.1/graphql
export ACKI_NACKI_ROOT="$ACKI_DIR" PROVER_DIR="$DIR/crates/bridge-prover-libraries" WORK_DIR="$WORK_ROOT/arbitrum-work"
mkdir -p "$WORK_DIR"
cd "$PROVER_DIR/python" && exec python3 arbitrum_e2e_an.py "$@" </dev/null
