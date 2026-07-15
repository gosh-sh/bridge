#!/usr/bin/env bash
# Copy deposit ZKHALO2VERIFYWITHVK *consumer* fixtures from deposit-prover
# into tvm-sdk. Producer witnesses (input.json), SRS, and EthCircuitParams
# archive stay in deposit-prover only.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${ROOT}/deposit-prover/fixtures/deposit_10proofs"
DST="${ROOT}/../tvm-sdk/tvm_vm/halo2_test_data/deposit_10proofs"

if [[ ! -d "$SRC" ]]; then
  echo "error: missing $SRC (run deposit-prover export_deposit_proof_set first)" >&2
  exit 1
fi
if [[ ! -d "$(dirname "$DST")" ]]; then
  echo "error: missing tvm-sdk tree at $(dirname "$DST")" >&2
  exit 1
fi

mkdir -p "$DST"
install -m 0644 "$SRC/deposit_vk_blob.bin" "$DST/deposit_vk_blob.bin"

for i in $(seq 0 9); do
  d=$(printf 'proof_%02d' "$i")
  mkdir -p "$DST/$d"
  install -m 0644 "$SRC/$d/public_inputs.bin" "$DST/$d/public_inputs.bin"
  install -m 0644 "$SRC/$d/proof.bin" "$DST/$d/proof.bin"
done

echo "Synced opcode triple to $DST"
echo "  vk_blob: $(wc -c <"$DST/deposit_vk_blob.bin") bytes"
echo "  proofs:  proof_00 .. proof_09 (public_inputs + proof only)"
