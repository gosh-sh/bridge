#!/usr/bin/env bash
set -Eeuo pipefail
set +x

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
kit_dir=${1:-$(cd -- "$script_dir/.." && pwd)}
env_file=${2:-/etc/gosh-bridge-relayer/shellnet-l2.env}
[[ -f "$kit_dir/compose.yaml" && -r "$env_file" ]] ||
  die "usage: $0 [KIT_DIR] [RUNTIME_ENV]"

# shellcheck disable=SC1090
source "$env_file"
unset RELAYER_PRIVATE_KEY
if [[ -f "$kit_dir/.env" ]]; then
  # Non-secret Compose paths and image settings.
  # shellcheck disable=SC1090
  source "$kit_dir/.env"
fi
runtime_root=${RELAYER_RUNTIME_ROOT:-/var/lib/gosh-bridge-relayer}

for command_name in cast curl docker jq; do
  command -v "$command_name" >/dev/null || die "$command_name is not installed"
done

cd "$kit_dir"
printf '=== Compose ===\n'
docker compose ps --all
container_id=$(docker compose ps -q relayer)
if [[ -n "$container_id" ]]; then
  docker inspect "$container_id" --format \
    'container={{.Name}} status={{.State.Status}} exit={{.State.ExitCode}} oom={{.State.OOMKilled}} restarts={{.RestartCount}} image={{.Image}} started={{.State.StartedAt}}'
  docker top "$container_id" -eo pid,ppid,user,etime,pcpu,pmem,args
else
  printf 'No relayer container exists.\n'
fi

printf '\n=== Sepolia ===\n'
chain_id=$(ETH_RPC_URL="$RPC_URL" cast chain-id)
cursor=$(ETH_RPC_URL="$RPC_URL" cast call "$BRIDGE_ADDRESS" \
  'storedLastSeenBlockSeqNo()(uint64)' | awk '{print $1}')
nonce_latest=$(ETH_RPC_URL="$RPC_URL" cast nonce "$RELAYER_ADDRESS" --block latest)
nonce_pending=$(ETH_RPC_URL="$RPC_URL" cast nonce "$RELAYER_ADDRESS" --block pending)
balance=$(ETH_RPC_URL="$RPC_URL" cast balance "$RELAYER_ADDRESS")
printf 'chain_id=%s bridge=%s cursor=%s EOA=%s nonce=%s/%s balance_wei=%s\n' \
  "$chain_id" "$BRIDGE_ADDRESS" "$cursor" "$RELAYER_ADDRESS" \
  "$nonce_latest" "$nonce_pending" "$balance"

printf '\n=== Shellnet ===\n'
payload='{"query":"{ blockchain { blocks(last: 1) { edges { node { seq_no thread_id } } } } }"}'
response=$(curl --fail-with-body --silent --show-error --max-time 20 \
  -H 'content-type: application/json' --data-binary "$payload" "$BRIDGE_GQL_ENDPOINT")
head=$(jq -er '.data.blockchain.blocks.edges[-1].node.seq_no' <<<"$response")
thread=$(jq -er '.data.blockchain.blocks.edges[-1].node.thread_id' <<<"$response")
next=$((cursor + 16384))
remaining=$((next - head))
if (( remaining < 0 )); then
  remaining=0
fi
printf 'head=%s thread=%s next_l2_boundary=%s blocks_remaining=%s\n' \
  "$head" "$thread" "$next" "$remaining"

printf '\n=== Persistent state ===\n'
relayer_state=$runtime_root/L2_config/relayer-state.json
prover_state=$runtime_root/L2_config/state/prover_state.json
if [[ -s "$relayer_state" ]]; then
  printf '%s\n' "$relayer_state"
  jq '{
    last_processed_seqno,
    last_attempt_seqno,
    attempts_since_progress,
    last_bk_update_processed_seqno,
    bk_update_attempts_since_progress,
    last_observed_on_chain
  }' "$relayer_state"
else
  printf '%s: not created yet\n' "$relayer_state"
fi
if [[ -s "$prover_state" ]]; then
  printf '%s\n' "$prover_state"
  jq '{
    window_size,
    initialized,
    anchor_level,
    stored_last_seen_block_seq_no,
    stored_last_seen_block_height,
    stored_last_bk_set_update_seq_no,
    populated_layers: [
      .layer_windows
      | to_entries[]
      | select(.value.data_len > 0)
      | {
          layer: (.key + 1),
          data_len: .value.data_len,
          write_cursor: .value.write_cursor,
          last_height: .value.last_height
        }
    ]
  }' "$prover_state"
else
  printf '%s: not created yet\n' "$prover_state"
fi
du -sh \
  "$runtime_root/params/pk_cache" \
  "$runtime_root/submissions" \
  "$runtime_root/tmp"

printf '\n=== Recent significant logs ===\n'
docker compose logs --no-color --since 6h relayer 2>&1 |
  grep -E 'Starting shellnet|startup:|bootstrap seed applied|layers=|aggregate-proof|verified|verifyBlock|transaction|hard aborting|bridge reverted|tick failed|ERROR' |
  sed -E $'s/\033\\[[0-9;]*[mK]//g' |
  tail -80 || true
