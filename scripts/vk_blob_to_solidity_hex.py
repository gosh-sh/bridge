#!/usr/bin/env python3
"""Emit Solidity concatenated hex\"...\" lines for USDCBridge.sol VK_BLOB."""
from __future__ import annotations
import argparse, hashlib, pathlib

def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("vk_blob", type=pathlib.Path)
    ap.add_argument("--width", type=int, default=100)
    args = ap.parse_args()
    data = args.vk_blob.read_bytes()
    hx = data.hex()
    chunks = [hx[i : i + args.width] for i in range(0, len(hx), args.width)]
    print(f"    // {len(data)} bytes; sha256={hashlib.sha256(data).hexdigest()}")
    print("    bytes constant VK_BLOB =")
    for i, c in enumerate(chunks):
        suf = ";" if i == len(chunks) - 1 else ""
        print(f'        hex"{c}"{suf}')

if __name__ == "__main__":
    main()
