#!/usr/bin/env bash
# Gate: a recursive-aggregation VkBlob MUST carry accumulator_limbs = 12
# (VkBlob header byte 11). If a re-emit ever ships byte 11 = 0, ZKHALO2VERIFYWITHVK
# accepts the rotate proof without pairing the 12-limb KZG accumulator — silently.
#
# Checks:
#   1. fixtures/rotate_vkblob/rotate_vk_blob.bin  — magic/v1/Blake2b/Base, byte 11 == 12
#   2. fixtures/step_vkblob/step_vk_blob.bin      — byte 11 == 0 (non-aggregation)
#   3. rotate fixture sha256 matches its sidecar
#   4. contracts/an/EthBeaconLightClient.sol ROTATE_VK_BLOB hex == fixture
#      (and therefore also has byte 11 == 12)
#   5. Negative: a byte-11-cleared copy of the rotate blob must NOT pass (1)
#
# Usage:
#   ./scripts/check_rotate_vkblob_accumulator.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ROTATE_BLOB="${ROOT}/eth-light-client-prover/fixtures/rotate_vkblob/rotate_vk_blob.bin"
ROTATE_PIN="${ROOT}/eth-light-client-prover/fixtures/rotate_vkblob/rotate_vk_blob.bin.sha256"
STEP_BLOB="${ROOT}/eth-light-client-prover/fixtures/step_vkblob/step_vk_blob.bin"
# The contract source, not a patch of it: the whole-file acki-nacki patch was
# removed (it recreated the deployed light client from this repo's variant and
# would have unwired the sink). The literal it used to carry lives here.
SOURCE="${ROOT}/contracts/an/EthBeaconLightClient.sol"

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  cat <<'EOF'
Assert rotate VkBlob header byte 11 (accumulator_limbs) is 12, the step blob
is 0, the sha256 sidecar matches, and the AN-side patch embeds the same bytes.

A rotate blob with byte 11 = 0 would be accepted by the opcode without the
KZG-accumulator pairing — this gate is what makes that failure noisy.
EOF
  exit 0
fi

python3 - "$ROTATE_BLOB" "$ROTATE_PIN" "$STEP_BLOB" "$SOURCE" <<'PY'
import hashlib, re, sys, tempfile
from pathlib import Path

MAGIC = b"VKBLOB\x00\x00"
ROTATE_ACC = 12
STEP_ACC = 0

rotate_path, pin_path, step_path, source_path = map(Path, sys.argv[1:5])


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


if not rotate_path.is_file():
    fail(f"missing {rotate_path}")
if not step_path.is_file():
    fail(f"missing {step_path}")
if not pin_path.is_file():
    fail(f"missing {pin_path}")
if not source_path.is_file():
    fail(f"missing {source_path}")

rotate = rotate_path.read_bytes()
step = step_path.read_bytes()
check_header(rotate, want_acc=ROTATE_ACC, label="rotate_vk_blob.bin")
check_header(step, want_acc=STEP_ACC, label="step_vk_blob.bin")

pin_line = pin_path.read_text().splitlines()[0]
pinned_hash = pin_line.split()[0]
actual_hash = hashlib.sha256(rotate).hexdigest()
if pinned_hash != actual_hash:
    fail(f"rotate sha256 sidecar mismatch: pin {pinned_hash} actual {actual_hash}")

# The contract must embed the exact fixture (catches a re-emit that updated the
# fixture but not contracts/an/EthBeaconLightClient.sol).
source_text = source_path.read_text()
# The rotate literal sits after the ROTATE_VK_BLOB declaration; collect hex"
# chunks until the semicolon that closes it. The file is a unified diff, so
# lines are prefixed with '+'.
idx = source_text.find("bytes constant ROTATE_VK_BLOB")
if idx < 0:
    fail("contract has no `bytes constant ROTATE_VK_BLOB`")
tail = source_text[idx:]
end = tail.find(";")
if end < 0:
    fail("ROTATE_VK_BLOB literal is not semicolon-terminated")
hexes = re.findall(r'hex"([0-9a-f]*)"', tail[:end])
if not hexes:
    fail("ROTATE_VK_BLOB has no hex\"...\" chunks")
embedded = bytes.fromhex("".join(hexes))
if embedded != rotate:
    fail(
        "contract ROTATE_VK_BLOB hex != fixtures/rotate_vkblob/rotate_vk_blob.bin "
        f"(contract {len(embedded)} B sha256 {hashlib.sha256(embedded).hexdigest()[:16]}…, "
        f"fixture {len(rotate)} B sha256 {actual_hash[:16]}…)"
    )
check_header(embedded, want_acc=ROTATE_ACC, label="contract ROTATE_VK_BLOB")

# Negative: clearing byte 11 must be rejected. This is the failure mode the
# reviewer flagged — a re-emit that forgets the flag.
tampered = bytearray(rotate)
tampered[11] = 0
if header_ok(bytes(tampered), want_acc=ROTATE_ACC):
    fail("negative probe: byte-11-cleared blob incorrectly passed the header check")

print(
    f"OK: rotate accumulator_limbs=12 ({len(rotate)} B, sha256={actual_hash[:16]}…); "
    f"step accumulator_limbs=0 ({len(step)} B); contract hex matches fixture; "
    "byte-11-cleared probe rejected"
)
PY
