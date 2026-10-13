#!/usr/bin/env bash
# Deploy or redeploy the shellnet E2E bridge bundle end-to-end.
#
# Runs `compute_bridge_anchors --at-head` against live shellnet to derive
# GENESIS_LAST_SEEN_BLOCK_SEQNO / GENESIS_PREV_MAX_LEVEL_LAYER_HASH /
# GENESIS_BK_SET_COMMITMENT, deploys the bridge bundle via forge, extracts
# BRIDGE_ADDRESS from the broadcast record, rewrites the non-secret
# L${LEVEL}_config/env, and archives prior prover state.
#
# NOT IDEMPOTENT: every run broadcasts a new set of contracts and spends new
# nonces. Audit the Foundry broadcast record and latest/pending nonce before a
# retry. This is the flow described in
# crates/bridge-relayer-daemon/docs/live_relayer_bridge_verifyBlock_runbook.md §3.
#
# Usage:
#   LEVEL=1 PRIVATE_KEY=<burner> ./scripts/deploy_bridge_bundle.sh   # L1 anchor
#   LEVEL=2 PRIVATE_KEY=<burner> ./scripts/deploy_bridge_bundle.sh   # L2 anchor
#
# Inputs (env):
#   LEVEL                      1 or 2 (required)
#   CONFIRM_NEW_BRIDGE_DEPLOY  must equal DEPLOY_NEW_CONTRACTS
#   PRIVATE_KEY                Sepolia burner from runbook §1 (required)
#   SEPOLIA_RPC_URL            default: https://ethereum-sepolia-rpc.publicnode.com
#   BRIDGE_GQL_ENDPOINT        default: https://shellnet.ackinacki.org/graphql
#   BRIDGE_BK_SET_CONFIG       default: ./bk_set.shellnet.json
#                              (relative to crates/bridge-prover-libraries)
#   WITHDRAW_ACC_FR            authoritative AN bridge account field element;
#                              required and immutable after deployment
#   USE_AAVE                   true: deploy with DeployRealBridge instead, wired
#                              to the AAVE V3 Sepolia pool. The bridge then takes
#                              the pool's USDC (0x94a9D9AC…E4C8), not Circle's,
#                              and it replaces the live bridge: pause that one
#                              first, then trust the new address on the AN side
#                              (docs/aave-yield.md, "Launching a Sepolia stand
#                              with AAVE"). Default: false.
#   DEMO_AAVE                  true: keep DeployShellnetE2EBridge and Circle USDC,
#                              and wire the bridge to a fresh DemoAavePool owned
#                              by PRIVATE_KEY; yield is whatever that key pays in
#                              with accrueYield (docs/aave-yield.md, "A demo pool
#                              on Sepolia"). Replaces the live bridge as well.
#                              Excludes USE_AAVE. Default: false.
set -euo pipefail
set +x
umask 077

: "${LEVEL:?set LEVEL=1 or LEVEL=2}"
: "${PRIVATE_KEY:?set PRIVATE_KEY=<sepolia burner>}"
: "${WITHDRAW_ACC_FR:?set authoritative WITHDRAW_ACC_FR (or an explicit dummy for verifyBlock-only smoke)}"
[[ "${CONFIRM_NEW_BRIDGE_DEPLOY:-}" == DEPLOY_NEW_CONTRACTS ]] || {
  echo "Refusing broadcast: set CONFIRM_NEW_BRIDGE_DEPLOY=DEPLOY_NEW_CONTRACTS" >&2
  exit 2
}
[[ "$LEVEL" == 1 || "$LEVEL" == 2 ]] || {
  echo "LEVEL must be 1 or 2" >&2
  exit 2
}
SEPOLIA_RPC_URL="${SEPOLIA_RPC_URL:-https://ethereum-sepolia-rpc.publicnode.com}"
BRIDGE_GQL_ENDPOINT="${BRIDGE_GQL_ENDPOINT:-https://shellnet.ackinacki.org/graphql}"
BRIDGE_BK_SET_CONFIG="${BRIDGE_BK_SET_CONFIG:-./bk_set.shellnet.json}"
USE_AAVE="${USE_AAVE:-false}"
DEMO_AAVE="${DEMO_AAVE:-false}"
[[ "$USE_AAVE" == true || "$USE_AAVE" == false ]] || {
  echo "USE_AAVE must be true or false" >&2
  exit 2
}
[[ "$DEMO_AAVE" == true || "$DEMO_AAVE" == false ]] || {
  echo "DEMO_AAVE must be true or false" >&2
  exit 2
}
[[ "$USE_AAVE" == false || "$DEMO_AAVE" == false ]] || {
  echo "USE_AAVE and DEMO_AAVE exclude each other" >&2
  exit 2
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
PROVER_DIR="$REPO_ROOT/crates/bridge-prover-libraries"
CONTRACTS_DIR="$REPO_ROOT/contracts/ethereum"
GENESIS_ENV="$(mktemp -t genesis.XXXXXX.env)"
trap 'rm -f "$GENESIS_ENV"' EXIT

# ─── 1. Derive genesis anchors from live shellnet chain head ──────────────────
echo ">>> Computing genesis anchors (level=$LEVEL) against $BRIDGE_GQL_ENDPOINT"
cd "$PROVER_DIR/bridge-prover-lib"
BRIDGE_GQL_ENDPOINT="$BRIDGE_GQL_ENDPOINT" \
BRIDGE_BK_SET_CONFIG="$PROVER_DIR/${BRIDGE_BK_SET_CONFIG#./}" \
cargo run --release --quiet --bin compute_bridge_anchors -- \
  --at-head --level "$LEVEL" \
  > "$GENESIS_ENV"

echo ">>> Derived anchors:"
cat "$GENESIS_ENV"
set -a && source "$GENESIS_ENV" && set +a

# CRITICAL RENAME: compute_bridge_anchors emits GENESIS_SEED_SEQNO;
# DeployShellnetE2EBridge.s.sol reads GENESIS_LAST_SEEN_BLOCK_SEQNO via
# vm.envOr(..., 0) — without this alias the constructor silently pins 0.
export GENESIS_LAST_SEEN_BLOCK_SEQNO="$GENESIS_SEED_SEQNO"

# ─── 2. Deploy the bundle (14 CREATEs across 6 logical components) ────────────
cd "$CONTRACTS_DIR"
export PRIVATE_KEY
export WIRE_WITHDRAW_BY_PROOF=true
export WITHDRAW_ACC_FR

if [[ "$USE_AAVE" == true ]]; then
  DEPLOY_SCRIPT=DeployRealBridge
  # DeployRealBridge defaults the withdraw aliases to 0; the shellnet AN side
  # emits dstChainId=1 / tokenId=3, which only these aliases accept.
  export USE_AAVE WIRE_VERIFY_BLOCK=true
  export WITHDRAW_ALT_DST_CHAIN_ID="${WITHDRAW_ALT_DST_CHAIN_ID:-1}"
  export WITHDRAW_ALT_DST_HOST_CHAIN_ID="${WITHDRAW_ALT_DST_HOST_CHAIN_ID:-11155111}"
  export WITHDRAW_ALT_TOKEN_ID="${WITHDRAW_ALT_TOKEN_ID:-3}"
  EXPECTED_USDC_LINE="EXPECTED_USDC_ADDRESS=0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8"
  EXPECTED_AAVE_LINE="EXPECTED_AAVE_POOL=0x6Ae43d3271ff6888e7Fc43Fd7321a503ff738951"
else
  DEPLOY_SCRIPT=DeployShellnetE2EBridge
  export DEMO_AAVE
  EXPECTED_USDC_LINE="EXPECTED_USDC_ADDRESS=0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238"
  EXPECTED_AAVE_LINE="(leave EXPECTED_AAVE_POOL unset: DeployShellnetE2EBridge wires no pool)"
fi

echo ">>> forge script $DEPLOY_SCRIPT (chain 11155111, USE_AAVE=$USE_AAVE, DEMO_AAVE=$DEMO_AAVE)"
forge script "script/$DEPLOY_SCRIPT.s.sol:$DEPLOY_SCRIPT" \
  --rpc-url "$SEPOLIA_RPC_URL" --broadcast --slow

# ─── 3. Extract BRIDGE_ADDRESS from the broadcast record ──────────────────────
BROADCAST="$CONTRACTS_DIR/broadcast/$DEPLOY_SCRIPT.s.sol/11155111/run-latest.json"
BRIDGE_ADDRESS="$(jq -r '
  .transactions[]
  | select(.contractName=="AckiNackiBridge" and .transactionType=="CREATE")
  | .contractAddress
' "$BROADCAST" | tail -n 1)"
: "${BRIDGE_ADDRESS:?failed to extract AckiNackiBridge address from $BROADCAST}"
echo ">>> Deployed BRIDGE_ADDRESS = $BRIDGE_ADDRESS"
if [[ "$DEMO_AAVE" == true ]]; then
  DEMO_POOL="$(jq -r '
    .transactions[]
    | select(.contractName=="DemoAavePool" and .transactionType=="CREATE")
    | .contractAddress
  ' "$BROADCAST" | tail -n 1)"
  : "${DEMO_POOL:?failed to extract DemoAavePool address from $BROADCAST}"
  echo ">>> Deployed DemoAavePool = $DEMO_POOL"
  EXPECTED_AAVE_LINE="EXPECTED_AAVE_POOL=$DEMO_POOL"
fi
# The block that CREATE landed in: where the relayer's and the CLI's
# `LayerAnchorAppended` scan stops (BRIDGE_DEPLOY_BLOCK). Never fatal: the
# bridge is on-chain by now, and a broadcast record without a matching
# receipt must not leave the config below unwritten.
deploy_block_hex="$(jq -r --arg addr "$(printf '%s' "$BRIDGE_ADDRESS" | tr '[:upper:]' '[:lower:]')" '
  (.receipts // [])[]
  | select(((.contractAddress // "") | ascii_downcase) == $addr)
  | .blockNumber // empty
' "$BROADCAST" 2>/dev/null | tail -n 1 || true)"
if [[ "$deploy_block_hex" =~ ^0x[0-9a-fA-F]+$ ]]; then
  deploy_block_line="BRIDGE_DEPLOY_BLOCK=$((deploy_block_hex))"
  echo ">>> Deployed in block $((deploy_block_hex))"
else
  deploy_block_line="# BRIDGE_DEPLOY_BLOCK=<block of the AckiNackiBridge CREATE receipt in $BROADCAST>"
  echo ">>> WARNING: could not read the deploy block from $BROADCAST; set BRIDGE_DEPLOY_BLOCK in the env by hand" >&2
fi

# ─── 4. Rewrite non-secret L${LEVEL}_config/env ───────────────────────────────
CFG_DIR="$PROVER_DIR/L${LEVEL}_config"
mkdir -p "$CFG_DIR/state" "$CFG_DIR/proofs" "$CFG_DIR/work_dir"

cat > "$CFG_DIR/env" <<EOF
# Generated by scripts/deploy_bridge_bundle.sh — do not hand-edit
# BRIDGE_BOOTSTRAP_SEQNO; re-run the script to regenerate anchors.
source "\$(dirname "\${BASH_SOURCE[0]}")/../shellnet.common"

BRIDGE_ADDRESS=$BRIDGE_ADDRESS
$deploy_block_line
BRIDGE_BOOTSTRAP_SEQNO=$GENESIS_SEED_SEQNO
BRIDGE_ANCHOR_LEVEL=$LEVEL
# For the shellnet-l2 runtime env (preflight.sh):
# $EXPECTED_USDC_LINE
# $EXPECTED_AAVE_LINE
EOF
echo ">>> Wrote $CFG_DIR/env"
if [[ "$USE_AAVE" == true || "$DEMO_AAVE" == true ]]; then
  echo ">>> AAVE stand: it replaces the live bridge, which must already be paused (docs/aave-yield.md, step 2)."
  echo ">>> Trust $BRIDGE_ADDRESS on the AN bridge with setTrustedL1Bridge before the first deposit."
fi

# ─── 5. Archive stale state so the new bridge cold-start remains recoverable ─
STATE_ARCHIVE="$CFG_DIR/state.pre_deploy_$(date -u +%Y%m%dT%H%M%SZ)"
if compgen -G "$CFG_DIR/state/*.json" >/dev/null; then
  mkdir -p "$STATE_ARCHIVE"
  mv -- "$CFG_DIR/state"/*.json "$STATE_ARCHIVE/"
  echo ">>> Archived prior state in $STATE_ARCHIVE"
else
  echo ">>> No prior state JSON to archive"
fi

cat <<EOF

>>> Done. Next:
    cd $PROVER_DIR
    export BRIDGE_CONFIG_DIR=./L${LEVEL}_config
    set -a && source "\$BRIDGE_CONFIG_DIR/env" && set +a
    ./target/release/relayer --state "\$BRIDGE_CONFIG_DIR/relayer-state.json" daemon-live
EOF
