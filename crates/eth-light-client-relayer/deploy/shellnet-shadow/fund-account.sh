#!/usr/bin/env bash
# Fund an account on shellnet from the zerostate giver (dapp_id 0).
#
#   fund-account.sh <address> [nano_vmshell]
#
# Same two-shot pattern as the bridge python helper (`send_from_giver`):
# `sendCurrencyWithFlag` with flag 16, then flag 2, each carrying `value`
# and ECC[2] = value. Default 10_000 vmshell (1e13 nano).

# shellcheck disable=SC1091
source "$(dirname "$0")/lib.sh"

[[ $# -ge 1 ]] || die "usage: $0 <address> [nano_vmshell]"
DEST="$(legacy_addr "$1")"
VALUE="${2:-10000000000000}"
[[ -f "$GIVER_ABI" ]] || die "giver ABI missing: $GIVER_ABI"
[[ -f "$GIVER_KEYS" ]] || die "giver keys missing: $GIVER_KEYS (copy from bridge repo python/contracts, chmod 600)"

tvm_configure
GIVER="${GIVER_DAPP}::${GIVER_ACCOUNT}"
log "giver $GIVER -> $DEST value=$VALUE ecc[2]=$VALUE"
for flag in 16 2; do
    tvm callx --abi "$GIVER_ABI" --keys "$GIVER_KEYS" --addr "$GIVER" -m sendCurrencyWithFlag \
        "{\"dest\":\"$DEST\",\"value\":$VALUE,\"ecc\":{\"2\":$VALUE},\"flag\":$flag}" >/dev/null \
        || log "shot flag=$flag returned non-zero (may be fine on the second shot)"
done
sleep 3
account_json "$(dapp_addr_self "$DEST")" | grep -E '"(acc_type|balance|ecc_balance)"' || log "account not visible yet"
