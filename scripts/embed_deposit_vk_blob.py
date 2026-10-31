#!/usr/bin/env python3
"""Rewrite the `VK_BLOB` and `KECCAK_VK_BLOB` literals in an AN-side
`USDCBridge.sol` from the deposit-prover fixtures, and update the sha256 in the
comment above each.

`VK_BLOB` is the deposit circuit's key, `KECCAK_VK_BLOB` the key of the keccak
shard proof that goes with every deposit proof.

Every VK rotation has to reach three places: the fixture in this repo, the
`tvm-sdk` opcode fixtures (`scripts/sync_deposit_opcode_fixtures_to_tvm_sdk.sh`),
and the blob embedded in `USDCBridge.sol`. The third was being hand-edited as
hex, and on 2026-08-03 it turned out the patch artefact and the `acki-nacki`
source had drifted two rotations apart — the patch carried `7322fb82…` while
the source still had `3e2a2db2…`. Hence this script.

    scripts/embed_deposit_vk_blob.py contracts/an/exchange/eccUSDCBridge.sol

Pass --check to verify without writing (exit 1 on mismatch).
"""

import argparse
import hashlib
import pathlib
import re
import sys

FIXTURES = pathlib.Path(__file__).resolve().parents[1] / "deposit-prover/fixtures/deposit_10proofs"
CONSTANTS = [
    ("VK_BLOB", FIXTURES / "deposit_vk_blob.bin"),
    ("KECCAK_VK_BLOB", FIXTURES / "keccak_vk_blob.bin"),
]
BYTES_PER_LINE = 60  # 120 hex chars, matching the existing formatting
SHA_RE = re.compile(r"(//\s+sha256\s*=?\s*)([0-9a-f]{64})")


def literal_re(name: str) -> re.Pattern:
    # Group 1 stops at `=`: the whitespace before the first `hex"` belongs to
    # group 2, which `render` replaces, so no blank line is left behind.
    return re.compile(rf'(bytes constant {name}\s*=)((?:\s*hex"[0-9a-f]*")+)(;)')


def render(blob: bytes, indent: str = " " * 8) -> str:
    lines = [
        f'{indent}hex"{blob[i : i + BYTES_PER_LINE].hex()}"'
        for i in range(0, len(blob), BYTES_PER_LINE)
    ]
    return "\n" + "\n".join(lines)


def comment_start(src: str, pos: int) -> int:
    """Start of the run of `//` lines directly above the line holding `pos`."""
    lines = src[:pos].split("\n")
    start = len(src[:pos]) - len(lines[-1])
    for line in reversed(lines[:-1]):
        if not line.strip().startswith("//"):
            break
        start -= len(line) + 1
    return start


def sync(src: str, name: str, fixture: pathlib.Path, target: pathlib.Path, check: bool):
    """Returns (new source, in sync?) or None on error."""
    blob = fixture.read_bytes()
    digest = hashlib.sha256(blob).hexdigest()
    print(f"{name} fixture: {fixture}\n  {len(blob)} bytes, sha256 {digest}")

    literal = literal_re(name).search(src)
    if literal is None:
        print(f"error: no `bytes constant {name}` literal in {target}", file=sys.stderr)
        return None

    embedded = bytes.fromhex("".join(re.findall(r'hex"([0-9a-f]*)"', literal.group(2))))
    embedded_digest = hashlib.sha256(embedded).hexdigest()
    print(f"{name} in {target}:\n  {len(embedded)} bytes, sha256 {embedded_digest}")

    head = comment_start(src, literal.start())
    comment = src[head : literal.start()]
    sha = SHA_RE.search(comment)
    if embedded_digest == digest and sha is not None and sha.group(2) == digest:
        print(f"✓ {name} already in sync")
        return src, True
    if check:
        print(f"✗ {name} OUT OF SYNC — rerun without --check to rotate", file=sys.stderr)
        return src, False

    out = src[: literal.start(2)] + render(blob) + src[literal.end(2) :]
    if sha is None:
        print(f"warning: no `// sha256 <hex>` comment above {name}", file=sys.stderr)
    else:
        at = head + sha.start(2)
        out = out[:at] + digest + out[at + 64 :]
    print(f"✓ {name} rotated {embedded_digest[:8]}… → {digest[:8]}…")
    return out, True


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("target", type=pathlib.Path, help="path to USDCBridge.sol")
    ap.add_argument("--check", action="store_true", help="verify only, do not write")
    args = ap.parse_args()

    src = original = args.target.read_text()
    in_sync = True
    for name, fixture in CONSTANTS:
        result = sync(src, name, fixture, args.target, args.check)
        if result is None:
            return 2
        src, ok = result
        in_sync = in_sync and ok
    if args.check:
        return 0 if in_sync else 1
    if src != original:
        args.target.write_text(src)
        print("  remember: recompile USDCBridge.sol and redeploy; the blobs live in code")
    return 0


if __name__ == "__main__":
    sys.exit(main())
