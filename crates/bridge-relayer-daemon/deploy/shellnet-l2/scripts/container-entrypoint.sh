#!/usr/bin/env bash
set -Eeuo pipefail
set +x
umask 077

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
env_file=${RELAYER_RUNTIME_ENV_FILE:-/run/secrets/runtime-env}
action=${1:-run}

[[ -r "$env_file" ]] || die "runtime environment is not mounted/readable: $env_file"
case "$action" in
  preflight|run) ;;
  *) die "usage: $0 [preflight|run]" ;;
esac

# Both commands are intentionally repeated on every real container start. A
# direct `docker restart` therefore cannot bypass artifact, chain, key or
# cursor validation. Neither check sends a transaction.
RELAYER_RUNTIME_LAYOUT=compose "$script_dir/verify-params.sh" "$env_file"
RELAYER_RUNTIME_LAYOUT=compose "$script_dir/preflight.sh" "$env_file"

if [[ "$action" == preflight ]]; then
  printf 'CONTAINER PREFLIGHT PASSED. No transaction was sent and no daemon was started.\n'
  exit 0
fi

set -a
# The file is root/operator-generated in the same simple KEY=value format as
# the reviewed template. Do not enable xtrace around this source operation.
# shellcheck disable=SC1090
source "$env_file"
set +a
# shellcheck disable=SC1091
source "$script_dir/container-paths.sh"
remap_compose_runtime_paths

[[ "${BRIDGE_ANCHOR_LEVEL:-}" == 2 ]] || die "only L2 anchor level 2 is allowed"
[[ "${EXPECTED_EVM_CHAIN_ID:-}" =~ ^[0-9]+$ ]] || die "EXPECTED_EVM_CHAIN_ID must be the decimal chain id the bridge is deployed on"

printf 'Starting shellnet -> EVM chain %s L2 relayer: source=%s bridge=%s EOA=%s seed=%s\n' \
  "$EXPECTED_EVM_CHAIN_ID" "$EXPECTED_BRIDGE_COMMIT" "$BRIDGE_ADDRESS" \
  "$RELAYER_ADDRESS" "$BRIDGE_BOOTSTRAP_SEQNO"
cd /opt/gosh-relayer/prover

# Run as a child, not exec'd: its exit status decides what Docker sees. 78
# means the Acki Nacki chain is behind the bridge (a restarted network, or a
# GraphQL endpoint serving another one). No restart can help, and under
# `restart: unless-stopped` the container would rerun the preflight and the
# startup scan and stop again every few minutes. It stays up instead, with
# the daemon stopped, until an operator stops or recreates it.
"$RELAYER_BINARY" --state "$RELAYER_STATE_PATH" daemon-live &
daemon=$!
trap 'kill -TERM "$daemon" 2>/dev/null || true' TERM INT
status=0
wait "$daemon" || status=$?
# A signal handled by the trap ends `wait` early; wait for the daemon itself.
while kill -0 "$daemon" 2>/dev/null; do
  status=0
  wait "$daemon" || status=$?
done
trap - TERM INT

if (( status == 78 )); then
  printf 'STOPPED: the Acki Nacki chain is behind the bridge (error above); restarting cannot help. The container stays up with the daemon stopped. Deploy a new bridge (live relayer runbook, section 5.5), then recreate the container.\n' >&2
  trap 'exit 0' TERM INT
  while :; do
    sleep 3600 &
    wait $! || true
  done
fi
exit "$status"
