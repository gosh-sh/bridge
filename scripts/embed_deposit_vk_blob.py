#!/usr/bin/env python3
"""Rewrite the `VK_BLOB` literal in an AN-side `USDCBridge.sol` from the
deposit-prover fixture, and update the sha256 in the comment above it.

Every VK rotation has to reach three places: the fixture in this repo, the
`tvm-sdk` opcode fixtures (`scripts/sync_deposit_opcode_fixtures_to_tvm_sdk.sh`),
and the blob embedded in `USDCBridge.sol`. The third was being hand-edited as
hex, and on 2026-08-03 it turned out the patch artefact and the `acki-nacki`
source had drifted two rotations apart — the patch carried `7322fb82…` while
the source still had `3e2a2db2…`. Hence this script.

    scripts/embed_deposit_vk_blob.py ../acki-nacki/contracts/exchange/USDCBridge.sol

Pass --check to verify without writing (exit 1 on mismatch).
"""

import argparse
import hashlib
import pathlib
import re
import sys

BLOB = pathlib.Path(__file__).resolve().parents[1] / (
    "deposit-prover/fixtures/deposit_10proofs/deposit_vk_blob.bin"
)
BYTES_PER_LINE = 60  # 120 hex chars, matching the existing formatting
LITERAL_RE = re.compile(r'(bytes constant VK_BLOB\s*=\s*)((?:\s*hex"[0-9a-f]*")+)(;)')
SHA_RE = re.compile(r"(//\s+sha256 )([0-9a-f]{64})")


def render(blob: bytes, indent: str = " " * 8) -> str:
    lines = [
        f'{indent}hex"{blob[i : i + BYTES_PER_LINE].hex()}"'
        for i in range(0, len(blob), BYTES_PER_LINE)
    ]
    return "\n" + "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("target", type=pathlib.Path, help="path to USDCBridge.sol")
    ap.add_argument("--check", action="store_true", help="verify only, do not write")
    args = ap.parse_args()

    blob = BLOB.read_bytes()
    digest = hashlib.sha256(blob).hexdigest()
    print(f"fixture: {BLOB}\n  {len(blob)} bytes, sha256 {digest}")

    src = args.target.read_text()
    literal = LITERAL_RE.search(src)
    if literal is None:
        print(f"error: no `bytes constant VK_BLOB` literal in {args.target}", file=sys.stderr)
        return 2

    embedded = bytes.fromhex("".join(re.findall(r'hex"([0-9a-f]*)"', literal.group(2))))
    embedded_digest = hashlib.sha256(embedded).hexdigest()
    print(f"{args.target}:\n  {len(embedded)} bytes, sha256 {embedded_digest}")

    if embedded_digest == digest:
        print("\n✓ already in sync")
        return 0
    if args.check:
        print("\n✗ OUT OF SYNC — rerun without --check to rotate", file=sys.stderr)
        return 1

    out = src[: literal.start(2)] + render(blob) + src[literal.end(2) :]
    out, n = SHA_RE.subn(lambda m: m.group(1) + digest, out, count=1)
    if n == 0:
        print("warning: no `// sha256 <hex>` comment found to update", file=sys.stderr)

    args.target.write_text(out)
    print(f"\n✓ rotated {embedded_digest[:8]}… → {digest[:8]}…")
    print("  remember: recompile USDCBridge.sol and redeploy; the blob lives in code")
    return 0


if __name__ == "__main__":
    sys.exit(main())
