#!/usr/bin/env bash
# On-chain head vs daemon state vs live Sepolia finality.

# shellcheck disable=SC1091
source "$(dirname "$0")/lib.sh"
load_env
[[ -n "${AN_LIGHT_CLIENT:-}" ]] || die "AN_LIGHT_CLIENT not set (env file $ENV_FILE)"
ABI="${AN_LC_ABI_PATH:-$ROOT/contracts/EthBeaconLightClient.abi.json}"
STATE="${ETH_LC_STATE:-$ROOT/state/eth-lc-relayer-state.json}"

tvm_configure
echo "== EthBeaconLightClient $AN_LIGHT_CLIENT"
HEAD_JSON="$(tvm_json runx --abi "$ABI" --addr "$AN_LIGHT_CLIENT" -m getHead)"
printf '%s\n' "$HEAD_JSON"
# The contract stores 32-byte roots as `(hi << 128) | lo` over little-endian
# 16-byte halves (same encoding as USDCBridge._parseBlockHash), so the getter
# hex is not the Ethereum hash literal: reverse each half to compare with
# eth_getBlockByHash / the beacon finality_update.
printf '%s' "$HEAD_JSON" | python3 -c '
import json, sys
d = json.load(sys.stdin)
def eth(v):
    b = bytes.fromhex(v[2:].rjust(64, "0"))
    return "0x" + (b[:16][::-1] + b[16:][::-1]).hex()
print("  execution block hash (Ethereum form):", eth(d["executionBlockHash"]))
print("  finalized beacon root (Ethereum form):", eth(d["finalizedBeaconRoot"]))
'
tvm_json runx --abi "$ABI" --addr "$AN_LIGHT_CLIENT" -m getCommitteeState
account_json "$AN_LIGHT_CLIENT" | grep -E '"(acc_type|balance|ecc_balance)"'

echo "== daemon state $STATE"
[[ -f "$STATE" ]] && cat "$STATE" || echo "(none)"

if [[ -n "${BEACON_URL:-}" ]]; then
    echo "== Sepolia finality ($BEACON_URL)"
    curl -s -m 15 -H 'accept: application/json' "$BEACON_URL/eth/v1/beacon/light_client/finality_update" \
        | python3 -c 'import json,sys; d=json.load(sys.stdin)["data"]; f=d["finalized_header"]; print("finalized_slot", f["beacon"]["slot"], "exec", f["execution"]["block_hash"])' \
        || echo "(beacon unreachable)"
fi

if command -v systemctl >/dev/null 2>&1; then
    echo "== systemd"
    systemctl --no-pager --lines=0 status eth-lc-relayer-shadow 2>/dev/null | head -5 || true
fi
