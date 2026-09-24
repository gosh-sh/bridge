#!/usr/bin/env bash
set -Eeuo pipefail
set +x

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

ok() {
  printf 'OK: %s\n' "$*"
}

lower() {
  tr '[:upper:]' '[:lower:]'
}

call_word() {
  cast call "$1" "$2" "${@:3}" --rpc-url "$RPC_URL" | awk '{print $1}'
}

require_code() {
  local address=$1 label=$2 code
  code=$(cast code "$address" --rpc-url "$RPC_URL")
  [[ "$code" != 0x && ${#code} -gt 100 ]] || die "$label has no runtime code ($address)"
}

verify_verifier_lane() {
  local bridge_getter=$1 bin_name=$2 label=$3
  local adapter wrapper yul on_chain expected

  adapter=$(call_word "$BRIDGE_ADDRESS" "$bridge_getter")
  [[ "$adapter" =~ ^0x[0-9a-fA-F]{40}$ && "${adapter,,}" != 0x0000000000000000000000000000000000000000 ]] ||
    die "$label adapter address is invalid"
  require_code "$adapter" "$label adapter"

  wrapper=$(call_word "$adapter" 'shplonkVerifier()(address)')
  [[ "$wrapper" =~ ^0x[0-9a-fA-F]{40}$ && "${wrapper,,}" != 0x0000000000000000000000000000000000000000 ]] ||
    die "$label SHPLONK wrapper address is invalid"
  require_code "$wrapper" "$label SHPLONK wrapper"

  yul=$(call_word "$wrapper" 'yulVerifier()(address)')
  [[ "$yul" =~ ^0x[0-9a-fA-F]{40}$ && "${yul,,}" != 0x0000000000000000000000000000000000000000 ]] ||
    die "$label Yul verifier address is invalid"
  require_code "$yul" "$label Yul verifier"

  [[ -s "$BRIDGE_VERIFIERS_DIR/$bin_name" ]] || die "missing verifier bin $bin_name"
  # aggregate-proof self-checks every proof against the source, not the bin.
  [[ -s "$BRIDGE_VERIFIERS_DIR/${bin_name%.bin}.sol" ]] ||
    die "missing verifier source ${bin_name%.bin}.sol"
  on_chain=$(cast code "$yul" --rpc-url "$RPC_URL" | lower)
  # gen_evm_verifier_shplonk emits a 32-byte CREATE prelude followed by the
  # runtime payload. ShplonkDeployLib deploys the full bin; eth_getCode returns
  # only the payload, hence tail from byte 33.
  expected=0x$(tail -c +33 "$BRIDGE_VERIFIERS_DIR/$bin_name" | xxd -p | tr -d '\n' | lower)
  [[ "$on_chain" == "$expected" ]] || die "$label deployed Yul runtime differs from $bin_name"
  ok "$label verifier stack adapter=$adapter wrapper=$wrapper yul=$yul"
}

env_file=${1:-}
[[ -n "$env_file" && -f "$env_file" ]] ||
  die "usage: $0 /path/to/shellnet-l2.env"

set -a
# shellcheck disable=SC1090
source "$env_file"
set +a

runtime_layout=${RELAYER_RUNTIME_LAYOUT:-host}
case "$runtime_layout" in
  host) ;;
  compose)
    script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
    # shellcheck disable=SC1091
    source "$script_dir/container-paths.sh"
    remap_compose_runtime_paths
    ;;
  *) die "unknown RELAYER_RUNTIME_LAYOUT: $runtime_layout" ;;
esac

required=(
  BRIDGE_REPO_DIR EXPECTED_BRIDGE_COMMIT
  RELAYER_BINARY EXPECTED_RELAYER_SHA256
  RELAYER_STATE_PATH RPC_URL BRIDGE_ADDRESS RELAYER_ADDRESS RELAYER_PRIVATE_KEY
  MIN_RELAYER_BALANCE_WEI BRIDGE_GQL_ENDPOINT BRIDGE_PARAMS_DIR
  BRIDGE_BOOTSTRAP_SEQNO BRIDGE_CONFIG_DIR BRIDGE_STATE_DIR BRIDGE_BK_SET_CONFIG
  BRIDGE_BK_SET_EXPECTED_SHA256 EXPECTED_BK_SET_COMMITMENT
  EXPECTED_GENESIS_PREV_ANCHOR EXPECTED_WITHDRAW_DAPP_FR
  EXPECTED_WITHDRAW_ACC_FR EXPECTED_WITHDRAW_ALT_DST_CHAIN_ID
  EXPECTED_WITHDRAW_ALT_DST_HOST_CHAIN_ID EXPECTED_WITHDRAW_ALT_TOKEN_ID
  EXPECTED_USDC_ADDRESS
  BRIDGE_AGGREGATOR_DIR EXPECTED_AGGREGATE_PROOF_SHA256 BRIDGE_VERIFIERS_DIR
  BRIDGE_PK_CACHE_DIR BRIDGE_DUMP_SUBMISSIONS_DIR BRIDGE_ANCHOR_LEVEL
  EXPECTED_EVM_CHAIN_ID
)
for name in "${required[@]}"; do
  value=${!name-}
  [[ -n "$value" && "$value" != *CHANGEME* ]] || die "$name is missing"
done

required_commands=(cast curl jq od sha256sum xxd)
[[ "$runtime_layout" == host ]] && required_commands+=(git)
for command_name in "${required_commands[@]}"; do
  command -v "$command_name" >/dev/null || die "$command_name is not installed"
done

[[ "$BRIDGE_ANCHOR_LEVEL" == 2 ]] || die "only L2 anchor level 2 is allowed"
[[ "$EXPECTED_EVM_CHAIN_ID" == 11155111 ]] || die "expected Sepolia chain id"
[[ "$BRIDGE_ADDRESS" =~ ^0x[0-9a-fA-F]{40}$ ]] || die "bad bridge address"
[[ "$RELAYER_ADDRESS" =~ ^0x[0-9a-fA-F]{40}$ ]] || die "bad relayer address"
[[ "$RELAYER_PRIVATE_KEY" =~ ^(0x)?[0-9a-fA-F]{64}$ ]] || die "bad private key shape"
[[ "$BRIDGE_BOOTSTRAP_SEQNO" =~ ^[0-9]+$ ]] || die "bad bootstrap seqno"
(( BRIDGE_BOOTSTRAP_SEQNO % 16384 == 0 )) || die "bootstrap is not L2-aligned"
[[ "$MIN_RELAYER_BALANCE_WEI" =~ ^[0-9]+$ ]] || die "bad minimum balance"
[[ "$EXPECTED_GENESIS_PREV_ANCHOR" =~ ^0x[0-9a-fA-F]{64}$ ]] || die "bad genesis anchor"
[[ "$EXPECTED_WITHDRAW_DAPP_FR" =~ ^(0x[0-9a-fA-F]{1,64}|[0-9]+)$ ]] || die "bad withdraw dApp Fr"
[[ "$EXPECTED_WITHDRAW_ACC_FR" =~ ^(0x[0-9a-fA-F]{1,64}|[0-9]+)$ ]] || die "bad withdraw acc Fr"
for value in "$EXPECTED_WITHDRAW_ALT_DST_CHAIN_ID" \
  "$EXPECTED_WITHDRAW_ALT_DST_HOST_CHAIN_ID" "$EXPECTED_WITHDRAW_ALT_TOKEN_ID"; do
  [[ "$value" =~ ^[0-9]+$ ]] || die "bad withdraw destination/token ID"
done

if [[ "$runtime_layout" == compose ]]; then
  [[ -s "$BRIDGE_REPO_DIR/SOURCE_COMMIT" ]] || die "container source marker is missing"
  [[ -s "$BRIDGE_REPO_DIR/IMAGE-SHA256SUMS" ]] || die "container image manifest is missing"
  actual_commit=$(tr -d '\r\n' <"$BRIDGE_REPO_DIR/SOURCE_COMMIT")
  [[ "$actual_commit" == "$EXPECTED_BRIDGE_COMMIT" ]] ||
    die "image source $actual_commit != $EXPECTED_BRIDGE_COMMIT"
  (
    cd "$BRIDGE_REPO_DIR"
    sha256sum --check --strict IMAGE-SHA256SUMS
  )
  ok "immutable image artifacts pinned at source $actual_commit"
else
  actual_commit=$(git -c safe.directory="$BRIDGE_REPO_DIR" -C "$BRIDGE_REPO_DIR" rev-parse HEAD)
  [[ "$actual_commit" == "$EXPECTED_BRIDGE_COMMIT" ]] ||
    die "source $actual_commit != $EXPECTED_BRIDGE_COMMIT"
  [[ -z $(git -c safe.directory="$BRIDGE_REPO_DIR" -C "$BRIDGE_REPO_DIR" \
    status --porcelain --untracked-files=no) ]] ||
    die "tracked source changes are present"
  ok "source pinned at $actual_commit"
fi

[[ -x "$RELAYER_BINARY" ]] || die "relayer binary is missing"
aggregator_binary=$BRIDGE_AGGREGATOR_DIR/target/release/aggregate-proof
[[ -x "$aggregator_binary" ]] || die "aggregate-proof binary is missing"
"$aggregator_binary" --help | grep -q -- --allow-source-drift ||
  die "aggregate-proof predates the verifier source self-check; rebuild it"
"$RELAYER_BINARY" --help >/dev/null || die "relayer cannot execute on this OS"
actual_relayer_hash=$(sha256sum "$RELAYER_BINARY" | awk '{print $1}')
actual_aggregator_hash=$(sha256sum "$aggregator_binary" | awk '{print $1}')
[[ "${actual_relayer_hash,,}" == "${EXPECTED_RELAYER_SHA256,,}" ]] || die "relayer binary checksum mismatch"
[[ "${actual_aggregator_hash,,}" == "${EXPECTED_AGGREGATE_PROOF_SHA256,,}" ]] || die "aggregate-proof checksum mismatch"
ok "release binary checksums"

derived_address=$(cast wallet address --private-key "$RELAYER_PRIVATE_KEY")
[[ "${derived_address,,}" == "${RELAYER_ADDRESS,,}" ]] ||
  die "private key derives $derived_address, expected $RELAYER_ADDRESS"
ok "private key matches expected EOA $RELAYER_ADDRESS"

actual_bk_hash=$(sha256sum "$BRIDGE_BK_SET_CONFIG" | awk '{print $1}')
[[ "${actual_bk_hash,,}" == "${BRIDGE_BK_SET_EXPECTED_SHA256,,}" ]] ||
  die "BK-set checksum mismatch"

declare -A srs_sizes=(
  [17]=16777476
  [19]=67109124
  [20]=134217988
  [21]=268435716
  [22]=536871172
)
for k in 17 19 20 21 22; do
  srs=$BRIDGE_PARAMS_DIR/kzg_bn254_${k}.srs
  [[ -f "$srs" ]] || die "missing K=$k Hermez SRS"
  size=$(stat -c '%s' "$srs")
  [[ "$size" == "${srs_sizes[$k]}" ]] || die "K=$k SRS size $size != ${srs_sizes[$k]}"
  s_g2_head=$(od -An -tx1 -j $((size - 128)) -N 6 "$srs" | tr -d ' \n')
  [[ "$s_g2_head" == 928fafb3d0cc ]] || die "K=$k SRS is not the Hermez ceremony"
done
[[ -d "$BRIDGE_PK_CACHE_DIR" && -w "$BRIDGE_PK_CACHE_DIR" ]] || die "PK cache is not writable"
[[ -d "$BRIDGE_STATE_DIR" && -w "$BRIDGE_STATE_DIR" ]] || die "prover state directory is not writable"
[[ -d "$BRIDGE_DUMP_SUBMISSIONS_DIR" && -w "$BRIDGE_DUMP_SUBMISSIONS_DIR" ]] ||
  die "submission dump directory is not writable"
[[ -d $(dirname -- "$RELAYER_STATE_PATH") && -w $(dirname -- "$RELAYER_STATE_PATH") ]] ||
  die "relayer state directory is not writable"
ok "BK set, Hermez SRS K=17/19/20/21/22, and writable state paths"

# Child diagnostics below do not need the signing key. This does not alter the
# EnvironmentFile inherited later by the daemon process.
export -n RELAYER_PRIVATE_KEY 2>/dev/null || true

chain_id=$(cast chain-id --rpc-url "$RPC_URL")
[[ "$chain_id" == "$EXPECTED_EVM_CHAIN_ID" ]] ||
  die "RPC chain id $chain_id != $EXPECTED_EVM_CHAIN_ID"
require_code "$BRIDGE_ADDRESS" "bridge"

owner=$(call_word "$BRIDGE_ADDRESS" 'owner()(address)')
[[ "${owner,,}" == "${RELAYER_ADDRESS,,}" ]] || die "bridge owner $owner != expected EOA $RELAYER_ADDRESS"
usdc=$(call_word "$BRIDGE_ADDRESS" 'usdc()(address)')
[[ "${usdc,,}" == "${EXPECTED_USDC_ADDRESS,,}" ]] || die "bridge USDC $usdc != $EXPECTED_USDC_ADDRESS"

last_seen=$(call_word "$BRIDGE_ADDRESS" 'storedLastSeenBlockSeqNo()(uint64)')
(( last_seen >= BRIDGE_BOOTSTRAP_SEQNO )) || die "on-chain cursor $last_seen is behind bootstrap"
(( (last_seen - BRIDGE_BOOTSTRAP_SEQNO) % 16384 == 0 )) || die "on-chain cursor is off the L2 deployment lane"
bk_decimal=$(call_word "$BRIDGE_ADDRESS" 'storedBkSetCommitment()(uint256)')
expected_bk_decimal=$(cast to-dec "$EXPECTED_BK_SET_COMMITMENT")
[[ "$bk_decimal" == "$expected_bk_decimal" ]] || die "on-chain BK commitment mismatch"
genesis_prev=$(call_word "$BRIDGE_ADDRESS" 'storedPrevMaxLevelLayerHash()(uint256)')
expected_genesis_prev=$(cast to-dec "$EXPECTED_GENESIS_PREV_ANCHOR")
[[ "$genesis_prev" == "$expected_genesis_prev" ]] || die "immutable genesis anchor mismatch"
withdraw_dapp=$(call_word "$BRIDGE_ADDRESS" 'bridgeWithdrawalDappFr()(uint256)')
expected_withdraw_dapp=$(cast to-dec "$EXPECTED_WITHDRAW_DAPP_FR")
[[ "$withdraw_dapp" == "$expected_withdraw_dapp" ]] || die "withdraw dApp Fr mismatch"
withdraw_acc=$(call_word "$BRIDGE_ADDRESS" 'bridgeWithdrawalAccFr()(uint256)')
expected_withdraw_acc=$(cast to-dec "$EXPECTED_WITHDRAW_ACC_FR")
[[ "$withdraw_acc" == "$expected_withdraw_acc" ]] || die "withdraw acc Fr mismatch"
withdraw_alt_chain=$(call_word "$BRIDGE_ADDRESS" 'bridgeWithdrawalAltDstChainId()(uint256)')
[[ "$withdraw_alt_chain" == "$EXPECTED_WITHDRAW_ALT_DST_CHAIN_ID" ]] || die "withdraw alt chain mismatch"
withdraw_alt_host=$(call_word "$BRIDGE_ADDRESS" 'bridgeWithdrawalAltDstHostChainId()(uint256)')
[[ "$withdraw_alt_host" == "$EXPECTED_WITHDRAW_ALT_DST_HOST_CHAIN_ID" ]] || die "withdraw alt host mismatch"
withdraw_alt_token=$(call_word "$BRIDGE_ADDRESS" 'bridgeWithdrawalAltTokenId()(uint256)')
[[ "$withdraw_alt_token" == "$EXPECTED_WITHDRAW_ALT_TOKEN_ID" ]] || die "withdraw alt token mismatch"

if [[ ! -f "$BRIDGE_STATE_DIR/prover_state.json" ]]; then
  [[ "$last_seen" == "$BRIDGE_BOOTSTRAP_SEQNO" ]] ||
    die "cold start has no local state but contract cursor $last_seen != bootstrap $BRIDGE_BOOTSTRAP_SEQNO"
  expected_prev=$(call_word "$BRIDGE_ADDRESS" 'expectedPrevAnchor(uint8)(uint256)' 2)
  [[ "$expected_prev" == "$expected_genesis_prev" ]] || die "cold contract expectedPrevAnchor(2) mismatch"
else
  jq -e '.initialized == true and .anchor_level == 2' "$BRIDGE_STATE_DIR/prover_state.json" >/dev/null ||
    die "existing prover state is not initialized L2 state"
fi
ok "Sepolia bridge owner/config/anchors cursor=$last_seen"

verify_verifier_lane 'primaryVerifier()(address)' 'PrimaryAggregatorVerifier.bin' 'primary'
verify_verifier_lane 'fallbackVerifier()(address)' 'FallbackAggregatorVerifier.bin' 'fallback'
verify_verifier_lane 'layerHashesVerifier()(address)' 'LayerHashesAggregatorVerifier.bin' 'layer-hashes'
verify_verifier_lane 'bridgeWithdrawalVerifier()(address)' 'BridgeWithdrawalAggregatorVerifier.bin' 'withdrawal'

latest_nonce=$(cast nonce "$RELAYER_ADDRESS" --block latest --rpc-url "$RPC_URL")
pending_nonce=$(cast nonce "$RELAYER_ADDRESS" --block pending --rpc-url "$RPC_URL")
[[ "$latest_nonce" == "$pending_nonce" ]] ||
  die "relayer EOA has a pending transaction (latest=$latest_nonce pending=$pending_nonce)"
balance=$(cast balance "$RELAYER_ADDRESS" --rpc-url "$RPC_URL")
(( balance >= MIN_RELAYER_BALANCE_WEI )) ||
  die "relayer balance $balance wei is below minimum $MIN_RELAYER_BALANCE_WEI"
ok "EOA nonce=$latest_nonce balance=$balance wei"

# The daemon retries the primary and then cycles through the failover list at
# runtime, so a single unreachable endpoint must not block a (re)start —
# this script runs on every container start. Fail only when no endpoint at
# all answers the smoke query.
payload='{"query":"{ blockchain { blocks(last: 2) { edges { node { seq_no thread_id } } } } }"}'
gql_smoke() {
  local response
  response=$(curl --fail-with-body --silent --show-error --max-time 20 \
    -H 'content-type: application/json' --data-binary "$payload" "$1") || return 1
  jq -e '.errors == null and (.data.blockchain.blocks.edges | length) > 0' \
    >/dev/null <<<"$response"
}
gql_alive=0
if gql_smoke "$BRIDGE_GQL_ENDPOINT"; then
  ok "shellnet GQL primary $BRIDGE_GQL_ENDPOINT"
  gql_alive=1
else
  printf 'WARN: primary GQL endpoint %s is not answering; the daemon will retry it and use failover endpoints\n' \
    "$BRIDGE_GQL_ENDPOINT" >&2
fi
IFS=',' read -r -a failover_endpoints <<<"${BRIDGE_GQL_FAILOVER_ENDPOINTS:-}"
for endpoint in "${failover_endpoints[@]}"; do
  endpoint=${endpoint//[[:space:]]/}
  [[ -n "$endpoint" ]] || continue
  if gql_smoke "$endpoint"; then
    ok "shellnet GQL failover $endpoint"
    gql_alive=1
  else
    printf 'WARN: failover GQL endpoint %s is not answering; the daemon will skip it until it recovers\n' \
      "$endpoint" >&2
  fi
done
(( gql_alive )) || die "no GraphQL endpoint answers (primary + BRIDGE_GQL_FAILOVER_ENDPOINTS); the daemon could not fetch a single block"

free_kib=$(df -Pk "$BRIDGE_PK_CACHE_DIR" | awk 'NR==2 {print $4}')
(( free_kib >= 80 * 1024 * 1024 )) || die "less than 80 GiB free on runtime filesystem"
ok "disk headroom $((free_kib / 1024 / 1024)) GiB"

printf 'READ-ONLY L2 PREFLIGHT PASSED. No transaction was sent and no daemon was started.\n'
