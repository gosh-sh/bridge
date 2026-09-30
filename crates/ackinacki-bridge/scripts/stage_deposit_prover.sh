#!/usr/bin/env bash
# Lay out the deposit prover the way `ackinacki-bridge deposit` runs it:
# the three tools, configs/circuit_params.json and an empty data/ in one
# directory, which becomes the tools' working directory.
#
#   stage_deposit_prover.sh <out-dir> [--no-build]
#
# Builds deposit-prover's examples unless --no-build is given. The prover
# code is used as it is; only its build output is copied.
set -euo pipefail

out=${1:?usage: stage_deposit_prover.sh <out-dir> [--no-build]}
root=$(cd "$(dirname "$0")/../../.." && pwd)
prover="$root/deposit-prover"

if [ "${2:-}" != "--no-build" ]; then
  (cd "$prover" && cargo build --release --locked --examples)
fi

mkdir -p "$out/configs" "$out/data"
for bin in fetch_deposit_data export_blake2b_proof export_vk_blob; do
  install -m 0755 "$prover/target/release/examples/$bin" "$out/"
done
cp "$prover/configs/circuit_params.json" "$out/configs/"
echo "staged the deposit prover in $out (SRS goes to $out/data/kzg_params_18.srs)"
