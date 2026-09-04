#!/usr/bin/env bash
# Deploy a fresh EthBeaconLightClient to shellnet (self-rooted: dapp_id ==
# account_id) with the committee UNSET (commitment 0 / period 0).
#
#   deploy-contract.sh [--keys <owner.keys.json>] [--l1-chain-id 11155111] [--fund <nano>]
#
# Owner keys are generated when the file does not exist. The committee is set
# afterwards from the first proven bundle (`eth-lc-relayer set-committee`),
# which is the weak-subjectivity anchor of this deployment.
#
# Prints AN_LIGHT_CLIENT / AN_SENDER for the env file.

# shellcheck disable=SC1091
source "$(dirname "$0")/lib.sh"
need python3

KEYS="$CFG/eth-lc-owner.keys.json"
L1_CHAIN_ID=11155111
FUND=10000000000000
while [[ $# -gt 0 ]]; do
    case "$1" in
        --keys) KEYS="$2"; shift 2 ;;
        --l1-chain-id) L1_CHAIN_ID="$2"; shift 2 ;;
        --fund) FUND="$2"; shift 2 ;;
        *) die "unknown arg: $1" ;;
    esac
done

TVC="$ROOT/contracts/EthBeaconLightClient.tvc"
ABI="$ROOT/contracts/EthBeaconLightClient.abi.json"
[[ -f "$TVC" && -f "$ABI" ]] || die "compile first: compile-contract.sh"

tvm_configure
mkdir -p "$CFG"
if [[ ! -f "$KEYS" ]]; then
    log "generating owner keypair -> $KEYS"
    ( umask 077 && tvm getkeypair -o "$KEYS" >/dev/null )
fi
chmod 600 "$KEYS"
PUB="$(pubkey_of_keys "$KEYS")"
log "owner pubkey 0x$PUB"

ADDR_RAW="$(tvm_json genaddr "$TVC" --abi "$ABI" --setkey "$KEYS" | json_get raw_address)"
ACC="$(acc_id_of "$ADDR_RAW")"
DAPP_ADDR="$(dapp_addr_self "$ACC")"
log "contract address $ADDR_RAW  (cli form $DAPP_ADDR)"

if account_json "$DAPP_ADDR" | grep -q '"acc_type": *"Active"'; then
    log "already Active at $DAPP_ADDR, skipping fund/deploy"
else
    "$(dirname "$0")/fund-account.sh" "$ADDR_RAW" "$FUND"
    wait_account "$DAPP_ADDR" "Uninit" 24 || die "account did not appear after funding"
    # Constructor parameter names come from the compiled ABI (positional:
    # owner pubkey, L1 chain id, bootstrap commitment, bootstrap period).
    CTOR_PARAMS="$(python3 - "$ABI" "$PUB" "$L1_CHAIN_ID" <<'PY'
import json, sys
abi, pub, chain = sys.argv[1:4]
ctor = next(f for f in json.load(open(abi))["functions"] if f["name"] == "constructor")
names = [i["name"] for i in ctor["inputs"]]
assert len(names) == 4, names
print(json.dumps({names[0]: "0x" + pub, names[1]: int(chain), names[2]: 0, names[3]: 0}))
PY
)"
    log "deployx $CTOR_PARAMS"
    tvm deployx --abi "$ABI" --keys "$KEYS" "$TVC" "$CTOR_PARAMS"
    wait_account "$DAPP_ADDR" "Active" 24 || die "contract did not become Active"
fi

log "getVersion / getConfig / getCommitteeState"
tvm_json runx --abi "$ABI" --addr "$DAPP_ADDR" -m getVersion
tvm_json runx --abi "$ABI" --addr "$DAPP_ADDR" -m getConfig
tvm_json runx --abi "$ABI" --addr "$DAPP_ADDR" -m getCommitteeState

cat <<EOF

# add to $ENV_FILE
AN_LIGHT_CLIENT=$DAPP_ADDR
AN_SENDER=$DAPP_ADDR
AN_KEYS_PATH=$KEYS
AN_LC_ABI_PATH=$ABI
EOF
