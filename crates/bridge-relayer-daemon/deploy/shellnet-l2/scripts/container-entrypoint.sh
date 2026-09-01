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
[[ "${EXPECTED_EVM_CHAIN_ID:-}" == 11155111 ]] || die "only Sepolia is allowed"

printf 'Starting shellnet -> Sepolia L2 relayer: source=%s bridge=%s EOA=%s seed=%s\n' \
  "$EXPECTED_BRIDGE_COMMIT" "$BRIDGE_ADDRESS" \
  "$RELAYER_ADDRESS" "$BRIDGE_BOOTSTRAP_SEQNO"
cd /opt/gosh-relayer/prover
exec "$RELAYER_BINARY" --state "$RELAYER_STATE_PATH" daemon-live
