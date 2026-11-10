#!/usr/bin/env python3
"""Check that every tracked Acki Nacki contract artefact is its source, compiled.

What reaches a zerostate is the committed `.tvc`, and nothing else compiles the
reviewed `.sol` into it. This builds `contracts/an/exchange/` with the compilers
its `Makefile` pins and compares each `.tvc` and `.abi.json` byte for byte with
the tracked one in `contracts/an/<version>_compiled/exchange/`, the version
being the one the `Makefile` builds that contract with.

    SOLD_0_80_1=... SOLD_0_81=... SOLD_0_81_1=... scripts/check_an_artefacts.py

`scripts/fetch_an_toolchain.sh` downloads the released compilers. The build
happens in a copy of `contracts/an`; the checkout is not touched. Exits
non-zero naming every artefact that differs.
"""

import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile

REPO = pathlib.Path(__file__).resolve().parents[1]
AN = REPO / "contracts/an"


def pinned() -> dict[str, str]:
    """Contract name → the sold version the Makefile builds it with."""
    makefile = (AN / "exchange/Makefile").read_text()
    versions = dict(re.findall(r"^SOLD_VERSION_(\w+) := (\S+)$", makefile, re.M))
    names = re.search(r"^TVCS := (.+)$", makefile, re.M)[1].split()
    missing = [n for n in names if n not in versions]
    if missing:
        sys.exit(f"Makefile: no SOLD_VERSION_ for {missing}")
    return {n: versions[n] for n in names}


def main() -> int:
    contracts = pinned()
    # The variables the Makefile reads the compilers from, passed through.
    variables = sorted(set(re.findall(r"^SOLD_VAR_\w+ := (\w+)$",
                                      (AN / "exchange/Makefile").read_text(), re.M)))
    args = [f"{v}={os.environ.get(v, '')}" for v in variables]
    problems = []
    with tempfile.TemporaryDirectory(prefix="an-artefacts-") as tmp:
        copy = pathlib.Path(tmp) / "an"
        shutil.copytree(AN, copy)
        build = copy / "exchange"
        for f in list(build.glob("*.tvc")) + list(build.glob("*.abi.json")):
            f.unlink()
        subprocess.run(["make", "-s", "-C", str(build)] + args, check=True)
        for name, version in contracts.items():
            for ext in ("tvc", "abi.json"):
                built = build / f"{name}.{ext}"
                tracked = AN / f"{version}_compiled/exchange/{name}.{ext}"
                rel = tracked.relative_to(REPO)
                if not tracked.exists():
                    problems.append(f"{rel}: not tracked; sold {version} builds {name}")
                elif built.read_bytes() != tracked.read_bytes():
                    problems.append(f"{rel}: differs from {name}.sol built with sold {version}")
                else:
                    print(f"ok  {rel}")
    for p in problems:
        print(f"FAIL {p}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
