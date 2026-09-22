#!/usr/bin/env python3
"""Fail if the USDC abbreviation appears without the `ecc` prefix.

The bridge holds real USDC on Ethereum and issues a wrapped token on Acki
Nacki. They are not the same asset, and the difference is exactly what a
reader needs to keep straight: one is Circle's ERC-20, held in custody; the
other is what this bridge mints against it. Writing both as "USDC" makes the
prose and the identifiers lie about which side of the bridge they are on.

So: our token, our contracts and our components are `eccUSDC` — on both
sides, in prose, in file names and in code. A bare `USDC` is reserved for the
genuine Circle token, and has to say so explicitly.

The check is deliberately blunt. Every occurrence of `usdc`, in any case, that
is not immediately preceded by `ecc` is reported — in the contents of every
tracked text file and in the tracked paths themselves. There is no attempt to
guess which side of the bridge a given line is about: that judgement belongs
to a person, and recording it is what the two exemption mechanisms are for.

Exemptions, both meaning "this one really is Circle's token":

  * `usdc-naming.toml` next to this script's repository root, for whole paths
    and for patterns that are the real token wherever they occur. Use it for
    files this repository does not author — vendored patches, compiler output,
    third-party ABIs — and for directories that only ever talk to the L1 side.

  * the marker `real-usdc-ok` on the offending line or the line directly above
    it, for one-off mentions inside a file that is otherwise about our token:

        uint256 before = usdc.balanceOf(address(this));  // real-usdc-ok

    Both placements are honoured because formatters relocate a trailing
    comment that follows an opening brace onto the next line.

Usage:
    scripts/check_ecc_usdc_prefix.py [path ...]

With no arguments every tracked file is checked. Exit status is 1 if anything
was found, 0 otherwise.
"""

import re
import subprocess
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # Python < 3.11
    tomllib = None

MARKER = "real-usdc-ok"
CONFIG = "usdc-naming.toml"

# The abbreviation, in any case, not already carrying the prefix. The prefix
# is matched case-insensitively too: `eccUSDC` is the spelling we want, but
# `ECCUSDC` in a shouted heading is prefixed all the same.
BARE = re.compile(r"(?<!ecc)usdc", re.IGNORECASE)

SKIP_DIRS = {".git", "target", "node_modules", "out", "dist", "build", "cache"}


def load_config(root):
    """`exclude_path` globs and `allow` regexes from the config, if present."""
    path = root / CONFIG
    if not path.is_file():
        return [], []
    if tomllib is None:
        print(f"note: {CONFIG} ignored, this Python has no tomllib")
        return [], []
    with path.open("rb") as handle:
        cfg = tomllib.load(handle)
    return cfg.get("exclude_path", []), [re.compile(p) for p in cfg.get("allow", [])]


def is_checkable(path):
    """True for a regular file we should open; symlinks are checked at target."""
    return not path.is_symlink() and path.is_file()


def tracked_files(root):
    """Every tracked file, or every source file if git is not available.

    Same reasoning as the English check: the CI image has no git, and in a
    fresh clone the filtered walk sees the same set that `git ls-files` would.
    """
    try:
        out = subprocess.run(
            ["git", "-C", str(root), "ls-files", "-z"],
            check=True,
            capture_output=True,
        ).stdout
        paths = (root / p for p in out.decode().split("\0") if p)
        return sorted(p for p in paths if is_checkable(p))
    except (OSError, subprocess.CalledProcessError):
        print("note: git unavailable, walking the working tree instead")
        found = []
        for path in root.rglob("*"):
            if not is_checkable(path):
                continue
            if SKIP_DIRS & set(path.relative_to(root).parts):
                continue
            found.append(path)
        return sorted(found)


def read_text(path):
    """The file's lines, or None if it is not UTF-8 text."""
    try:
        data = path.read_bytes()
    except OSError as err:
        print(f"note: cannot read {path}: {err}")
        return None
    if b"\0" in data:
        return None  # binary
    try:
        return data.decode("utf-8").splitlines()
    except UnicodeDecodeError:
        return None


def excluded(display, globs):
    return any(Path(display).match(g) or str(display).startswith(g.rstrip("*")) for g in globs)


def check_contents(path, display, allow):
    """Report every bare mention inside `path`. Returns the count."""
    lines = read_text(path)
    if lines is None:
        return 0

    found = 0
    for lineno, line in enumerate(lines, start=1):
        previous = lines[lineno - 2] if lineno > 1 else ""
        if MARKER in line or MARKER in previous:
            continue
        if any(p.search(line) for p in allow):
            continue
        hits = list(BARE.finditer(line))
        if not hits:
            continue
        found += len(hits)
        spellings = sorted({h.group(0) for h in hits})
        print(f"{display}:{lineno}:{hits[0].start() + 1}: {', '.join(spellings)}")
        print(f"    {line.strip()[:160]}")
    return found


def main(argv):
    root = Path(__file__).resolve().parent.parent
    globs, allow = load_config(root)

    if argv:
        paths = [Path(a).resolve() for a in argv]
    else:
        paths = tracked_files(root)

    in_names = in_text = skipped = 0
    for path in paths:
        try:
            display = path.relative_to(root)
        except ValueError:
            display = path
        if excluded(display, globs):
            skipped += 1
            continue
        if BARE.search(Path(display).name):
            in_names += 1
            print(f"{display}: the file name itself carries an unprefixed mention")
        in_text += check_contents(path, display, allow)

    if in_names or in_text:
        print()
        print(f"{in_text} mention(s) and {in_names} file name(s) without the `ecc` prefix.")
        print("Our token, contracts and components are `eccUSDC` on both sides.")
        print("If the line really means Circle's token, record that — either with")
        print(f"`{MARKER}` on the line, or as a path/pattern in {CONFIG}.")
        return 1

    print(f"{len(paths) - skipped} file(s) checked ({skipped} excluded), all prefixed.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
