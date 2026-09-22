#!/usr/bin/env python3
"""Check the manifest that tells acki-nacki what to place and where.

acki-nacki pins one commit of this repository and places the files this manifest
lists; nothing else about the set lives on that side. Two mistakes are therefore
silent and expensive: a new artefact that nobody lists never reaches a zerostate,
and a destination that escapes `contracts/` writes into a tree that is not ours —
`contracts/an/token/interface/ISubscriber.sol`, for one, would overwrite
acki-nacki's own copy of that interface if it were ever placed.

    scripts/check_place_manifest.py

Exits non-zero describing every problem.
"""

import argparse
import json
import pathlib
import subprocess
import sys

REPO = pathlib.Path(__file__).resolve().parents[1]
MANIFEST_PATH = "contracts/an/place.json"
ROOT = "contracts/an/"

# acki-nacki keeps its own original of each of these paths at the destination
# our layout would imply. Placing our copy would silently overwrite
# acki-nacki's own copy, so the manifest can never list one of these as
# `place` no matter what `to` says — moving one out of here is a deliberate
# edit of this constant, not a one-line slip in the manifest.
NEVER_PLACED = frozenset({
    "contracts/an/token/interface/ISubscriber.sol",
})


def tracked_files() -> set[str]:
    out = subprocess.run(
        ["git", "-C", str(REPO), "ls-tree", "-r", "--name-only", "HEAD", ROOT],
        capture_output=True, text=True, check=True,
    ).stdout
    # The manifest describes the other files, not itself.
    return {line for line in out.split("\n") if line and line != MANIFEST_PATH}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--manifest", type=pathlib.Path, default=REPO / MANIFEST_PATH)
    args = ap.parse_args()

    manifest = json.loads(args.manifest.read_text())
    problems: list[str] = []

    placed = manifest.get("place", [])
    not_placed = manifest.get("not_placed", [])

    if not isinstance(placed, list):
        problems.append(f"`place` must be a list, got {type(placed).__name__}")
        placed = []
    if not isinstance(not_placed, list):
        problems.append(f"`not_placed` must be a list, got {type(not_placed).__name__}")
        not_placed = []

    listed: dict[str, str] = {}
    destinations: dict[str, str] = {}

    for entry in placed:
        if not isinstance(entry, dict):
            problems.append(f"`place` entry is not an object: {entry!r}")
            continue
        src, dst = entry.get("from", ""), entry.get("to", "")
        if src in NEVER_PLACED:
            problems.append(
                f"{src} must never be placed: acki-nacki owns the original at "
                "its destination, placing it would overwrite acki-nacki's own copy"
            )
        if not src.startswith(ROOT):
            problems.append(f"`from` outside {ROOT}: {src}")
        if src in listed:
            problems.append(f"listed twice: {src}")
        listed[src] = dst
        if not dst.startswith("contracts/") or dst.startswith("/") or ".." in dst.split("/"):
            problems.append(f"`to` must stay inside contracts/ and carry no '..': {dst}")
        if dst in destinations:
            problems.append(f"two sources write to {dst}: {destinations[dst]} and {src}")
        destinations[dst] = src

    for src in not_placed:
        if not src.startswith(ROOT):
            problems.append(f"not_placed outside {ROOT}: {src}")
        if src in listed:
            problems.append(f"both placed and not placed: {src}")
        listed[src] = ""

    tracked = tracked_files()
    for src in sorted(tracked - set(listed)):
        problems.append(f"tracked but in neither list: {src}")
    for src in sorted(set(listed) - tracked):
        problems.append(f"listed but not tracked: {src}")

    print(f"{args.manifest}: {len(placed)} placed, {len(not_placed)} not placed, "
          f"{len(tracked)} tracked under {ROOT}")
    if problems:
        print(f"\n{len(problems)} problem(s):", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        return 1
    print("\nOK — every file under contracts/an is accounted for")
    return 0


if __name__ == "__main__":
    sys.exit(main())
