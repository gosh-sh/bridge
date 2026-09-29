#!/usr/bin/env bash
# run_multipath.sh — end-to-end multi-path reachability session.
#
# What it does (one command, blocking, ~35–50 min wall time):
#
#   1. Validates the tool binaries in $MT_DIR/bins_<OS>/ (auto-detected
#      from `uname -s`; overridden by $TOOLS_DIR) and exports the
#      CLI_NAME / TVM_CLI / SOLD / TVM_DEBUGGER / ZEROSTATE_HELPER /
#      NODE_HELPER env vars that tests/mt/cli.py needs.
#   2. If GQL at http://localhost/graphql answers → skip node bringup.
#      Else brings up the acki-nacki devnet via `make generate_zerostate`
#      (only if `docker/.env` is missing) + `make run`.
#   3. Starts the thread-liveness monitor in the background, writing
#      per-sample JSONL to $STATS_DIR/thread-mon-$TS.jsonl.
#   4. Starts `cli.py test-multithread-cross-thread` in the background
#      (Michael's tested params, `--threads 2` reference recipe).
#   5. Waits up to 5 min for the monitor's latest sample to report
#      ≥ 2 threads LIVE — the split-established signal.
#   6. Starts the multi-path collector in the background.
#   7. Runs smart_trigger.py in the foreground.
#   8. A watchdog polls the monitor every 15 s throughout smart_trigger.
#      If any child thread drops to STALLED/IDLE after a 90 s grace,
#      it kills smart_trigger and the session aborts.
#   9. After smart_trigger exits (natural or aborted), drains the
#      collector for one observation window, then runs the analyzer
#      and prints where every JSONL / log landed.
#
# Prereqs on a fresh box: see multithreading/bins_macOS/README.md
# (macOS) or bins_linux/README.md (Linux).

set -euo pipefail

# ----------------------------------------------------------------------
# Paths & env

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ACKI_NACKI_DIR="${ACKI_NACKI_DIR:-$(cd "$MT_DIR/../../acki-nacki" 2>/dev/null && pwd || echo)}"

# Default tools dir is bins_<OS>/ next to runbooks/. Auto-detected from
# `uname` so the same script works on macOS and Linux without arguments;
# TOOLS_DIR overrides both.
case "$(uname -s)" in
  Darwin) _DEFAULT_TOOLS="$MT_DIR/bins_macOS" ;;
  Linux)  _DEFAULT_TOOLS="$MT_DIR/bins_linux" ;;
  *)      _DEFAULT_TOOLS="$MT_DIR/bins_$(uname -s)" ;;
esac
TOOLS_DIR="${TOOLS_DIR:-$_DEFAULT_TOOLS}"
unset _DEFAULT_TOOLS

STATS_DIR="$MT_DIR/research/stats"

TS="$(date +%Y%m%d-%H%M%S)"
LOG="$STATS_DIR/session-$TS.log"
MON_OUT="$STATS_DIR/thread-mon-$TS.jsonl"
MON_LOG="$STATS_DIR/monitor-$TS.log"
CLI_LOG="$STATS_DIR/cli-$TS.log"
COL_OUT="$STATS_DIR/dirb-mp-$TS.jsonl"
COL_LOG="$STATS_DIR/collector-$TS.log"
EVT_OUT="$STATS_DIR/events-$TS.jsonl"
TRG_LOG="$STATS_DIR/trigger-$TS.log"
ABORT_FLAG="$STATS_DIR/.abort-$TS"

MON_PID=""; CLI_PID=""; COL_PID=""; TRG_PID=""; WDG_PID=""

mkdir -p "$STATS_DIR"

log() { echo "[$(date +%H:%M:%S)] $*" | tee -a "$LOG"; }

# ----------------------------------------------------------------------
# Trap / teardown

teardown() {
  # Foreground processes get a chance to flush; monitor/cli.py we hard-stop.
  for name_pid in "trigger:$TRG_PID" "collector:$COL_PID" "watchdog:$WDG_PID"; do
    name="${name_pid%%:*}"; pid="${name_pid##*:}"
    [ -n "$pid" ] && kill -TERM "$pid" 2>/dev/null && log "sent TERM to $name (pid $pid)" || true
  done
  # Give them ~5 s to exit (collector emits pending JSONL on SIGTERM).
  sleep 5
  for pid in "$TRG_PID" "$COL_PID" "$WDG_PID" "$CLI_PID" "$MON_PID"; do
    [ -n "$pid" ] && kill -KILL "$pid" 2>/dev/null || true
  done
  rm -f "$ABORT_FLAG"
}
trap teardown EXIT INT TERM

# ----------------------------------------------------------------------
# Deps

need() {
  command -v "$1" >/dev/null 2>&1 || { echo "MISSING: $1" >&2; exit 1; }
}

check_deps() {
  need python3
  need docker
  need jq
  need curl
  need make
  [ -n "$ACKI_NACKI_DIR" ] && [ -d "$ACKI_NACKI_DIR" ] \
    || { echo "ACKI_NACKI_DIR not set and $MT_DIR/../../../acki-nacki not found." >&2; exit 1; }
  for tool in tvm-cli sold tvm-debugger zerostate-helper node-helper; do
    if [ ! -x "$TOOLS_DIR/$tool" ]; then
      echo "MISSING or non-executable: $TOOLS_DIR/$tool" >&2
      echo "See $TOOLS_DIR/README.md (or $MT_DIR/bins_macOS/README.md for the macOS recipe) for how to populate." >&2
      exit 1
    fi
  done
}

export_tool_env() {
  export CLI_NAME="$TOOLS_DIR/tvm-cli"
  export TVM_CLI="$TOOLS_DIR/tvm-cli"
  export SOLD="$TOOLS_DIR/sold"
  export TVM_DEBUGGER="$TOOLS_DIR/tvm-debugger"
  export ZEROSTATE_HELPER="$TOOLS_DIR/zerostate-helper"
  export NODE_HELPER="$TOOLS_DIR/node-helper"
  export DISABLE_MV=true
  export MESSAGE_ARCHIVE_OTEL_RUN_ID="mt-$TS"
}

# ----------------------------------------------------------------------
# Node health & bringup

gql_healthy() {
  curl -sS -m 5 http://localhost/graphql \
    -H 'Content-Type: application/json' \
    -d '{"query":"{ blockchain { blocks(last:1) { edges { node { seq_no } } } } }"}' \
    2>/dev/null \
    | jq -e '.data.blockchain.blocks.edges[0].node.seq_no' >/dev/null 2>&1
}

ensure_node_up() {
  if gql_healthy; then
    log "acki-nacki GQL healthy — skipping node bringup"
    return 0
  fi
  log "acki-nacki GQL not responding — bringing node up in $ACKI_NACKI_DIR"
  (
    cd "$ACKI_NACKI_DIR"
    if [ ! -f docker/.env ]; then
      log "docker/.env missing — running make generate_zerostate"
      make generate_zerostate
    fi
    make run
  )
  log "waiting up to 5 min for GQL to answer ..."
  for i in $(seq 1 60); do
    gql_healthy && { log "GQL up after ${i}×5s"; return 0; }
    sleep 5
  done
  log "GQL never came up — abort"
  exit 2
}

# ----------------------------------------------------------------------
# Container name for the liveness monitor

detect_node_container() {
  # Prefer explicit env; else pick the first container whose name matches *node0*.
  if [ -n "${NODE_CONTAINER:-}" ]; then
    echo "$NODE_CONTAINER"; return 0
  fi
  local c
  c="$(docker ps --format '{{.Names}}' | grep -E '(^|[-_])node0([-_]|$)' | head -n1 || true)"
  [ -n "$c" ] || { echo ""; return 1; }
  echo "$c"
}

# ----------------------------------------------------------------------
# Split-established check via monitor's JSONL

monitor_live_count() {
  # Latest full JSONL line → count of threads with status=LIVE.
  local line
  line="$(tail -n 1 "$MON_OUT" 2>/dev/null || true)"
  [ -z "$line" ] && { echo 0; return; }
  echo "$line" | jq -r '[.threads[] | select(.status=="LIVE")] | length' 2>/dev/null || echo 0
}

monitor_dead_count() {
  # STALLED or IDLE = not producing candidates in current window.
  local line
  line="$(tail -n 1 "$MON_OUT" 2>/dev/null || true)"
  [ -z "$line" ] && { echo 0; return; }
  echo "$line" | jq -r '[.threads[] | select(.status=="STALLED" or .status=="IDLE")] | length' 2>/dev/null || echo 0
}

# ----------------------------------------------------------------------
# Main

main() {
  check_deps
  export_tool_env

  log "session $TS"
  log "  MT_DIR         = $MT_DIR"
  log "  ACKI_NACKI_DIR = $ACKI_NACKI_DIR"
  log "  TOOLS_DIR      = $TOOLS_DIR"
  log "  STATS_DIR      = $STATS_DIR"

  ensure_node_up

  local container
  container="$(detect_node_container)" \
    || { log "no node* container found — is the compose project up?"; exit 3; }
  log "monitoring docker container: $container"

  # ------- (E) liveness monitor -------
  ( python3 "$MT_DIR/research/thread_liveness_monitor.py" \
      --container "$container" --interval 5 --out "$MON_OUT" \
      > "$MON_LOG" 2>&1 ) &
  MON_PID=$!
  log "monitor started (pid $MON_PID) -> $MON_OUT"

  # ------- (B) cli.py test-multithread-cross-thread -------
  # Params are Michael's reference recipe. 30-min hold (--hold-seconds 1800)
  # gives smart_trigger enough headroom for ~12 events with mixed same/cross
  # thread pacing. --timeout 2400 = 40 min overall (10 min buffer for fan
  # + drain phases).
  ( cd "$ACKI_NACKI_DIR" && python3 tests/mt/cli.py test-multithread-cross-thread \
      --threads 2 \
      --total 20000 \
      --hold-burst-total 5000 \
      --hold-quiet-seconds 0 \
      --batch-size 200 \
      --deploy-value 12000000000000 \
      --minimum-balance 8000000000000 \
      --hold-seconds 1800 \
      --timeout 2400 \
      > "$CLI_LOG" 2>&1 ) &
  CLI_PID=$!
  log "cli.py started (pid $CLI_PID) -> $CLI_LOG"

  # ------- wait for split -------
  log "waiting up to 5 min for split (≥ 2 threads LIVE) ..."
  local deadline=$((SECONDS + 300))
  local n_live=0
  while [ $SECONDS -lt $deadline ]; do
    n_live=$(monitor_live_count)
    [ "$n_live" -ge 2 ] && break
    if ! kill -0 $CLI_PID 2>/dev/null; then
      log "cli.py exited before split established — check $CLI_LOG"; exit 4
    fi
    sleep 5
  done
  if [ "$n_live" -lt 2 ]; then
    log "split never reached ≥ 2 LIVE threads — abort. tail of $CLI_LOG:"
    tail -n 30 "$CLI_LOG" | sed 's/^/  /' | tee -a "$LOG"
    exit 5
  fi
  log "split established ($n_live LIVE threads)"

  # ------- (D) collector -------
  ( cd "$MT_DIR" && python3 research/multipath_collector.py \
      --graphql http://localhost/graphql \
      --out "$COL_OUT" \
      --poll-interval 2.0 \
      --scan-window 50 \
      --fetch-budget-per-poll 200 \
      --observation-window-s 300 \
      --max-wait-anchor-s 600 \
      --max-hop-budget 60 \
      --max-paths-per-event 20 \
      --heartbeat-s 30 \
      > "$COL_LOG" 2>&1 ) &
  COL_PID=$!
  log "collector started (pid $COL_PID) -> $COL_OUT"

  # Let the collector prime the DAG before firing events.
  sleep 10

  # ------- watchdog (background) -------
  # 90 s grace after start-of-run, then check every 15 s. On wedge:
  # write ABORT_FLAG and send SIGTERM to trigger.
  ( sleep 90
    while [ ! -f "$ABORT_FLAG" ]; do
      dead=$(monitor_dead_count)
      if [ "$dead" -gt 0 ]; then
        echo "watchdog: $dead thread(s) STALLED/IDLE" >&2
        touch "$ABORT_FLAG"
        kill -TERM "$TRG_PID" 2>/dev/null || true
        exit 0
      fi
      sleep 15
    done
  ) > "$STATS_DIR/watchdog-$TS.log" 2>&1 &
  WDG_PID=$!
  log "watchdog started (pid $WDG_PID)"

  # ------- (C) smart_trigger (foreground) -------
  # --count 12: on 2-thread runs with USDCBridge under DEFAULT_DAPP_ID,
  # ~100 % of events land on thread 0 → ~60 s/event → ~12 min. If a mixed
  # or all-cross-thread run materialises (4-thread or dapp redeploy),
  # each cross-thread event costs the full observation window (~345 s),
  # so 12 × 345 s = 69 min > 30 min hold. In that case cut --count to 5.
  log "firing 12 events via smart_trigger ..."
  ( cd "$MT_DIR" && python3 research/smart_trigger.py \
      --count 12 \
      --graphql http://localhost/graphql \
      --observation-window-s 300 \
      --same-thread-pause-s 15 \
      --collector-out "$COL_OUT" \
      --events-out "$EVT_OUT" \
      > "$TRG_LOG" 2>&1 ) &
  TRG_PID=$!
  set +e
  wait "$TRG_PID"; local trg_rc=$?
  set -e

  if [ -f "$ABORT_FLAG" ]; then
    log "ABORTED by watchdog — a thread stalled mid-run"
    log "  watchdog log: $STATS_DIR/watchdog-$TS.log"
    log "  monitor tail:"
    tail -n 5 "$MON_LOG" 2>/dev/null | sed 's/^/    /' | tee -a "$LOG"
  elif [ "$trg_rc" -ne 0 ]; then
    log "smart_trigger exited rc=$trg_rc; continuing to drain collector"
  else
    log "smart_trigger completed rc=0"
  fi

  # ------- drain collector -------
  log "draining collector for one observation window (300 s) ..."
  sleep 300
  kill -TERM "$COL_PID" 2>/dev/null || true
  wait "$COL_PID" 2>/dev/null || true

  # ------- analyzer -------
  log "running multipath_analyzer ..."
  ( cd "$MT_DIR" && python3 research/multipath_analyzer.py "$COL_OUT" ) \
    2>&1 | tee -a "$LOG"
  ( cd "$MT_DIR" && python3 research/multipath_analyzer.py "$COL_OUT" --json ) \
    > "$STATS_DIR/summary-$TS.json" 2>/dev/null || true

  log ""
  log "session complete. outputs:"
  log "  session log:   $LOG"
  log "  monitor JSONL: $MON_OUT"
  log "  cli.py log:    $CLI_LOG"
  log "  collector:     $COL_OUT"
  log "  events:        $EVT_OUT"
  log "  summary JSON:  $STATS_DIR/summary-$TS.json"
}

main "$@"
