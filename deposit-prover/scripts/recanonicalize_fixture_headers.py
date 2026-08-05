#!/usr/bin/env python3
"""Replace the `block_header_rlp` in every fixture witness with the canonical
header of the same block.

Witnesses generated before the EIP-7685 fix carry a header with the Prague
`requestsHash` silently dropped (ethers-core 2.0.14 does not model it), so the
circuit committed the hash of a truncated header as its `blockHash` public
input. Only the header changes here: the receipt / transaction MPT proofs and
the roots they hash to are identical in both encodings, which this script
asserts before writing anything back.

    SEPOLIA_RPC_URL=... python3 scripts/recanonicalize_fixture_headers.py [--check]
"""

import argparse
import json
import os
import pathlib
import shutil
import subprocess
import sys
import urllib.request

RPC = os.environ.get("SEPOLIA_RPC_URL", "https://ethereum-sepolia-rpc.publicnode.com")
FIXTURES = pathlib.Path(__file__).resolve().parent.parent / "fixtures"

# Header field order through Prague / OP Isthmus. Entries are
# (rpc key, "hash" for byte strings | "num" for quantities, required?).
FIELDS = [
    ("parentHash", "hash", True),
    ("sha3Uncles", "hash", True),
    ("miner", "hash", True),
    ("stateRoot", "hash", True),
    ("transactionsRoot", "hash", True),
    ("receiptsRoot", "hash", True),
    ("logsBloom", "hash", True),
    ("difficulty", "num", True),
    ("number", "num", True),
    ("gasLimit", "num", True),
    ("gasUsed", "num", True),
    ("timestamp", "num", True),
    ("extraData", "hash", True),
    ("mixHash", "hash", True),
    ("nonce", "hash", True),
    ("baseFeePerGas", "num", False),
    ("withdrawalsRoot", "hash", False),
    ("blobGasUsed", "num", False),
    ("excessBlobGas", "num", False),
    ("parentBeaconBlockRoot", "hash", False),
    ("requestsHash", "hash", False),
]


def rlp_string(raw: bytes) -> bytes:
    if len(raw) == 1 and raw[0] < 0x80:
        return raw
    if len(raw) <= 55:
        return bytes([0x80 + len(raw)]) + raw
    length = len(raw).to_bytes((len(raw).bit_length() + 7) // 8, "big")
    return bytes([0xB7 + len(length)]) + length + raw


def encode_header(block: dict) -> bytes:
    payload = b""
    for key, kind, required in FIELDS:
        value = block.get(key)
        if value is None:
            if required:
                raise SystemExit(f"block {block['number']} is missing {key}")
            continue
        if kind == "hash":
            raw = bytes.fromhex(value[2:])
        else:
            n = int(value, 16)
            raw = n.to_bytes((n.bit_length() + 7) // 8, "big") if n else b""
        payload += rlp_string(raw)
    length = len(payload).to_bytes((len(payload).bit_length() + 7) // 8, "big")
    return bytes([0xF7 + len(length)]) + length + payload


def keccak256(data: bytes) -> str:
    """Keccak-256 via Foundry's `cast` (hashlib only ships SHA3, not Keccak)."""
    if shutil.which("cast") is None:
        raise SystemExit("`cast` not found — install Foundry (see `make setup`)")
    out = subprocess.run(
        ["cast", "keccak", "0x" + data.hex()],
        capture_output=True,
        text=True,
        check=True,
    )
    return out.stdout.strip()


def fetch_block(number: int) -> dict:
    request = urllib.request.Request(
        RPC,
        data=json.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "eth_getBlockByNumber",
                "params": [hex(number), False],
            }
        ).encode(),
        # Some public endpoints 403 urllib's default agent.
        headers={"content-type": "application/json", "user-agent": "curl/8"},
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        body = json.load(response)
    if "result" not in body or body["result"] is None:
        raise SystemExit(f"RPC returned no block {number}: {body}")
    return body["result"]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit non-zero if any fixture header is non-canonical, write nothing",
    )
    args = parser.parse_args()

    cache: dict[int, bytes] = {}
    stale = []
    for path in sorted(FIXTURES.rglob("input.json")):
        witness = json.loads(path.read_text())
        number = witness["event_data"]["block_number"]
        if number not in cache:
            block = fetch_block(number)
            header = encode_header(block)
            got = keccak256(header)
            if got != block["hash"]:
                raise SystemExit(
                    f"block {number}: encoded header hashes to {got}, chain says {block['hash']}"
                )
            cache[number] = header
        header = cache[number]

        current = bytes(witness["receipt_proof"]["block_header_rlp"])
        if current == header:
            print(f"{path.relative_to(FIXTURES.parent)}: already canonical")
            continue

        # The MPT proofs are unchanged, so the roots must survive the swap.
        for section, root_key, offset in (
            ("receipt_proof", "receipt_root", 5),
            ("tx_proof", "transactions_root", 4),
        ):
            expected = bytes(witness[section][root_key])
            if expected not in header:
                raise SystemExit(
                    f"{path}: canonical header does not contain the fixture's "
                    f"{root_key} — witness and block disagree (field {offset})"
                )

        stale.append(path)
        if args.check:
            continue
        witness["receipt_proof"]["block_header_rlp"] = list(header)
        path.write_text(json.dumps(witness) + "\n")
        print(
            f"{path.relative_to(FIXTURES.parent)}: {len(current)} B -> {len(header)} B "
            f"(block {number})"
        )

    if args.check and stale:
        print(f"\n{len(stale)} fixture(s) carry a non-canonical header:", file=sys.stderr)
        for path in stale:
            print(f"  {path}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
