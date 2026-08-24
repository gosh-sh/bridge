#!/usr/bin/env bash
# Copy the ETH light-client recursive `rotate` ZKHALO2VERIFYWITHVK *consumer*
# fixtures from eth-light-client-prover into tvm-sdk. Three-operand ABI only:
#   vk_cell            ← rotate_vk_blob.bin   (VkBlob v1 Base, Blake2b, 15 PI)
#   public_inputs_cell ← rotate_public_inputs.bin (15 × 32 B LE Fr:
#                          12 KZG accumulator limbs + [current, next, period])
#   proof_cell         ← rotate_proof.bin     (Blake2b SHPLONK, Hermez k=21)
#
# The root proof is a 2-to-1 recursion tree over 8 committee shards + a step
# snark (`examples/rotate_tree_n8.rs`). Producer SRS / config stay in
# eth-light-client-prover only.
#
# ⚠ NOT YET OPCODE-SOUND: the opcode runs a plain SHPLONK verify and does NOT
# pair instances[0..12] (the accumulator) against the embedded [s]·G2. Acceptance
# is necessary but not sufficient until the partner opcode decider extension
# lands (see eth-light-client-prover/examples/rotate_decider_check.rs).
#
# Regenerate the source triple first with:
#   cd eth-light-client-prover && \
#     EMIT_VKBLOB=1 cargo run --release --features aggregation --example rotate_tree_n8
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${ROOT}/eth-light-client-prover/fixtures/rotate_vkblob"
DST="${ROOT}/../tvm-sdk/tvm_vm/halo2_test_data/rotate_light_client"

if [[ ! -d "$SRC" ]]; then
  echo "error: missing $SRC (run eth-light-client-prover rotate_tree_n8 with EMIT_VKBLOB=1 first)" >&2
  exit 1
fi
if [[ ! -d "$(dirname "$DST")" ]]; then
  echo "error: missing tvm-sdk tree at $(dirname "$DST")" >&2
  exit 1
fi

mkdir -p "$DST"
install -m 0644 "$SRC/rotate_vk_blob.bin"             "$DST/rotate_vk_blob.bin"
install -m 0644 "$SRC/rotate_public_inputs.bin"       "$DST/rotate_public_inputs.bin"
install -m 0644 "$SRC/rotate_proof_blake2b.bin"       "$DST/rotate_proof.bin"
install -m 0644 "$SRC/rotate_base_circuit_params.json" "$DST/rotate_base_circuit_params.json"

echo "Synced rotate opcode triple to $DST"
echo "  vk_blob:        $(wc -c <"$DST/rotate_vk_blob.bin") bytes"
echo "  public_inputs:  $(wc -c <"$DST/rotate_public_inputs.bin") bytes"
echo "  proof:          $(wc -c <"$DST/rotate_proof.bin") bytes"
echo
echo "Verify byte-identity (must match eth-light-client-prover sidecars):"
sha256sum "$DST/rotate_vk_blob.bin" "$DST/rotate_public_inputs.bin" "$DST/rotate_proof.bin"
