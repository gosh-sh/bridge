#!/usr/bin/env bash
# Deploy and wire Circle CCTP for a USDC burn into a hub AckiNackiBridge,
# then print the two keeper commands.
#
#   deploy-endpoint   source-chain CctpEndpoint (Arbitrum, or the hub for bridgeOut)
#   deploy-receiver   hub CctpHubReceiver, next to an existing AckiNackiBridge
#   set-hub           source endpoint: which hub receiver a token burns toward
#   set-source        hub receiver: which remote endpoint may burn which token
#   set-egress        hub endpoint: which domain bridgeOut may mint on
#   set-forwarder     hub receiver: which keeper EOA may call forward
#   print-keepers     print cctp-daemon and the hub deposit-relayer daemon
#
# A broadcast refuses to run unless CONFIRM_CCTP=BROADCAST. Gas limits come
# from the node (--skip-simulation): forge's local EVM can lag a fork that
# repriced gas, and its estimate then runs the deployment out of gas. Circle contract
# addresses are not defaulted: pass the TokenMessengerV2 / MessageTransmitterV2
# / USDC for that chain from Circle's registry.
#
# print-keepers does not start a process and does not print private keys.
# The keeper sends two hub transactions per burn: receiveBurn (Circle message
# and attestation, mints and parks) and forward (36 bytes, calls
# bridge.deposit). Only forward's Deposit is proven, so the keeper EOA must be
# a forwarder (set-forwarder) and must send EIP-1559 transactions directly.
#
# Arbitrum's CCTP domain is 3. Ethereum's is 0. Those are not chain ids.
set -euo pipefail
set +x

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ETH_DIR="${ROOT}/contracts/ethereum"
RELAYER_DIR="${ROOT}/crates/deposit-relayer-daemon"

usage() {
  sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
}

require_addr() {
  local name="$1" value="$2"
  [[ "${value}" =~ ^0x[0-9a-fA-F]{40}$ ]] || {
    echo "${name} must be a 20-byte 0x address, got: ${value}" >&2
    exit 2
  }
}

require_uint() {
  local name="$1" value="$2"
  [[ "${value}" =~ ^[0-9]+$ ]] || {
    echo "${name} must be an unsigned integer, got: ${value}" >&2
    exit 2
  }
}

require_broadcast() {
  : "${RPC_URL:?set RPC_URL to the chain this transaction is sent to}"
  : "${PRIVATE_KEY:?set PRIVATE_KEY to the owner/deployer key}"
  [[ "${CONFIRM_CCTP:-}" == BROADCAST ]] || {
    echo "Refusing broadcast: set CONFIRM_CCTP=BROADCAST" >&2
    exit 2
  }
}

forge_broadcast() {
  local script="$1"
  (cd "${ETH_DIR}" && forge script "${script}" --rpc-url "${RPC_URL}" --broadcast --slow \
    --skip-simulation)
}

cmd="${1:-}"
shift || true

case "${cmd}" in
  deploy-endpoint)
    require_broadcast
    require_addr TOKEN_MESSENGER "${TOKEN_MESSENGER:?set TOKEN_MESSENGER}"
    export CCTP_KIND=endpoint
    echo ">>> Deploying CctpEndpoint via ${RPC_URL}"
    forge_broadcast script/DeployCctp.s.sol:DeployCctp
    ;;
  deploy-receiver)
    require_broadcast
    require_addr DEPOSIT_BRIDGE "${DEPOSIT_BRIDGE:?set DEPOSIT_BRIDGE to the hub AckiNackiBridge}"
    require_addr MESSAGE_TRANSMITTER "${MESSAGE_TRANSMITTER:?set MESSAGE_TRANSMITTER}"
    require_uint CCTP_LOCAL_DOMAIN "${CCTP_LOCAL_DOMAIN:?set CCTP_LOCAL_DOMAIN (Ethereum is 0)}"
    export CCTP_KIND=receiver
    echo ">>> Deploying CctpHubReceiver via ${RPC_URL}"
    forge_broadcast script/DeployCctp.s.sol:DeployCctp
    ;;
  set-hub)
    require_broadcast
    require_addr CCTP_ENDPOINT "${CCTP_ENDPOINT:?set CCTP_ENDPOINT}"
    require_addr CCTP_TOKEN "${CCTP_TOKEN:?set CCTP_TOKEN to source-chain USDC}"
    require_uint CCTP_DESTINATION_DOMAIN "${CCTP_DESTINATION_DOMAIN:?set CCTP_DESTINATION_DOMAIN (hub domain, Ethereum is 0)}"
    require_addr CCTP_HUB_RECEIVER "${CCTP_HUB_RECEIVER:?set CCTP_HUB_RECEIVER}"
    export CCTP_WIRE=set-hub
    export CCTP_ENABLED="${CCTP_ENABLED:-true}"
    echo ">>> setHub on ${CCTP_ENDPOINT}"
    forge_broadcast script/WireCctp.s.sol:WireCctp
    ;;
  set-source)
    require_broadcast
    require_addr CCTP_RECEIVER "${CCTP_RECEIVER:?set CCTP_RECEIVER}"
    require_uint CCTP_SOURCE_DOMAIN "${CCTP_SOURCE_DOMAIN:?set CCTP_SOURCE_DOMAIN (Arbitrum is 3)}"
    require_uint CCTP_SOURCE_CHAIN_ID "${CCTP_SOURCE_CHAIN_ID:?set CCTP_SOURCE_CHAIN_ID (Arbitrum One is 42161)}"
    require_addr CCTP_ADAPTER "${CCTP_ADAPTER:?set CCTP_ADAPTER to the source CctpEndpoint}"
    require_addr CCTP_REMOTE_TOKEN "${CCTP_REMOTE_TOKEN:?set CCTP_REMOTE_TOKEN to source-chain USDC}"
    export CCTP_WIRE=set-source
    export CCTP_ENABLED="${CCTP_ENABLED:-true}"
    echo ">>> setSource on ${CCTP_RECEIVER}"
    forge_broadcast script/WireCctp.s.sol:WireCctp
    ;;
  set-egress)
    require_broadcast
    require_addr CCTP_ENDPOINT "${CCTP_ENDPOINT:?set CCTP_ENDPOINT to the hub CctpEndpoint}"
    require_uint CCTP_EGRESS_DOMAIN "${CCTP_EGRESS_DOMAIN:?set CCTP_EGRESS_DOMAIN}"
    : "${CCTP_EGRESS_ALLOWED:?set CCTP_EGRESS_ALLOWED to true or false}"
    export CCTP_WIRE=set-egress
    echo ">>> setEgressDomain on ${CCTP_ENDPOINT}"
    forge_broadcast script/WireCctp.s.sol:WireCctp
    ;;
  set-forwarder)
    require_broadcast
    require_addr CCTP_RECEIVER "${CCTP_RECEIVER:?set CCTP_RECEIVER}"
    require_addr CCTP_FORWARDER "${CCTP_FORWARDER:?set CCTP_FORWARDER to the cctp-daemon EOA}"
    export CCTP_WIRE=set-forwarder
    export CCTP_ENABLED="${CCTP_ENABLED:-true}"
    echo ">>> setForwarder on ${CCTP_RECEIVER}"
    forge_broadcast script/WireCctp.s.sol:WireCctp
    ;;
  print-keepers)
    : "${SOURCE_RPC_URL:?set SOURCE_RPC_URL}"
    require_uint SOURCE_DOMAIN "${SOURCE_DOMAIN:?set SOURCE_DOMAIN (Arbitrum is 3)}"
    require_addr CCTP_ENDPOINT "${CCTP_ENDPOINT:?set CCTP_ENDPOINT to the source endpoint}"
    require_uint FROM_BLOCK "${FROM_BLOCK:?set FROM_BLOCK to the source endpoint deploy block}"
    : "${CCTP_IRIS_URL:?set CCTP_IRIS_URL to https://iris-api.circle.com or the sandbox host}"
    : "${HUB_RPC_URL:?set HUB_RPC_URL}"
    require_addr CCTP_RECEIVER "${CCTP_RECEIVER:?set CCTP_RECEIVER}"
    require_addr HUB_BRIDGE "${HUB_BRIDGE:?set HUB_BRIDGE to the hub AckiNackiBridge}"
    cat <<EOF
# 1. Submit Circle attestations: receiveBurn, then forward. Export
#    CCTP_HUB_PRIVATE_KEY in the environment of this process; the key is not
#    written here, and its address must be a forwarder on the receiver. This
#    does not prove and does not call Acki Nacki. Cursor:
#    ./cctp-forward-state.json
cd ${RELAYER_DIR}
cargo run --bin deposit-relayer -- cctp-daemon \\
  --source-rpc-url ${SOURCE_RPC_URL} \\
  --source-domain ${SOURCE_DOMAIN} \\
  --endpoint ${CCTP_ENDPOINT} \\
  --from-block ${FROM_BLOCK} \\
  --iris-url ${CCTP_IRIS_URL} \\
  --hub-rpc-url ${HUB_RPC_URL} \\
  --receiver ${CCTP_RECEIVER}

# 2. Prove the hub Deposit and call finalizeDeposit. Fill the Acki Nacki flags
#    the way a normal deposit-relayer daemon is started (AN_GRAPHQL_URL,
#    AN_KEYS_PATH, AN_BRIDGE_ABI_PATH, AN_TOKEN_BRIDGE, AN_SENDER, AN_DAPP_ID,
#    and the deposit-prover directory). The Acki Nacki bridge must trust the
#    hub bridge for the hub chain id (setTrustedL1Bridge), not the source L2.
cargo run --bin deposit-relayer -- daemon \\
  --rpc-url ${HUB_RPC_URL} \\
  --bridge-address ${HUB_BRIDGE} \\
  --from-block ${HUB_FROM_BLOCK:-0} \\
  --deposit-prover-dir ${DEPOSIT_PROVER_DIR:-<path-to-deposit-prover>}
EOF
    ;;
  *)
    usage
    ;;
esac
