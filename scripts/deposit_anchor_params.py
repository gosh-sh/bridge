#!/usr/bin/env python3
"""Derive the `eccBridge.setAcceptedBlockHash` arguments for a deposit, and
check against an RPC that the block really is canonical.

`finalizeDeposit` proves the deposit's receipt against a block header, but
nothing in the receipt shows that block is on the canonical chain — a privately
mined block with a fabricated event proves equally well. So the AN bridge gates
`finalizeDeposit` on an anchor set that someone populates from outside the
deposit (audit finding BC-D01).

Whoever holds that key is asserting canonicality, which means two obligations
that no contract can enforce: read the hash from a source independent of the
party that built the receipt proof, and wait out reorg depth. This script exists
so that discharging them is one command rather than a judgement call:

    scripts/deposit_anchor_params.py <witness.json>... [--verify]
    scripts/deposit_anchor_params.py --chain-id 421614 --block-hash 0x... [--verify]

A `witness.json` is the `finalizeDeposit` input the deposit relayer's
`prove-one` and the CLI's work directory hold; its header must hash to its
`blockHash`. `--verify` resolves the hash over JSON-RPC and fails unless the
node agrees the block is canonical (its number maps back to the same hash) and
buried at least `--min-confirmations` deep. Point `--rpc` at a node you trust
and that the relayer does not control; verifying against the same endpoint that
produced the witness proves nothing.

The arguments printed are for the owner's `setAcceptedBlockHash`, the one anchor
writer a person calls; `eccBridge` has no attester path. `--call` stays for
scripts that pass it and takes only `setAcceptedBlockHash`. Run this yourself
rather than copying a peer's output.
"""

import argparse
import json
import os
import pathlib
import sys
import urllib.error
import urllib.request

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent / "l2-block-proof-bench"))
from keccak import keccak256  # noqa: E402

USER_AGENT = "curl/8"

# Public RPC per chain id, used only when --rpc is not given. Deliberately
# third-party: the point of the check is independence from our own pipeline.
DEFAULT_RPC = {
    1: "https://ethereum-rpc.publicnode.com",
    10: "https://optimism-rpc.publicnode.com",
    8453: "https://base-rpc.publicnode.com",
    42161: "https://arbitrum-one-rpc.publicnode.com",
    421614: "https://arbitrum-sepolia-rpc.publicnode.com",
    11155111: "https://ethereum-sepolia-rpc.publicnode.com",
}

# Reorg depth beyond which a block is treated as settled. L1 finality is two
# epochs (~64 slots); the default is a round number above it, not a guarantee.
DEFAULT_MIN_CONFIRMATIONS = 64


def anchor_of(path: pathlib.Path) -> tuple[int, int]:
    """(chainId, blockHash) of a witness.json, after checking its header hashes to it."""
    w = json.loads(path.read_text())
    try:
        chain_id = int(w["chainId"])
        block_hash = int(w["blockHash"], 16)
        header = bytes.fromhex(w["header"].removeprefix("0x"))
    except (KeyError, ValueError) as exc:
        raise SystemExit(f"{path}: not a deposit witness ({exc})") from exc
    if int.from_bytes(keccak256(header), "big") != block_hash:
        raise SystemExit(f"{path}: the header does not hash to blockHash")
    return chain_id, block_hash


def rpc(url: str, method: str, params: list) -> object:
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
    req = urllib.request.Request(
        url,
        data=body.encode(),
        # Public providers sit behind WAFs that 403 the default Python-urllib
        # agent, which reads as an unrelated network failure.
        headers={"content-type": "application/json", "user-agent": USER_AGENT},
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            payload = json.load(resp)
    except (urllib.error.URLError, TimeoutError) as exc:
        raise SystemExit(f"{url}: {method} failed: {exc}") from exc
    if "error" in payload:
        raise SystemExit(f"{url}: {method} returned {payload['error']}")
    return payload["result"]


def verify_canonical(url: str, block_hash: int, min_confirmations: int) -> list[str]:
    """Return a list of reasons the anchor must not be admitted (empty = admit)."""
    hash_hex = f"0x{block_hash:064x}"
    block = rpc(url, "eth_getBlockByHash", [hash_hex, False])
    if block is None:
        return [
            "node does not know this block hash — it is not canonical there "
            "(a privately mined or reorged-out block looks exactly like this)"
        ]

    number = int(block["number"], 16)
    at_number = rpc(url, "eth_getBlockByNumber", [hex(number), False])
    if at_number is None or int(at_number["hash"], 16) != block_hash:
        seen = "none" if at_number is None else at_number["hash"]
        return [
            f"block {number} is canonically {seen}, not {hash_hex} — "
            "this header was reorged out"
        ]

    latest = int(rpc(url, "eth_blockNumber", []), 16)
    confirmations = latest - number
    problems = []
    if confirmations < min_confirmations:
        problems.append(
            f"only {confirmations} confirmations (need {min_confirmations}); "
            "admitting now risks anchoring a block that still reorgs out"
        )
    print(f"    number={number} confirmations={confirmations} latest={latest}")
    return problems


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("inputs", nargs="*", type=pathlib.Path, help="witness.json files")
    ap.add_argument("--chain-id", type=int, help="with --block-hash, instead of a witness")
    ap.add_argument("--block-hash", help="with --chain-id, instead of a witness")
    ap.add_argument("--verify", action="store_true", help="check canonicality over RPC")
    ap.add_argument(
        "--call",
        choices=("setAcceptedBlockHash",),
        default="setAcceptedBlockHash",
        help="the call to print arguments for; only the owner path exists",
    )
    ap.add_argument("--rpc", help="JSON-RPC endpoint (default: per-chain public node)")
    ap.add_argument(
        "--min-confirmations",
        type=int,
        default=DEFAULT_MIN_CONFIRMATIONS,
        help=f"reorg depth required by --verify (default {DEFAULT_MIN_CONFIRMATIONS})",
    )
    args = ap.parse_args()

    if (args.chain_id is None) != (args.block_hash is None):
        ap.error("--chain-id and --block-hash go together")
    if not args.inputs and args.chain_id is None:
        ap.error("give witness.json files or --chain-id with --block-hash")
    anchors: dict[tuple[int, int], list[str]] = {}
    for path in args.inputs:
        anchors.setdefault(anchor_of(path), []).append(str(path))
    if args.chain_id is not None:
        anchors.setdefault((args.chain_id, int(args.block_hash, 16)), []).append("the command line")

    rejected = 0
    for (chain_id, block_hash), sources in sorted(anchors.items()):
        print(f"chainId={chain_id} blockHash=0x{block_hash:064x}")
        for src in sources:
            print(f"    from {src}")

        problems: list[str] = []
        if args.verify:
            url = args.rpc or DEFAULT_RPC.get(chain_id) or os.environ.get("RPC_URL")
            if not url:
                problems.append(f"no RPC known for chain {chain_id}; pass --rpc")
            else:
                problems = verify_canonical(url, block_hash, args.min_confirmations)

        if problems:
            rejected += 1
            for problem in problems:
                print(f"    REJECT: {problem}")
            continue

        call_args: dict[str, object] = {
            "chainId": str(chain_id),
            "blockHash": f"0x{block_hash:064x}",
            "accepted": True,
        }
        print(f"    {args.call} " + json.dumps(call_args))

    if rejected:
        print(f"\n{rejected} anchor(s) must NOT be admitted", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
