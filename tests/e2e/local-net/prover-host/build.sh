#!/usr/bin/env bash
# The EVM side of a build on the prover host, after `docker build --target tools` has put
# every binary into /out (ci-entry build): the contracts for deploy.sh, built with the forge
# the image build fetched, with poseidon-solidity vendored the way the release does it.
# Runs in the plain rust container with /src (the commit, read-only), /cache and /out.
set -euo pipefail
POSEIDON_TGZ=https://registry.npmjs.org/poseidon-solidity/-/poseidon-solidity-0.0.5.tgz
POSEIDON_SHA256=e21e1b6f1cd0d95b321e9a1a69031b544b4966635eefdbeaacb0bd6ea9fb3517
for b in forge cast relayer aggregate-proof eth-lc-relayer deposit-relayer ackinacki-bridge \
  fetch_deposit_data export_vk_blob export_blake2b_proof downsize_srs export_step_vk_blob; do
  [ -x "/out/$b" ] || { echo "build.sh: /out/$b is missing: docker build --target tools did not run" >&2; exit 1; }
done
rm -rf /out/evm && cp -r /src/contracts/ethereum /out/evm
curl -fsSL -o /tmp/poseidon.tgz "$POSEIDON_TGZ"
echo "$POSEIDON_SHA256  /tmp/poseidon.tgz" | sha256sum -c - >/dev/null
mkdir -p /out/evm/node_modules/poseidon-solidity
tar -xzf /tmp/poseidon.tgz -C /out/evm/node_modules/poseidon-solidity --strip-components=1
(cd /out/evm && /out/forge install --no-git foundry-rs/forge-std >/dev/null && /out/forge build >/dev/null)
ls -la /out
