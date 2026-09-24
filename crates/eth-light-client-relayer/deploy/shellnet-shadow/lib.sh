# Shared helpers for the shellnet shadow deployment scripts. Source, do not run.
#
# Layout (override with ETH_LC_ROOT):
#   $ROOT/src/bridge   checkout of gosh-sh/bridge (this crate + eth-light-client-prover)
#   $ROOT/bin          tvm-cli, sold, eth-lc-relayer
#   $ROOT/srs          Hermez KZG SRS (kzg_bn254_19.srs)
#   $ROOT/config       env file, key files (0600), tvm-cli config, giver ABI
#   $ROOT/state        daemon state file
#   $ROOT/bundles      prove-one outputs
#   $ROOT/logs
#   $ROOT/contracts    compiled EthBeaconLightClient (.tvc/.abi.json)

set -euo pipefail

ROOT="${ETH_LC_ROOT:-/mnt/data/gosh-eth-lc-relayer}"
SRC="${ETH_LC_SRC:-$ROOT/src/bridge}"
BIN="$ROOT/bin"
CFG="$ROOT/config"
ENV_FILE="${ETH_LC_ENV:-$CFG/eth-lc-relayer.env}"

TVM_CLI="${TVM_CLI:-$BIN/tvm-cli}"
SOLD="${SOLD:-$BIN/sold}"
RELAYER="${RELAYER:-$BIN/eth-lc-relayer}"

# Shellnet endpoints. AN_NODE_URL is the tvm-cli `--url` (REST/dapp_id API),
# AN_GRAPHQL_URL is what the relayer's tvm_client uses.
AN_NODE_URL="${AN_NODE_URL:-shellnet.ackinacki.org}"
AN_GRAPHQL_URL="${AN_GRAPHQL_URL:-https://shellnet.ackinacki.org/graphql}"

# Zerostate giver of shellnet (dapp_id 0). Key file lives in the bridge repo
# under crates/an-bridge-prover/python/contracts/GiverV3.keys.json; copy it to
# $CFG with mode 0600, never into this deploy directory.
GIVER_ACCOUNT="1111111111111111111111111111111111111111111111111111111111111111"
GIVER_DAPP="0000000000000000000000000000000000000000000000000000000000000000"
GIVER_ABI="${GIVER_ABI:-$CFG/GiverV3.abi.json}"
GIVER_KEYS="${GIVER_KEYS:-$CFG/GiverV3.keys.json}"

log()  { printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*" >&2; }
die()  { log "ERROR: $*"; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "missing tool: $1"; }

load_env() {
    if [[ -f "$ENV_FILE" ]]; then
        set -a
        # shellcheck disable=SC1090
        source "$ENV_FILE"
        set +a
    fi
}

# tvm-cli keeps tvm-cli.conf.json in the current directory; pin it to $CFG so
# the url/config never lands in a random cwd.
tvm() {
    ( cd "$CFG" && "$TVM_CLI" "$@" )
}

tvm_json() {
    ( cd "$CFG" && "$TVM_CLI" -j "$@" )
}

tvm_configure() {
    [[ -x "$TVM_CLI" ]] || die "tvm-cli not found at $TVM_CLI"
    mkdir -p "$CFG"
    tvm config --url "$AN_NODE_URL" >/dev/null
}

# "0:<acc>" | "<acc>" | "<dapp>::<acc>"  ->  "<acc>"
acc_id_of() {
    local a="$1"
    a="${a##*::}"
    a="${a#0:}"
    printf '%s' "$a"
}

# Self-rooted deploy: dapp_id == account_id.
dapp_addr_self() { local a; a="$(acc_id_of "$1")"; printf '%s::%s' "$a" "$a"; }
legacy_addr()    { printf '0:%s' "$(acc_id_of "$1")"; }

# JSON field from tvm-cli -j output (python, no jq dependency).
json_get() {
    python3 -c 'import json,sys; d=json.load(sys.stdin); k=sys.argv[1]
v=d
for p in k.split("."):
    v=v[p]
print(v if not isinstance(v,(dict,list)) else json.dumps(v))' "$1"
}

pubkey_of_keys() {
    python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["public"])' "$1"
}

account_json() {
    tvm_json account "$1" 2>/dev/null || true
}

wait_account() {
    # wait_account <dapp::acc> <expected acc_type substring> [attempts]
    local addr="$1" want="$2" n="${3:-20}" out
    for _ in $(seq 1 "$n"); do
        out="$(account_json "$addr")"
        if [[ -n "$out" ]] && printf '%s' "$out" | grep -q "\"acc_type\": *\"$want\""; then
            return 0
        fi
        sleep 5
    done
    return 1
}
