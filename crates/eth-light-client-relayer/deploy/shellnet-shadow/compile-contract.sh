#!/usr/bin/env bash
# Compile a private deployment of the exchange light client with pinned Linux `sold`.
# and check that the embedded step VK_BLOB equals the prover fixture.
#
# The `gosh.zkhalo2VerifyWithVK` builtin is in TVM-Solidity-Compiler master
# and in releases >= gosh_0.81.1 (Linux x86_64 tarballs). Output goes to
# $ROOT/contracts.

# shellcheck disable=SC1091
source "$(dirname "$0")/lib.sh"
need python3

SOLD_VERSION="${SOLD_VERSION:-gosh_0.81.1}"
SOLD_TGZ_SHA="${SOLD_TGZ_SHA:-4931c4f14e5454ed6d95a5b805246fda94f16d90eae56e956e444e8482532479}"
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
( cd "$SRC" && python3 scripts/embed_step_vk_blob.py --check contracts/an/exchange/EthBeaconLightClient.sol )

shadow_src="$(mktemp -d)"
trap 'rm -rf "$shadow_src"' EXIT
cp -R "$SRC_SOL/." "$shadow_src/"
python3 - "$shadow_src/exchange/EthBeaconLightClient.sol" <<'PYSHADOW'
from pathlib import Path
import sys

path = Path(sys.argv[1])
source = path.read_text()
start = source.index("    constructor() accept {")
end = source.index("    modifier accept()", start)
params = source.split("    function initialize(", 1)[1].split(") public accept {", 1)[0]
constructor = f"""    constructor({params}) accept {{
        require(pubkey != 0, ERR_ZERO_PUBKEY);
        _ownerPubkey = pubkey;
        _l1ChainId = l1ChainId;
        _setOnlyCommittee(bootstrapCommittee, bootstrapPeriod);
    }}

"""
source = source[:start] + constructor + source[end:]
source = source.replace('import "./eccBridge.sol";\n', "")
for name in ("_notifySink", "_forgetSink"):
    start = source.index(f"    function {name}(")
    end = source.index("\n    }", start) + len("\n    }")
    source = source[:start] + f"    function {name}(uint256) private pure {{}}" + source[end:]
source = source.replace(
    "return (_l1ChainId, USDC_BRIDGE_ADDRESS, _ownerPubkey);",
    "return (_l1ChainId, address(0), _ownerPubkey);",
)
path.write_text(source)
PYSHADOW

log "compiling the private EthBeaconLightClient"
( cd "$shadow_src/exchange" && "$SOLD" --tvm-version gosh --base-path .. EthBeaconLightClient.sol -o "$OUT" )
rm -f "$OUT/EthBeaconLightClient.debug.json" "$OUT/EthBeaconLightClient.code"

python3 - "$OUT/EthBeaconLightClient.abi.json" "$SRC/contracts/an/0.81.1_compiled/exchange/EthBeaconLightClient.abi.json" <<'PYABI'
import json
import sys

shadow, production = [
    {f["name"]: f for f in json.load(open(path))["functions"]} for path in sys.argv[1:]
]
for name in production.keys() - {"constructor", "initialize"}:
    assert shadow.get(name) == production[name], f"incompatible shadow ABI for {name}"
print("Shadow ABI matches the exchange light client")
PYABI

sha256sum "$OUT/EthBeaconLightClient.tvc" "$OUT/EthBeaconLightClient.abi.json"
if [[ -x "$TVM_CLI" ]]; then
    tvm_json decode stateinit --tvc "$OUT/EthBeaconLightClient.tvc" 2>/dev/null | grep -E '"code_hash"' || true
fi
log "done: $OUT"
