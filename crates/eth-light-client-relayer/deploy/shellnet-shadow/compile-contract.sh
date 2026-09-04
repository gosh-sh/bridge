#!/usr/bin/env bash
# Compile contracts/an/EthBeaconLightClient.sol with a pinned Linux `sold`
# and check that the embedded step VK_BLOB equals the prover fixture.
#
# The `gosh.zkhalo2VerifyWithVK` builtin is in TVM-Solidity-Compiler master
# and in releases >= gosh_0.81.0 (Linux x86_64 tarballs). Output goes to
# $ROOT/contracts.

# shellcheck disable=SC1091
source "$(dirname "$0")/lib.sh"
need python3

SOLD_VERSION="${SOLD_VERSION:-gosh_0.81.0}"
SOLD_TGZ_SHA="${SOLD_TGZ_SHA:-79b43aab6ae2fdc35c1cb3b486453aa2cf4f2718c58b69ccefab1122c704853c}"
SOLD_URL="https://github.com/gosh-sh/TVM-Solidity-Compiler/releases/download/${SOLD_VERSION}/sold_${SOLD_VERSION}_linux_x86_64.tar.gz"

OUT="$ROOT/contracts"
SRC_SOL="$SRC/contracts/an"
mkdir -p "$OUT" "$BIN"

if [[ ! -x "$SOLD" ]]; then
    need curl
    log "downloading sold $SOLD_VERSION"
    tmp="$(mktemp -d)"
    curl -sSL -o "$tmp/sold.tgz" "$SOLD_URL"
    echo "$SOLD_TGZ_SHA  $tmp/sold.tgz" | sha256sum -c - >/dev/null || die "sold tarball sha256 mismatch"
    tar -xzf "$tmp/sold.tgz" -C "$tmp"
    install -m 755 "$tmp/sold" "$SOLD"
    rm -rf "$tmp"
fi
log "sold: $("$SOLD" --version | head -1)"

log "embedded VK_BLOB vs fixture"
( cd "$SRC" && python3 scripts/embed_step_vk_blob.py --check contracts/an/EthBeaconLightClient.sol )

log "compiling EthBeaconLightClient.sol"
( cd "$SRC_SOL" && "$SOLD" --tvm-version gosh --base-path . EthBeaconLightClient.sol -o "$OUT" 2>&1 | grep -v -E "toSlice\(\)|^\s*\|\s*$|^\s*-->|^\s*[0-9]+ \||\^\^\^|deprecated" || true )
[[ -f "$OUT/EthBeaconLightClient.tvc" ]] || die "no tvc produced"
rm -f "$OUT/EthBeaconLightClient.debug.json" "$OUT/EthBeaconLightClient.code"

# The relayer ships a slim ABI; the compiler's full ABI must be a superset
# (same function signatures for what the relayer calls).
python3 - "$OUT/EthBeaconLightClient.abi.json" "$SRC/crates/eth-light-client-relayer/abi/EthBeaconLightClient.abi.json" <<'PY'
import json, sys
full = {f["name"]: f for f in json.load(open(sys.argv[1]))["functions"]}
slim = json.load(open(sys.argv[2]))["functions"]
for f in slim:
    g = full.get(f["name"])
    assert g, f"relayer ABI function {f['name']} missing from compiled ABI"
    assert [(i["name"], i["type"]) for i in f["inputs"]] == [(i["name"], i["type"]) for i in g["inputs"]], f["name"]
print(f"relayer ABI ({len(slim)} functions) is consistent with the compiled ABI ({len(full)})")
PY

sha256sum "$OUT/EthBeaconLightClient.tvc" "$OUT/EthBeaconLightClient.abi.json"
if [[ -x "$TVM_CLI" ]]; then
    tvm_json decode stateinit --tvc "$OUT/EthBeaconLightClient.tvc" 2>/dev/null | grep -E '"code_hash"' || true
fi
log "done: $OUT"
