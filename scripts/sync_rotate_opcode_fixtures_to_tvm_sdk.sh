#!/usr/bin/env bash
# Copy the ETH light-client `rotate` ZKHALO2VERIFYWITHVK *consumer* fixtures
# from eth-light-client-prover into tvm-sdk. Three-operand ABI only:
#   vk_cell            ← rotate_vk_blob.bin   (VkBlob v1 Base, Blake2b, 17 PI,
#                          byte 11 accumulator_limbs = 12)
#   public_inputs_cell ← rotate_public_inputs.bin (17 × 32 B LE Fr:
#                          12 KZG accumulator limbs + [next_commit, state_root_hi,
#                          state_root_lo, attested_slot, gloas_fork_slot])
#   proof_cell         ← rotate_proof.bin     (Blake2b SHPLONK, Hermez k=21)
#
# The proof wraps the committee circuit (`src/committee_table.rs`, k=18) in one
# aggregation (`examples/rotate_committee.rs`). Its validity rests on the
# accumulator: the opcode pairs instances[0..12] against the embedded [s]·G2
# because of byte 11 (tvm-sdk#284).
#
# Regenerate the source files first with:
#   cd eth-light-client-prover && ROTATE_OUT=fixtures/rotate_vkblob \
#     OUTER_SRS=../params/kzg_bn254_21.srs ROTATE_UPDATE_PATH=<update.json> \
#     cargo run --release --features aggregation --example rotate_committee
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${ROOT}/eth-light-client-prover/fixtures/rotate_vkblob"
DST="${ROOT}/../tvm-sdk/tvm_vm/halo2_test_data/rotate_light_client"

if [[ ! -d "$SRC" ]]; then
  echo "error: missing $SRC (run eth-light-client-prover rotate_committee first)" >&2
  exit 1
fi
if [[ ! -d "$(dirname "$DST")" ]]; then
  echo "error: missing tvm-sdk tree at $(dirname "$DST")" >&2
  exit 1
fi

ACC_LIMBS=$(python3 -c 'import sys; b=open(sys.argv[1],"rb").read(); sys.exit(0 if len(b)>=12 and b[11]==12 else 1)' "$SRC/rotate_vk_blob.bin" && echo 12 || echo bad)
if [[ "$ACC_LIMBS" != "12" ]]; then
  echo "error: $SRC/rotate_vk_blob.bin accumulator_limbs (byte 11) != 12; refusing to sync" >&2
  echo "  a 0 here would ship an unsound rotate fixture (opcode skips the KZG decider)" >&2
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
