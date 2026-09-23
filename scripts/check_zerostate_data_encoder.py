#!/usr/bin/env python3
"""Check that the zerostate data encoder still matches the bridge's upgrade decode.

`eccUSDCBridge.onCodeUpgrade` decodes one fixed tuple out of the cell that
`updateCode` carries, and `BridgeZerostateData.getBridgeData` is what builds that
cell for a zerostate. The two lists are written by hand in two files; a field
added to one and not the other is not a compile error — the premined bridge
either decodes the wrong shape or aborts, and the whole chain starts with a
bridge that cannot mint.

    scripts/check_zerostate_data_encoder.py
    scripts/check_zerostate_data_encoder.py --bridge FILE --encoder FILE

Exits non-zero printing both lists when they disagree.
"""

import argparse
import pathlib
import re
import sys

REPO = pathlib.Path(__file__).resolve().parents[1]
DEFAULT_BRIDGE = REPO / "contracts/an/exchange/eccUSDCBridge.sol"
DEFAULT_ENCODER = REPO / "contracts/an/zerostate/BridgeZerostateData.sol"


def strip_comments(src: str) -> str:
    src = re.sub(r"/\*.*?\*/", "", src, flags=re.DOTALL)
    return re.sub(r"//[^\n]*", "", src)


def normalize(type_name: str) -> str:
    return re.sub(r"\s+", " ", type_name).strip()


def decoded_types(path: pathlib.Path) -> list[str]:
    """The type list `onCodeUpgrade` passes to abi.decode."""
    src = strip_comments(path.read_text())
    body = re.search(r"function onCodeUpgrade\s*\([^)]*\)[^{]*\{(.*?)\n    \}", src, flags=re.DOTALL)
    if not body:
        raise SystemExit(f"{path}: no onCodeUpgrade body found")
    match = re.search(r"abi\.decode\s*\([^,]+,\s*\((.*?)\)\s*\)\s*;", body.group(1), flags=re.DOTALL)
    if not match:
        raise SystemExit(f"{path}: no abi.decode(...) in onCodeUpgrade")
    return [normalize(t) for t in split_types(match.group(1))]


def split_types(raw: str) -> list[str]:
    """Split a type list on commas that are not inside mapping(...)."""
    out, depth, current = [], 0, ""
    for ch in raw:
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        if ch == "," and depth == 0:
            out.append(current)
            current = ""
            continue
        current += ch
    if current.strip():
        out.append(current)
    return out


def encoded_types(path: pathlib.Path) -> list[str]:
    """The types of the values `getBridgeData` passes to abi.encode."""
    src = strip_comments(path.read_text())
    signature = re.search(r"function getBridgeData\s*\((.*?)\)\s*public", src, flags=re.DOTALL)
    if not signature:
        raise SystemExit(f"{path}: no getBridgeData(...) found")
    types: dict[str, str] = {}
    for part in split_types(signature.group(1)):
        tokens = part.split()
        if len(tokens) >= 2:
            types[tokens[-1]] = normalize(" ".join(tokens[:-1]))
    for decl in re.finditer(r"^\s*(mapping\s*\([^)]*\)|TvmCell|uint\d*|int\d*|address|bool)\s+(\w+)\s*;",
                            src, flags=re.MULTILINE):
        types[decl.group(2)] = normalize(decl.group(1))
    encode = re.search(r"abi\.encode\s*\((.*?)\)\s*;", src, flags=re.DOTALL)
    if not encode:
        raise SystemExit(f"{path}: no abi.encode(...) in getBridgeData")
    out = []
    for name in (n.strip() for n in split_types(encode.group(1))):
        if name not in types:
            raise SystemExit(f"{path}: cannot resolve the type of `{name}` passed to abi.encode")
        out.append(types[name])
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--bridge", type=pathlib.Path, default=DEFAULT_BRIDGE)
    ap.add_argument("--encoder", type=pathlib.Path, default=DEFAULT_ENCODER)
    args = ap.parse_args()

    decoded = decoded_types(args.bridge)
    encoded = encoded_types(args.encoder)
    print(f"{args.bridge}:\n  onCodeUpgrade decodes ({', '.join(decoded)})")
    print(f"{args.encoder}:\n  getBridgeData encodes ({', '.join(encoded)})")
    if decoded != encoded:
        print("\nthe encoder and the upgrade decode disagree", file=sys.stderr)
        return 1
    print("\nOK — the zerostate data encoder matches the bridge's upgrade decode")
    return 0


if __name__ == "__main__":
    sys.exit(main())
