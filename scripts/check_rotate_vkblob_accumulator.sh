#!/usr/bin/env bash
# Gate: rotate and step VkBlobs stay tied to the contract that verifies them.
#
# Rotate: a recursive-aggregation VkBlob MUST carry accumulator_limbs = 12
# (VkBlob header byte 11). If a re-emit ever ships byte 11 = 0, ZKHALO2VERIFYWITHVK
# accepts the rotate proof without pairing the 12-limb KZG accumulator — silently.
#
# Step: the blob embedded as `VK_BLOB` must equal the fixture. Same circuit
# shape with different key material is a silent miss — tvm-sdk#284 shipped a
# step fixture (sha256 bd108c08…) that matched this contract for the first
# 11 813 bytes and then diverged, so the opcode test covered a step circuit
# that is not the deployed one. Rotate already had this check; step did not.
#
# Checks:
#   1. fixtures/rotate_vkblob/rotate_vk_blob.bin  — magic/v1/Blake2b/Base, byte 11 == 12
#   2. fixtures/step_vkblob/step_vk_blob.bin      — byte 11 == 0 (non-aggregation)
#   3. both fixture sha256 sidecars match
#   4. contracts/an/EthBeaconLightClient.sol ROTATE_VK_BLOB hex == rotate fixture
#   5. contracts/an/EthBeaconLightClient.sol VK_BLOB hex == step fixture
#   6. Negative: a byte-11-cleared copy of the rotate blob must NOT pass (1)
#   7. If ../tvm-sdk/tvm_vm/halo2_test_data/step_light_client/step_vk_blob.bin
#      exists, it must equal the step fixture (the sync script's job)
#
# Usage:
#   ./scripts/check_rotate_vkblob_accumulator.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ROTATE_BLOB="${ROOT}/eth-light-client-prover/fixtures/rotate_vkblob/rotate_vk_blob.bin"
ROTATE_PIN="${ROOT}/eth-light-client-prover/fixtures/rotate_vkblob/rotate_vk_blob.bin.sha256"
STEP_BLOB="${ROOT}/eth-light-client-prover/fixtures/step_vkblob/step_vk_blob.bin"
STEP_PIN="${ROOT}/eth-light-client-prover/fixtures/step_vkblob/step_vk_blob.bin.sha256"
SOURCE="${ROOT}/contracts/an/EthBeaconLightClient.sol"
SDK_STEP="${ROOT}/../tvm-sdk/tvm_vm/halo2_test_data/step_light_client/step_vk_blob.bin"

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  sed -n '2,23p' "$0" | sed 's/^# \{0,1\}//'
  exit 0
fi

python3 - "$ROTATE_BLOB" "$ROTATE_PIN" "$STEP_BLOB" "$STEP_PIN" "$SOURCE" "$SDK_STEP" <<'PY'
import hashlib, re, sys
from pathlib import Path

MAGIC = b"VKBLOB\x00\x00"
ROTATE_ACC = 12
STEP_ACC = 0

rotate_path, rotate_pin, step_path, step_pin, source_path, sdk_step_path = map(
    Path, sys.argv[1:7]
)


def fail(msg: str) -> None:
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def check_header(blob: bytes, *, want_acc: int, label: str) -> None:
    if len(blob) < 16:
        fail(f"{label}: blob too short ({len(blob)} B)")
    if blob[:8] != MAGIC:
        fail(f"{label}: bad magic {blob[:8]!r}")
    if blob[8] != 1:
        fail(f"{label}: version byte {blob[8]} != 1 (Base)")
    if blob[9] != 0:
        fail(f"{label}: transcript byte {blob[9]} != 0 (Blake2b)")
    if blob[10] != 0:
        fail(f"{label}: shape byte {blob[10]} != 0 (Base)")
    if blob[11] != want_acc:
        fail(
            f"{label}: accumulator_limbs (byte 11) = {blob[11]}, want {want_acc}. "
            "A rotate blob with 0 is accepted by the opcode WITHOUT pairing the "
            "KZG accumulator — inner proofs would be unchecked."
        )


def header_ok(blob: bytes, *, want_acc: int) -> bool:
    return (
        len(blob) >= 16
        and blob[:8] == MAGIC
        and blob[8] == 1
        and blob[9] == 0
        and blob[10] == 0
        and blob[11] == want_acc
    )


def check_pin(blob: bytes, pin_path: Path, label: str) -> str:
    if not pin_path.is_file():
        fail(f"missing {pin_path}")
    pinned = pin_path.read_text().splitlines()[0].split()[0]
    actual = hashlib.sha256(blob).hexdigest()
    if pinned != actual:
        fail(f"{label} sha256 sidecar mismatch: pin {pinned} actual {actual}")
    return actual


def embedded_literal(source: str, decl: str) -> bytes:
    idx = source.find(decl)
    if idx < 0:
        fail(f"contract has no `{decl}`")
    tail = source[idx:]
    end = tail.find(";")
    if end < 0:
        fail(f"{decl} literal is not semicolon-terminated")
    hexes = re.findall(r'hex"([0-9a-f]*)"', tail[:end])
    if not hexes:
        fail(f"{decl} has no hex\"...\" chunks")
    return bytes.fromhex("".join(hexes))


for p in (rotate_path, step_path, source_path):
    if not p.is_file():
        fail(f"missing {p}")

rotate = rotate_path.read_bytes()
step = step_path.read_bytes()
check_header(rotate, want_acc=ROTATE_ACC, label="rotate_vk_blob.bin")
check_header(step, want_acc=STEP_ACC, label="step_vk_blob.bin")
rotate_hash = check_pin(rotate, rotate_pin, "rotate")
step_hash = check_pin(step, step_pin, "step")

source_text = source_path.read_text()
embedded_rotate = embedded_literal(source_text, "bytes constant ROTATE_VK_BLOB")
if embedded_rotate != rotate:
    fail(
        "contract ROTATE_VK_BLOB hex != fixtures/rotate_vkblob/rotate_vk_blob.bin "
        f"(contract {len(embedded_rotate)} B sha256 "
        f"{hashlib.sha256(embedded_rotate).hexdigest()[:16]}…, "
        f"fixture {len(rotate)} B sha256 {rotate_hash[:16]}…)"
    )
check_header(embedded_rotate, want_acc=ROTATE_ACC, label="contract ROTATE_VK_BLOB")

embedded_step = embedded_literal(source_text, "bytes constant VK_BLOB")
if embedded_step != step:
    fail(
        "contract VK_BLOB hex != fixtures/step_vkblob/step_vk_blob.bin "
        f"(contract {len(embedded_step)} B sha256 "
        f"{hashlib.sha256(embedded_step).hexdigest()[:16]}…, "
        f"fixture {len(step)} B sha256 {step_hash[:16]}…). "
        "Same circuit params with different key material is the failure "
        "tvm-sdk#284 shipped: run scripts/embed_step_vk_blob.py"
    )
check_header(embedded_step, want_acc=STEP_ACC, label="contract VK_BLOB")

tampered = bytearray(rotate)
tampered[11] = 0
if header_ok(bytes(tampered), want_acc=ROTATE_ACC):
    fail("negative probe: byte-11-cleared blob incorrectly passed the header check")

sdk_note = "tvm-sdk step fixture not present (optional)"
if sdk_step_path.is_file():
    sdk = sdk_step_path.read_bytes()
    if sdk != step:
        fail(
            "tvm-sdk step_vk_blob.bin != fixtures/step_vkblob/step_vk_blob.bin "
            f"(sdk sha256 {hashlib.sha256(sdk).hexdigest()[:16]}…, "
            f"fixture {step_hash[:16]}…). "
            "Run scripts/sync_step_opcode_fixtures_to_tvm_sdk.sh — this is how "
            "tvm-sdk#284 tested a step circuit that is not the deployed one."
        )
    sdk_note = f"tvm-sdk step fixture matches ({step_hash[:16]}…)"

print(
    f"OK: rotate accumulator_limbs=12 ({len(rotate)} B, sha256={rotate_hash[:16]}…); "
    f"step accumulator_limbs=0 ({len(step)} B, sha256={step_hash[:16]}…); "
    "contract ROTATE_VK_BLOB and VK_BLOB match fixtures; "
    f"byte-11-cleared probe rejected; {sdk_note}"
)
PY
