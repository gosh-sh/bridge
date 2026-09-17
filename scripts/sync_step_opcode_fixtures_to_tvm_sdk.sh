#!/usr/bin/env bash
# Copy the ETH light-client `step` ZKHALO2VERIFYWITHVK *consumer* fixtures from
# eth-light-client-prover into tvm-sdk. The three-operand ABI only:
#   vk_cell            ← step_vk_blob.bin   (VkBlob v1 Base, Blake2b, 10 PI)
#   public_inputs_cell ← step_public_inputs.bin (10 × 32 B LE Fr; 2-level commit)
#   proof_cell         ← step_proof.bin     (Blake2b SHPLONK, Hermez k=19)
# Producer SRS / config archive stay in eth-light-client-prover only.
#
# Regenerate the source triple first with:
#   cd eth-light-client-prover && cargo run --release --example export_step_vk_blob
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${ROOT}/eth-light-client-prover/fixtures/step_vkblob"
DST="${ROOT}/../tvm-sdk/tvm_vm/halo2_test_data/step_light_client"

if [[ ! -d "$SRC" ]]; then
  echo "error: missing $SRC (run eth-light-client-prover export_step_vk_blob first)" >&2
  exit 1
fi
if [[ ! -d "$(dirname "$DST")" ]]; then
  echo "error: missing tvm-sdk tree at $(dirname "$DST")" >&2
  exit 1
fi

mkdir -p "$DST"
install -m 0644 "$SRC/step_vk_blob.bin"        "$DST/step_vk_blob.bin"
install -m 0644 "$SRC/step_public_inputs.bin"  "$DST/step_public_inputs.bin"
install -m 0644 "$SRC/step_proof_blake2b.bin"  "$DST/step_proof.bin"
install -m 0644 "$SRC/step_base_circuit_params.json" "$DST/step_base_circuit_params.json"

echo "Synced step opcode triple to $DST"
echo "  vk_blob:        $(wc -c <"$DST/step_vk_blob.bin") bytes"
echo "  public_inputs:  $(wc -c <"$DST/step_public_inputs.bin") bytes"
echo "  proof:          $(wc -c <"$DST/step_proof.bin") bytes"
echo
echo "Verify byte-identity (must match eth-light-client-prover sidecars):"
sha256sum "$DST/step_vk_blob.bin" "$DST/step_public_inputs.bin" "$DST/step_proof.bin"
