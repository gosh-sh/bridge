#!/usr/bin/env python3
"""Rewrite the `ROTATE_VK_BLOB` literal in an AN-side `EthBeaconLightClient.sol`
from the eth-light-client-prover rotate fixture, and update the sha256 in the
comment above it.

Mirror of `embed_step_vk_blob.py` for the recursive light-client `rotate` root.
Every VK rotation must reach three places: the fixture in this repo
(`eth-light-client-prover/fixtures/rotate_vkblob/`), the `tvm-sdk` opcode fixtures
(`scripts/sync_rotate_opcode_fixtures_to_tvm_sdk.sh`), and the blob embedded in
`EthBeaconLightClient.sol`. Hand-editing kilobytes of hex is how blobs drift;
hence this script.

    scripts/embed_rotate_vk_blob.py ../acki-nacki/contracts/exchange/EthBeaconLightClient.sol

Pass --check to verify without writing (exit 1 on mismatch).
"""

import argparse
import hashlib
import pathlib
import re
import sys

BLOB = pathlib.Path(__file__).resolve().parents[1] / (
    "eth-light-client-prover/fixtures/rotate_vkblob/rotate_vk_blob.bin"
)
BYTES_PER_LINE = 60  # 120 hex chars, matching the existing formatting
LITERAL_RE = re.compile(r'(bytes constant ROTATE_VK_BLOB\s*=\s*)((?:\s*hex"[0-9a-f]*")+)(;)')
SHA_RE = re.compile(r"(//\s+sha256 )([0-9a-f]{64})")


def render(blob: bytes, indent: str = " " * 8) -> str:
    lines = [
        f'{indent}hex"{blob[i : i + BYTES_PER_LINE].hex()}"'
        for i in range(0, len(blob), BYTES_PER_LINE)
    ]
    return "\n" + "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("target", type=pathlib.Path, help="path to EthBeaconLightClient.sol")
    ap.add_argument("--check", action="store_true", help="verify only, do not write")
    args = ap.parse_args()

    blob = BLOB.read_bytes()
    if len(blob) < 16 or blob[:8] != b"VKBLOB\x00\x00":
        print(f"error: {BLOB} is not a Base v1 VkBlob", file=sys.stderr)
        return 2
    acc = blob[11]
    if acc != 12:
        print(
            f"error: {BLOB} accumulator_limbs (byte 11) = {acc}, want 12.\n"
            "  Refusing to embed: a rotate blob with byte 11 = 0 is accepted by\n"
            "  ZKHALO2VERIFYWITHVK without pairing the KZG accumulator.",
            file=sys.stderr,
        )
        return 2
    digest = hashlib.sha256(blob).hexdigest()
    print(f"fixture: {BLOB}\n  {len(blob)} bytes, sha256 {digest}, accumulator_limbs={acc}")

    src = args.target.read_text()
    literal = LITERAL_RE.search(src)
    if literal is None:
        print(f"error: no `bytes constant ROTATE_VK_BLOB` literal in {args.target}", file=sys.stderr)
        return 2

    embedded = bytes.fromhex("".join(re.findall(r'hex"([0-9a-f]*)"', literal.group(2))))
    embedded_digest = hashlib.sha256(embedded).hexdigest() if embedded else "(empty)"
    print(f"{args.target}:\n  {len(embedded)} bytes, sha256 {embedded_digest}")

    if embedded and embedded_digest == digest:
        print("\n\u2713 already in sync")
        return 0
    if args.check:
        print("\n\u2717 OUT OF SYNC \u2014 rerun without --check to rotate", file=sys.stderr)
        return 1

    # Rebuild the declaration prefix canonically (`bytes constant ROTATE_VK_BLOB =`)
    # rather than preserving group(1), whose `\s*=\s*` otherwise keeps a trailing
    # space and a blank indented line ahead of the hex (git-apply whitespace warns).
    out = (
        src[: literal.start(1)]
        + "bytes constant ROTATE_VK_BLOB ="
        + render(blob)
        + src[literal.end(2) :]
    )

    # Update ONLY the sha256 comment that sits just above the ROTATE literal (the
    # step blob has its own sha256 comment earlier in the file).
    lit_start = LITERAL_RE.search(out).start()
    sha_above = None
    for m in SHA_RE.finditer(out):
        if m.end() <= lit_start:
            sha_above = m
    if sha_above is None:
        print("warning: no `// sha256 <hex>` comment found above ROTATE_VK_BLOB", file=sys.stderr)
    else:
        out = out[: sha_above.start(2)] + digest + out[sha_above.end(2) :]

    args.target.write_text(out)
    short = embedded_digest[:8] if embedded else "(empty)"
    print(f"\n\u2713 rotated {short}\u2026 \u2192 {digest[:8]}\u2026")
    print("  remember: recompile EthBeaconLightClient.sol and redeploy; the blob lives in code")
    return 0


if __name__ == "__main__":
    sys.exit(main())
