#!/usr/bin/env bash
# Check a staged deposit prover outside the source tree, before it is
# published: its verification key must be the one the Acki Nacki bridge
# embeds, and it must prove the fixture deposit to the fixture's public
# inputs. A missing file or a prover the bridge would reject is caught
# here rather than on a user's machine after their USDC is in the bridge.
#
#   check_deposit_prover_bundle.sh <staged prover dir> <deposit-prover/fixtures/deposit_10proofs>
set -euo pipefail

dir=$(cd "${1:?prover dir}" && pwd)
fix=$(cd "${2:?fixtures dir}" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cd "$dir"   # the tools resolve configs/ and data/ against their working directory
for f in fetch_deposit_data export_blake2b_proof export_vk_blob configs/circuit_params.json data/kzg_params_18.srs; do
  [ -e "$f" ] || { echo "missing $f in $dir"; exit 1; }
done

./export_vk_blob --input "$fix/proof_00/input.json" --output "$work/vk.bin" \
  --config-out "$work/cfg.json" --degree 18 --max-data-byte-len 256 --max-log-num 20 --chain-id 11155111
want=$(sha256sum < "$fix/deposit_vk_blob.bin" | cut -d' ' -f1)
got=$(sha256sum < "$work/vk.bin" | cut -d' ' -f1)
[ "$want" = "$got" ] || { echo "verification key $got, the bridge embeds $want"; exit 1; }

./export_blake2b_proof --chain-id 11155111 --input "$fix/proof_00/input.json" \
  --proof-out "$work/proof.bin" --pubin-out "$work/pi.bin" \
  --degree 18 --max-data-byte-len 256 --max-log-num 20
cmp "$work/pi.bin" "$fix/proof_00/public_inputs.bin" || { echo "public inputs differ from the fixture"; exit 1; }
echo "deposit prover bundle OK: vk $got"
