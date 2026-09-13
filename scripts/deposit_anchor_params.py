#!/usr/bin/env python3
"""Derive the `USDCBridge.setAcceptedBlockHash` arguments for a deposit proof,
and check against an RPC that the block really is canonical.

The deposit proof binds the event to a block whose header hashes to public
inputs #9/#10, but it cannot show that block is on the canonical chain — a
privately mined block with a fabricated event proves equally well. So the AN
bridge gates `finalizeDeposit` on an anchor set that someone populates from
outside the proof (audit finding BC-D01).

Whoever holds that key is asserting canonicality, which means two obligations
that no contract can enforce: read the hash from a source independent of the
party that produced the proof, and wait out reorg depth. This script exists so
that discharging them is one command rather than a judgement call:

    scripts/deposit_anchor_params.py deposit-prover/fixtures/deposit_10proofs/proof_00
    scripts/deposit_anchor_params.py <dir-or-public_inputs.bin>... --verify
    scripts/deposit_anchor_params.py <...> --verify --call attestBlockHash

`--verify` resolves the hash over JSON-RPC and fails unless the node agrees the
block is canonical (its number maps back to the same hash) and buried at least
`--min-confirmations` deep. Point `--rpc` at a node you trust and that the
relayer does not control; verifying against the same endpoint that produced the
witness proves nothing.

`--call` picks which writer's arguments to print: `setAcceptedBlockHash` for the
owner path, `attestBlockHash` for one attester's vote under the M-of-N path. The
obligations are identical either way — the threshold distributes the trust, it
does not discharge the checks, and N attesters reading one RPC provider are one
attester. Run this yourself rather than copying a peer's output.
"""

import argparse
import json
import os
import pathlib
import sys
import urllib.error
import urllib.request

USER_AGENT = "curl/8"  # matches deposit-prover/scripts/recanonicalize_fixture_headers.py

FR_BYTES = 32
NUM_PI = 12
PI_CHAIN_ID = 4
PI_BLOCK_HASH_HI = 9
PI_BLOCK_HASH_LO = 10

# Public RPC per chain id, used only when --rpc is not given. Deliberately
# third-party: the point of the check is independence from our own pipeline.
DEFAULT_RPC = {
    1: "https://ethereum-rpc.publicnode.com",
    10: "https://optimism-rpc.publicnode.com",
    8453: "https://base-rpc.publicnode.com",
    42161: "https://arbitrum-one-rpc.publicnode.com",
    11155111: "https://ethereum-sepolia-rpc.publicnode.com",
}

# Reorg depth beyond which a block is treated as settled. L1 finality is two
# epochs (~64 slots); the default is a round number above it, not a guarantee.
DEFAULT_MIN_CONFIRMATIONS = 64


def read_public_inputs(path: pathlib.Path) -> bytes:
    """Accept either a `public_inputs.bin` or a proof directory containing one."""
    if path.is_dir():
        path = path / "public_inputs.bin"
    blob = path.read_bytes()
    if len(blob) != NUM_PI * FR_BYTES:
        raise SystemExit(
            f"{path}: expected {NUM_PI * FR_BYTES} bytes "
            f"({NUM_PI} x {FR_BYTES}-byte LE Fr), got {len(blob)}"
        )
    return blob


def fr(blob: bytes, index: int) -> int:
    off = index * FR_BYTES
    return int.from_bytes(blob[off : off + FR_BYTES], "little")


def anchor_of(blob: bytes) -> tuple[int, int]:
    """Return (chainId, blockHash) as the contract reconstructs them: the circuit
    splits the 32-byte hash into big-endian 16-byte halves, hi at the lower
    instance index, so the hash is `hi << 128 | lo`."""
    hi = fr(blob, PI_BLOCK_HASH_HI)
    lo = fr(blob, PI_BLOCK_HASH_LO)
    if hi >= 1 << 128 or lo >= 1 << 128:
        raise SystemExit(
            "block-hash halves exceed 128 bits — public inputs are not the "
            "12-PI chain-binding layout this script understands"
        )
    return fr(blob, PI_CHAIN_ID), (hi << 128) | lo


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
    ap.add_argument(
        "inputs",
        nargs="+",
        type=pathlib.Path,
        help="proof directories or public_inputs.bin files",
    )
    ap.add_argument("--verify", action="store_true", help="check canonicality over RPC")
    ap.add_argument(
        "--call",
        choices=("setAcceptedBlockHash", "attestBlockHash"),
        default="setAcceptedBlockHash",
        help="which anchor writer to print arguments for (default owner path)",
    )
    ap.add_argument("--rpc", help="JSON-RPC endpoint (default: per-chain public node)")
    ap.add_argument(
        "--min-confirmations",
        type=int,
        default=DEFAULT_MIN_CONFIRMATIONS,
        help=f"reorg depth required by --verify (default {DEFAULT_MIN_CONFIRMATIONS})",
    )
    args = ap.parse_args()

    anchors: dict[tuple[int, int], list[str]] = {}
    for path in args.inputs:
        chain_id, block_hash = anchor_of(read_public_inputs(path))
        anchors.setdefault((chain_id, block_hash), []).append(str(path))

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
        }
        # `attestBlockHash` has no `accepted` flag: a vote is only ever for
        # admission, and retraction stays an owner action.
        if args.call == "setAcceptedBlockHash":
            call_args["accepted"] = True
        print(f"    {args.call} " + json.dumps(call_args))

    if rejected:
        print(f"\n{rejected} anchor(s) must NOT be admitted", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
