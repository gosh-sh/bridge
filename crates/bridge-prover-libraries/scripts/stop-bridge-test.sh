#!/usr/bin/env bash
#
# Graceful-shutdown companion to run-bridge-test.sh.
#
# Sends SIGINT to prover first, then verifier — stopping the prover first
# means it won't write a half-finished proof file the verifier would race on.
# Waits up to 30s for clean exit (daemons print a SUMMARY block on SIGINT),
# then SIGKILLs anything still alive.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

PID_FILE="logs/test.pids"
if [[ ! -f "$PID_FILE" ]]; then
    echo "no $PID_FILE — nothing to stop"
    exit 0
fi

PROVER_PID=$(grep '^prover=' "$PID_FILE" | cut -d= -f2 || true)
VERIFIER_PID=$(grep '^verifier=' "$PID_FILE" | cut -d= -f2 || true)

send_int() {
    local pid="$1" name="$2"
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        echo "==> SIGINT $name ($pid)"
        kill -INT "$pid"
    fi
}

# Order matters: prover first, then verifier.
send_int "$PROVER_PID" "bridge-prover-daemon"
send_int "$VERIFIER_PID" "bridge-verifier-daemon"

# Wait up to 30s for both to exit.
for _ in $(seq 1 30); do
    alive=0
    for pid in "$PROVER_PID" "$VERIFIER_PID"; do
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            alive=1
        fi
    done
    [[ $alive -eq 0 ]] && break
    sleep 1
done

for pid in "$PROVER_PID" "$VERIFIER_PID"; do
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        echo "==> $pid still alive after 30s — SIGKILL"
        kill -KILL "$pid" 2>/dev/null || true
    fi
done

rm -f "$PID_FILE"
echo "==> stopped"
