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

Exemptions, all meaning "this one really is Circle's token":

  * `usdc-naming.toml` next to this script's repository root, for whole paths
    and for identifiers that are the real token wherever they occur. Use it
    for files this repository does not author — vendored patches, compiler
    output, third-party ABIs — and for directories that only ever talk to the
    L1 side.

    `allow_token` there is the precise instrument: it names whole identifiers
    (`aUSDC`, `USDC_SEPOLIA`, `UsdcTestLib`) rather than lines, so a line that
    mentions both an allowed identifier and a bare one is still reported on
    the bare one. Each entry is an auditable claim — "this identifier always
    denotes Circle's ERC-20" — which a line-level pattern could never be.

    A `[[path]]` table narrows that claim to one file, which is what files
    dealing with both assets need: the L1 contract may say `usdc` sixty times
    about the ERC-20 it holds while still naming the Acki Nacki counterpart,
    and only the first of those should be excused.

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

# The whole identifier a match sits inside, so an exemption can name
# `aUSDC` without also excusing a bare `USDC` elsewhere on the same line.
WORD = re.compile(r"[A-Za-z0-9_]*(?<!ecc)usdc[A-Za-z0-9_]*", re.IGNORECASE)

SKIP_DIRS = {".git", "target", "node_modules", "out", "dist", "build", "cache"}


def load_config(root):
    """Exclusions from the config: paths, identifiers, and per-path identifiers."""
    path = root / CONFIG
    if not path.is_file():
        return [], set(), []
    if tomllib is None:
        print(f"note: {CONFIG} ignored, this Python has no tomllib")
        return [], set(), []
    with path.open("rb") as handle:
        cfg = tomllib.load(handle)
    scoped = [(e["glob"], set(e.get("allow_token", []))) for e in cfg.get("path", [])]
    return cfg.get("exclude_path", []), set(cfg.get("allow_token", [])), scoped


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
    return any(matches(display, g) for g in globs)


def bare_words(line, allow):
    """Identifiers on `line` that mention the token without the prefix."""
    return [w for w in WORD.finditer(line) if w.group(0) not in allow]


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
        hits = bare_words(line, allow)
        if not hits:
            continue
        found += len(hits)
        spellings = sorted({h.group(0) for h in hits})
        print(f"{display}:{lineno}:{hits[0].start() + 1}: {', '.join(spellings)}")
        print(f"    {line.strip()[:160]}")
    return found


def matches(display, glob):
    return Path(display).match(glob) or str(display).startswith(glob.rstrip("*"))


def main(argv):
    root = Path(__file__).resolve().parent.parent
    globs, allow, scoped = load_config(root)

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
        here = set(allow)
        for glob, tokens in scoped:
            if matches(display, glob):
                here |= tokens
        # The stem, not the whole name: `UsdcTestLib.sol` is the identifier
        # `UsdcTestLib` plus an extension, and an exemption names the former.
        if bare_words(Path(display).stem, here):
            in_names += 1
            print(f"{display}: the file name itself carries an unprefixed mention")
        in_text += check_contents(path, display, here)

    if in_names or in_text:
        print()
        print(f"{in_text} mention(s) and {in_names} file name(s) without the `ecc` prefix.")
        print("Our token, contracts and components are `eccUSDC` on both sides.")
        print("If the line really means Circle's token, record that — with")
        print(f"`{MARKER}` on the line, or as a path or identifier in {CONFIG}.")
        return 1

    print(f"{len(paths) - skipped} file(s) checked ({skipped} excluded), all prefixed.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
