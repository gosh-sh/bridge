#!/usr/bin/env bash
# ETH-16 — node_exporter textfile collector over deposit-relayer state.json.
# No daemon changes. Point node_exporter --collector.textfile.directory at the
# output dir and alert on parked_deposits > 0, climbing attempts_since_progress,
# and a stale mtime (daemon hung or dead).
#
#   scripts/deposit_relayer_textfile_metrics.sh /path/to/state.json \
#     > /var/lib/node_exporter/textfile/bridge_deposit_relayer.prom
set -euo pipefail

STATE="${1:?usage: $0 <state.json>}"

if [[ ! -f "$STATE" ]]; then
  cat <<EOF
# HELP bridge_deposit_relayer_parked_deposits Count of deposit ids parked after skip-after-attempts.
# TYPE bridge_deposit_relayer_parked_deposits gauge
bridge_deposit_relayer_parked_deposits 0
# HELP bridge_deposit_relayer_attempts_since_progress Consecutive non-success ticks on the current id.
# TYPE bridge_deposit_relayer_attempts_since_progress gauge
bridge_deposit_relayer_attempts_since_progress 0
# HELP bridge_deposit_relayer_state_mtime_seconds Unix mtime of state.json (0 if missing).
# TYPE bridge_deposit_relayer_state_mtime_seconds gauge
bridge_deposit_relayer_state_mtime_seconds 0
EOF
  exit 0
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "jq required" >&2
  exit 1
fi

if stat --version >/dev/null 2>&1; then
  mtime=$(stat -c %Y "$STATE")
else
  mtime=$(stat -f %m "$STATE")
fi

parked=$(jq -r '(.parked_deposit_ids // []) | length' "$STATE")
attempts=$(jq -r '.attempts_since_progress // 0' "$STATE")

cat <<EOF
# HELP bridge_deposit_relayer_parked_deposits Count of deposit ids parked after skip-after-attempts.
# TYPE bridge_deposit_relayer_parked_deposits gauge
bridge_deposit_relayer_parked_deposits ${parked}
# HELP bridge_deposit_relayer_attempts_since_progress Consecutive non-success ticks on the current id.
# TYPE bridge_deposit_relayer_attempts_since_progress gauge
bridge_deposit_relayer_attempts_since_progress ${attempts}
# HELP bridge_deposit_relayer_state_mtime_seconds Unix mtime of state.json (0 if missing).
# TYPE bridge_deposit_relayer_state_mtime_seconds gauge
bridge_deposit_relayer_state_mtime_seconds ${mtime}
EOF
