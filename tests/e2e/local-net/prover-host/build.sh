#!/usr/bin/env bash
# Builds what the Arbitrum Sepolia e2e runs on the prover host, inside the build container the
# dispatcher (ci-entry build) starts: the source is /src (read-only), cargo and rustup
# caches are /cache, the binaries land in /out. Each workspace keeps its own target
# directory under /cache so the next commit builds incrementally.
set -euo pipefail
# The image's rustup installs each rust-toolchain.toml pin into the cache on first use.
export CARGO_HOME=/cache/cargo RUSTUP_HOME=/cache/rustup CARGO_NET_GIT_FETCH_WITH_CLI=true
export PATH=/usr/local/cargo/bin:$PATH
build() { # <workspace dir> <cargo args...>
  local dir=$1; shift
  local name; name=$(echo "$dir" | tr '/' '_')
  echo "== $dir: cargo build $*"
  (cd "/src/$dir" && CARGO_TARGET_DIR="/cache/target/$name" cargo build --release --locked "$@")
}
# ackinacki-bridge with dev-unfixed-bridge: MIN_BRIDGE_VERSION is unset until the release that
# sets it, and without the feature the CLI refuses every bridge, this run's fresh one included.
build crates/bridge-prover-libraries -p bridge-relayer-daemon --bin relayer -p bridge-prover-lib --bin compute_bridge_anchors \
  -p ackinacki-bridge --bin ackinacki-bridge --features ackinacki-bridge/dev-unfixed-bridge
build crates/bridge-evm-aggregator --bin aggregate-proof
build crates/deposit-relayer-daemon --bin deposit-relayer
build deposit-prover --example fetch_deposit_data --example export_vk_blob --example export_blake2b_proof \
  --example downsize_srs
# The Sepolia light client's relayer and its step prover.
build crates/eth-light-client-relayer --features live-submit --bin eth-lc-relayer
build eth-light-client-prover --example export_step_vk_blob
t=/cache/target
install -m 0755 "$t/crates_bridge-prover-libraries/release/relayer" "$t/crates_bridge-prover-libraries/release/compute_bridge_anchors" \
  "$t/crates_bridge-prover-libraries/release/ackinacki-bridge" "$t/crates_bridge-evm-aggregator/release/aggregate-proof" \
  "$t/crates_deposit-relayer-daemon/release/deposit-relayer" /out/
for e in fetch_deposit_data export_vk_blob export_blake2b_proof downsize_srs; do
  install -m 0755 "$t/deposit-prover/release/examples/$e" /out/
done
install -m 0755 "$t/crates_eth-light-client-relayer/release/eth-lc-relayer" \
  "$t/eth-light-client-prover/release/examples/export_step_vk_blob" /out/
install -m 0644 /src/deposit-prover/configs/circuit_params.json /out/

# Foundry and the Solidity tree the deploy and deposit jobs run, both pinned.
FOUNDRY=v1.7.1
FOUNDRY_SHA256=cf7e688ed0c4c48adffca788b496076e31060b67ac5afe1e43dbb5499c20c88b
POSEIDON_TGZ=https://registry.npmjs.org/poseidon-solidity/-/poseidon-solidity-0.0.5.tgz
POSEIDON_SHA256=e21e1b6f1cd0d95b321e9a1a69031b544b4966635eefdbeaacb0bd6ea9fb3517
f=/cache/foundry/$FOUNDRY
if [ ! -x "$f/forge" ]; then
  mkdir -p "$f"
  curl -fsSL -o "$f.tgz" "https://github.com/foundry-rs/foundry/releases/download/$FOUNDRY/foundry_${FOUNDRY}_linux_amd64.tar.gz"
  echo "$FOUNDRY_SHA256  $f.tgz" | sha256sum -c - >/dev/null
  tar -xzf "$f.tgz" -C "$f" && rm -f "$f.tgz"
fi
install -m 0755 "$f/forge" "$f/cast" /out/
rm -rf /out/evm && cp -r /src/contracts/ethereum /out/evm
curl -fsSL -o /tmp/poseidon.tgz "$POSEIDON_TGZ"
echo "$POSEIDON_SHA256  /tmp/poseidon.tgz" | sha256sum -c - >/dev/null
mkdir -p /out/evm/node_modules/poseidon-solidity
tar -xzf /tmp/poseidon.tgz -C /out/evm/node_modules/poseidon-solidity --strip-components=1
(cd /out/evm && /out/forge install --no-git foundry-rs/forge-std >/dev/null && /out/forge build >/dev/null)
ls -la /out
