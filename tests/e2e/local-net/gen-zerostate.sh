#!/usr/bin/env bash
# Controller-side zerostate generation for the bridge local-net e2e: place this
# bridge commit's contracts/an/* into the pinned acki-nacki checkout, compile the
# giver/test contracts, bake the zerostate (bridge account 0:1a1a…, its owner key,
# keeper keys, docker/.env). Run by .woodpecker/local-net-e2e.yaml inside the CI
# image, and usable by hand with the same variables.
#
# Required env:
#   ACKI_DIR          acki-nacki checkout at the pinned commit (its contracts/compiler
#                     binaries are macOS builds; this script fetches the x86_64 releases)
#   BRIDGE_REPO       gosh-sh/bridge clone that contains BRIDGE_SHA
#   BRIDGE_SHA        bridge commit whose contracts/an/place.json is placed
#   BRIDGE_ZS_L1_BRIDGE  L1 bridge address baked as the trusted pair (any 20-byte hex
#                     for a local network; the withdraw-only test never uses it)
# Optional:
#   HELPERS_DIR       dir with prebuilt node-helper + zerostate-helper (CI copies them
#                     out of the all-bin image); default ACKI_DIR/.bin
#   TVM_CLI_VERSION   tvmlabs/tvm-sdk release (tvm-cli + tvm-debugger), default v3.0.4-rc.an
#   TVM_CLI_SHA256 / TVM_DEBUGGER_SHA256  sha256 of the two x86_64 tarballs of that release
#   SOLD_VERSION / SOLD_SHA256  gosh-sh/TVM-Solidity-Compiler release + sha256 of its tarball
#   BRIDGE_ZS_L1_CHAIN_ID  default 11155111 (zerostate_init.py default)
#   DISABLE_BLACKLIST     presence skips the blacklist indexer (acki-nacki ci_test does the same)
set -Eeuo pipefail
ACKI=${ACKI_DIR:?ACKI_DIR}; BRIDGE=${BRIDGE_REPO:?BRIDGE_REPO}; BSHA=${BRIDGE_SHA:?BRIDGE_SHA}
: "${BRIDGE_ZS_L1_BRIDGE:?BRIDGE_ZS_L1_BRIDGE}"
HELPERS=${HELPERS_DIR:-$ACKI/.bin}
TVM_VER=${TVM_CLI_VERSION:-v3.0.4-rc.an}
TVM_CLI_SHA=${TVM_CLI_SHA256:-d5c0427237a4ccf06e7ec352ecda31c51f2be6f44bb680a36b5d8375db355cd0}
TVM_DBG_SHA=${TVM_DEBUGGER_SHA256:-328d63ea196b5a79f59dee790c5211b84eca0bf750fe2cc22b034da72015e727}
SOLD_VER=${SOLD_VERSION:-gosh_0.81.0}
SOLD_SHA=${SOLD_SHA256:-79b43aab6ae2fdc35c1cb3b486453aa2cf4f2718c58b69ccefab1122c704853c}
cd "$ACKI"

echo "== helpers from $HELPERS -> target/release"
[ -x "$HELPERS/node-helper" ] && [ -x "$HELPERS/zerostate-helper" ] || { echo "!! node-helper/zerostate-helper missing in $HELPERS"; exit 1; }
mkdir -p target/release contracts/compiler
cp -f "$HELPERS/node-helper" "$HELPERS/zerostate-helper" target/release/
chmod +x target/release/node-helper target/release/zerostate-helper

echo "== toolchain: tvm-cli/tvm-debugger $TVM_VER, sold $SOLD_VER (x86_64 release assets, sha256-checked)"
fetch_checked() { # url sha256 -> extracts into contracts/compiler
  local tmp; tmp=$(mktemp)
  curl -fsSL -o "$tmp" "$1"
  echo "$2  $tmp" | sha256sum -c - >/dev/null
  tar xz -f "$tmp" -C contracts/compiler && rm -f "$tmp"
}
fetch_checked "https://github.com/tvmlabs/tvm-sdk/releases/download/$TVM_VER/tvm-cli-${TVM_VER#v}-x86_64.tar.gz" "$TVM_CLI_SHA"
fetch_checked "https://github.com/tvmlabs/tvm-sdk/releases/download/$TVM_VER/tvm-debugger-${TVM_VER#v}-x86_64.tar.gz" "$TVM_DBG_SHA"
fetch_checked "https://github.com/gosh-sh/TVM-Solidity-Compiler/releases/download/$SOLD_VER/sold_${SOLD_VER}_linux_x86_64.tar.gz" "$SOLD_SHA"
chmod +x contracts/compiler/*
export PATH="$ACKI/contracts/compiler:$PATH"

# contracts/scripts/bridge_contracts.py pins one bridge commit (BRIDGE_COMMIT) and
# generate_zerostate.py places the files from that pin at import time, so any
# placement made beforehand is overwritten. Move the pin to the commit under test
# in this disposable checkout (the module's own instruction for shipping a contract
# change), then place from the local clone (BRIDGE_REPO).
echo "== move acki-nacki's bridge pin to $BSHA and place contracts/an/* from $BRIDGE"
sed -i -E "s/^BRIDGE_COMMIT = \"[0-9a-f]{40}\"/BRIDGE_COMMIT = \"$BSHA\"/" contracts/scripts/bridge_contracts.py
grep -q "^BRIDGE_COMMIT = \"$BSHA\"" contracts/scripts/bridge_contracts.py || { echo "!! could not move BRIDGE_COMMIT in contracts/scripts/bridge_contracts.py"; exit 1; }
export BRIDGE_REPO="$BRIDGE"
python3 contracts/scripts/bridge_contracts.py
want=$(git -C "$BRIDGE" show "$BSHA:contracts/an/0.80.0_compiled/exchange/eccUSDCBridge.tvc" | sha256sum | cut -c1-64)
have=$(sha256sum contracts/0.80.0_compiled/exchange/eccUSDCBridge.tvc | cut -c1-64)
[ "$want" = "$have" ] || { echo "!! placed eccUSDCBridge.tvc differs from $BSHA"; exit 1; }

echo "== compile_contracts (giver + test contracts)"
make compile_contracts >/dev/null

echo "== generate_zerostate (BRIDGE_ZS_L1_BRIDGE=$BRIDGE_ZS_L1_BRIDGE, DISABLE_BLACKLIST=${DISABLE_BLACKLIST-unset})"
python3 contracts/scripts/generate_zerostate.py > zerostate-gen.log 2>&1 || { tail -40 zerostate-gen.log; exit 1; }
[ "$(sha256sum contracts/0.80.0_compiled/exchange/eccUSDCBridge.tvc | cut -c1-64)" = "$want" ] || { echo "!! eccUSDCBridge.tvc was replaced during generation"; exit 1; }
# tests/dexdo/up.sh (DEXDO_SKIP_GEN=1) insists on the DEX seed-notes file even when
# no notes are baked (DEX_TEST_NOTES_CNT unset/0): an empty list is the truthful value.
[ -f config/dex_test_notes.keys.json ] || echo '[]' > config/dex_test_notes.keys.json
for f in config/zerostate config/block_manager1_signing.keys.json config/USDCBridge.keys.json config/LicenseRoot.keys.json docker/.env; do
  [ -s "$f" ] || { echo "!! $f missing after generation"; exit 1; }
done
echo "== zerostate ready: $(wc -c < config/zerostate) bytes, bridge owner key config/USDCBridge.keys.json, eccUSDCBridge.tvc $have"
