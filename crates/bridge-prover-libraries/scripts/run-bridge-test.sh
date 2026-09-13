#!/usr/bin/env bash
#
# Fresh-start launcher for the prover↔verifier pair.
#
# Wipes proofs/, state/, logs/ (NOT params/ — those VKs/PKs are expensive to
# regenerate), then launches both daemons in the background writing to
# logs/{prover,verifier}_output.log. Prints PIDs and how to stop.
#
# Why this isn't baked into the daemons themselves: restart-resume testing
# requires a daemon restart WITHOUT wiping state. The launcher is a separate
# concern from daemon startup logic. For restart-resume testing, launch the
# daemons manually with `cargo run --release --bin bridge-prover-daemon` /
# `--bin bridge-verifier-daemon`.
#
# Usage:
#   scripts/run-bridge-test.sh         # fresh start, default RUST_LOG=info
#   RUST_LOG=debug scripts/run-bridge-test.sh
#   scripts/run-bridge-test.sh --debug # debug build instead of release
#
# To stop cleanly:
#   scripts/stop-bridge-test.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

PROFILE="release"
PROFILE_DIR="release"
CARGO_FLAGS=("--release")
for arg in "$@"; do
    case "$arg" in
        --debug)
            PROFILE="dev"
            PROFILE_DIR="debug"
            CARGO_FLAGS=()
            ;;
        *)
            echo "unknown flag: $arg" >&2
            exit 1
            ;;
    esac
done

echo "==> wiping proofs/ state/ logs/ (keeping params/)"
rm -f proofs/*.json state/*.json logs/*.log logs/test.pids
mkdir -p proofs state logs

echo "==> building daemons ($PROFILE profile)"
cargo build "${CARGO_FLAGS[@]}" --bin bridge-prover-daemon --bin bridge-verifier-daemon

PID_FILE="logs/test.pids"
: > "$PID_FILE"

# Launch verifier first — it watches proofs/ and we want it ready before the
# prover writes the bootstrap seed.
echo "==> launching bridge-verifier-daemon"
RUST_LOG="${RUST_LOG:-info}" \
    "./target/$PROFILE_DIR/bridge-verifier-daemon" \
    > logs/verifier_output.log 2>&1 &
VERIFIER_PID=$!
echo "verifier=$VERIFIER_PID" >> "$PID_FILE"

# Tiny stagger so the verifier prints its banner first; not load-bearing.
sleep 1

echo "==> launching bridge-prover-daemon"
RUST_LOG="${RUST_LOG:-info}" \
    "./target/$PROFILE_DIR/bridge-prover-daemon" \
    > logs/prover_output.log 2>&1 &
PROVER_PID=$!
echo "prover=$PROVER_PID" >> "$PID_FILE"

echo
echo "==> running (verifier PID=$VERIFIER_PID, prover PID=$PROVER_PID)"
echo
echo "    tail logs:    tail -f logs/verifier_output.log logs/prover_output.log"
echo "    stop cleanly: scripts/stop-bridge-test.sh"
