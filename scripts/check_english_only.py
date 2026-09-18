#!/usr/bin/env python3
"""Fail if a tracked file carries text in a language other than English.

Documentation and comments in this repository are English. The rule is not
about taste: a comment written in another language is invisible to half the
people who will read the code next, it cannot be searched for by the English
term it describes, and it tends to be the sentence that explains *why* — the
one part a reader cannot reconstruct from the code.

What counts as a violation is a *letter outside the Latin script*: Cyrillic,
Greek prose, Hebrew, Arabic, CJK, Hangul, Devanagari, and so on. Symbols,
punctuation and emoji are not letters and are left alone, so the em dashes,
the `×` in `12 × 32 B`, the arrows and the ✅/❌ in the CI pipelines all pass.
Accented Latin (`é`, `ü`) is Latin script and passes too.

Three deliberate exceptions, all for characters that are notation rather than
language:

  * Unaccented Greek letters, which are how the ZK and pairing code names its
    operands. Accented Greek (alpha with tonos and friends) and final sigma
    occur in Greek prose but never in mathematics, so those still fail.
  * MICRO SIGN (U+00B5), for `µUSDC`.
  * Letters that exist only for formulas: the Mathematical Alphanumeric
    Symbols block (`𝔾₂`; bold, italic, script, fraktur and double-struck
    Latin and Greek), the letterlike double-struck, script and black-letter
    capitals (`ℤ`, `ℚ`, `ℋ`, `ℜ`) and superscript Latin letters
    (`limbᵢ·(2⁸⁸)ⁱ`). None of them occur in any language's prose, and the
    homoglyph concern below is about Cyrillic, which stays caught.

A side effect worth having: this also catches homoglyphs. A Cyrillic "a" that
wandered into a Latin identifier or a config key is the kind of bug that
survives review, because the two characters are pixel-identical.

Intentional non-Latin text — a test fixture that feeds non-ASCII input to a
parser, code that matches the localised output of an external tool — is
exempted by the marker `non-english-ok`, ideally with a reason. It goes either
on the offending line or on the line directly above it:

    "0x742d35Cc6634C0532...",  // non-english-ok: multibyte fixture

    // non-english-ok: multibyte fixture
    for tail in ["...", "..."] {

Both spellings exist because rustfmt relocates a trailing comment that follows
an opening brace onto the next line, which would silently carry the marker off
the line it was meant to cover. The exemption still reaches exactly one line,
so it cannot silence more than what is in front of the reviewer.

Usage:
    scripts/check_english_only.py [path ...]

With no arguments every tracked file is checked. Exit status is 1 if anything
was found, 0 otherwise.
"""

import re
import subprocess
import sys
import unicodedata
from pathlib import Path

MARKER = "non-english-ok"

# Greek letters spelled as a single word: "GREEK SMALL LETTER ALPHA" passes,
# "GREEK SMALL LETTER ALPHA WITH TONOS" and "GREEK SMALL LETTER FINAL SIGMA"
# do not. That is the line between notation and prose.
MATH_GREEK = re.compile(r"GREEK (SMALL|CAPITAL) LETTER [A-Z]+\Z")

ALLOWED_NAMES = {"MICRO SIGN"}
# Mathematical notation, by Unicode name: the Mathematical Alphanumeric
# Symbols block (U+1D400..U+1D7FF) is "MATHEMATICAL ..."; the letters among
# the letterlike symbols (U+2100..U+214F) are "DOUBLE-STRUCK CAPITAL Z",
# "SCRIPT CAPITAL H", "BLACK-LETTER CAPITAL R"; superscript Latin letters are
# "SUPERSCRIPT LATIN SMALL LETTER I" (subscripts already start with "LATIN ").
MATH_NOTATION = re.compile(
    r"(MATHEMATICAL |DOUBLE-STRUCK |SCRIPT (CAPITAL|SMALL) |BLACK-LETTER CAPITAL |SUPERSCRIPT LATIN )"
)

# Directories that hold build output rather than sources. Only consulted in
# the fallback walk below; `git ls-files` never reports them.
SKIP_DIRS = {".git", "target", "node_modules", "out", "dist", "build", "cache"}


def is_allowed(char):
    """True if `char` may appear in an English-only file."""
    if char.isascii():
        return True  # the overwhelming majority of every line, decided cheaply
    if unicodedata.category(char)[0] != "L":
        return True  # not a letter: punctuation, symbol, emoji, digit, space
    name = unicodedata.name(char, "")
    if name.startswith("LATIN "):
        return True
    if name in ALLOWED_NAMES:
        return True
    if MATH_NOTATION.match(name):
        return True
    return bool(MATH_GREEK.match(name))


def is_checkable(path):
    """True for a regular file we should open.

    Symlinks are skipped: git tracks the sub-workspace links under
    `crates/bridge-prover-libraries/` as files, but what they point at is a
    directory that is already checked at its real location.
    """
    return not path.is_symlink() and path.is_file()


def tracked_files(root):
    """Every tracked file, or every source file if git is not available.

    The CI image has no git binary, and installing one to list the files of a
    clone that was just made for us is not worth a network round trip — in a
    fresh clone the filtered walk and `git ls-files` see the same set. Locally
    git is present, and then the tracked set is the honest one: it keeps the
    check off build artefacts and off anything gitignored.
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


def check(path, display):
    """Report every non-English character in `path`. Returns the count."""
    lines = read_text(path)
    if lines is None:
        return 0

    found = 0
    for lineno, line in enumerate(lines, start=1):
        previous = lines[lineno - 2] if lineno > 1 else ""
        if MARKER in line or MARKER in previous:
            continue
        offenders = [(col, ch) for col, ch in enumerate(line, start=1) if not is_allowed(ch)]
        if not offenders:
            continue
        found += len(offenders)
        col, _ = offenders[0]
        names = sorted({unicodedata.name(ch, "UNNAMED") for _, ch in offenders})
        print(f"{display}:{lineno}:{col}: {', '.join(names)}")
        print(f"    {line.strip()}")
    return found


def main(argv):
    root = Path(__file__).resolve().parent.parent
    if argv:
        paths = [Path(a).resolve() for a in argv]
    else:
        paths = tracked_files(root)

    total = 0
    for path in paths:
        try:
            display = path.relative_to(root)
        except ValueError:
            display = path
        total += check(path, display)

    if total:
        print()
        print(f"{total} non-English character(s) in the text above.")
        print("Rewrite the line in English, or — if the text is a fixture or")
        print(f"has to match an external tool's output — put `{MARKER}: <reason>`")
        print("on the line.")
        return 1

    print(f"{len(paths)} file(s) checked, all English.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
