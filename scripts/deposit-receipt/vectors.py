#!/usr/bin/env python3
"""Deposit receipts in the form `eccBridge.finalizeDeposit` takes them.

`finalizeDeposit(chainId, header, txIndex, receiptProof, logIndex)` proves the
receipt of transaction `txIndex` against the receipts root of `header` and reads
the `Deposit` log number `logIndex` of that receipt. This module builds those
arguments two ways:

- `synthetic(...)` — a self-consistent block with no node: a receipt trie of
  `txs` receipts, the deposit receipt at `tx_index` with any logs around the
  deposit log, and a header over its root. Tests pass the logs they want.
- `live(rpc, tx_hash)` — a real block, from `eth_getBlockByNumber` and
  `eth_getBlockReceipts`. It refuses a block whose receipts do not rebuild its
  `receiptsRoot` or whose header does not hash to the block hash.

    ./vectors.py live --rpc-url <RPC> --tx <hash> -o fixtures/<name>.json

writes a live vector. `logIndex` there counts the logs of the one receipt, not
the block; the JSON keeps the block-wide index the node reports beside it.
"""
import argparse
import json
import os
import sys
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "l2-block-proof-bench"))
from keccak import keccak256  # noqa: E402
from make_synthetic import Trie, encode_header, header, ib, nibbles, rlp  # noqa: E402

DEPOSIT_EVENT = "Deposit(uint256,address,uint256,bytes32,uint256,address)"
DEPOSIT_TOPIC0 = keccak256(DEPOSIT_EVENT.encode())
LEGACY_DEPOSIT_TOPIC0 = keccak256(b"Deposit(uint256,address,uint256,bytes32,uint256)")


def word(v):
    return v.to_bytes(32, "big")


def deposit_log(bridge, deposit_id, amount, an_account, token, *, sender=0xab, timestamp=1_760_000_000,
                topic0=DEPOSIT_TOPIC0, extra_words=0, drop_words=0, extra_topics=0):
    """The `Deposit` log as `AckiNackiBridge.deposit` emits it; the keyword
    arguments bend its shape for negative tests."""
    topics = [topic0, word(deposit_id), word(sender)] + [word(7)] * extra_topics
    words = [word(amount), word(an_account), word(timestamp), word(token)] + [word(0)] * extra_words
    words = words[:len(words) - drop_words]
    return (bridge.to_bytes(20, "big"), topics, b"".join(words))


def receipt(logs, *, tx_type=2, status=1, gas=21_000):
    body = rlp([ib(status), ib(gas), b"\0" * 256, [[a, t, d] for a, t, d in logs]])
    return body if tx_type == 0 else bytes([tx_type]) + body


def synthetic(deposit_receipt, *, txs=40, tx_index=17, number=9_000_000, filler=None):
    """A block whose receipt trie holds `deposit_receipt` at `tx_index` among
    `txs` receipts. Returns the `finalizeDeposit` arguments and the block hash."""
    other = (keccak256(b"token")[:20], [keccak256(b"Transfer")], keccak256(b"amount"))
    receipts = filler or [receipt([other] if i % 3 else [], gas=21_000 * (i + 1)) for i in range(txs)]
    receipts[tx_index] = deposit_receipt
    items = [(nibbles(rlp(ib(i))), r) for i, r in enumerate(receipts)]
    root, proof = Trie(items).proof(items[tx_index][0])
    h = header(number, keccak256(b"state"), root, keccak256(b"parent"), 1_760_000_100)
    return {"header": h.hex(), "txIndex": tx_index, "receiptProof": [p.hex() for p in proof],
            "blockHash": "0x" + keccak256(h).hex(), "receiptsRoot": "0x" + root.hex(),
            "receipts": receipts}


# ------------------------------------------------------------------ live


def _rpc(url, method, params):
    req = urllib.request.Request(url, json.dumps({"jsonrpc": "2.0", "id": 1, "method": method,
                                                  "params": params}).encode(),
                                 {"Content-Type": "application/json", "User-Agent": "deposit-receipt"})
    with urllib.request.urlopen(req, timeout=60) as r:
        out = json.load(r)
    if "error" in out:
        raise SystemExit(f"{method}: {out['error']}")
    return out["result"]


def _hb(h):
    h = h[2:]
    return bytes.fromhex(h if len(h) % 2 == 0 else "0" + h)


def encode_receipt(r):
    """The receipt-trie value of a JSON-RPC receipt: the EIP-2718 type byte,
    unless legacy, then rlp([status, cumulativeGasUsed, logsBloom, logs])."""
    logs = [[_hb(l["address"]), [_hb(t) for t in l["topics"]], _hb(l["data"])] for l in r["logs"]]
    body = rlp([ib(r["status"]), ib(r["cumulativeGasUsed"]), _hb(r["logsBloom"]), logs])
    t = int(r.get("type", "0x0"), 16)
    return body if t == 0 else bytes([t]) + body


def live(url, tx_hash):
    tx = _rpc(url, "eth_getTransactionReceipt", [tx_hash])
    number = tx["blockNumber"]
    block = _rpc(url, "eth_getBlockByNumber", [number, False])
    receipts = _rpc(url, "eth_getBlockReceipts", [number])
    receipts.sort(key=lambda r: int(r["transactionIndex"], 16))
    values = [encode_receipt(r) for r in receipts]
    index = int(tx["transactionIndex"], 16)
    items = [(nibbles(rlp(ib(i))), v) for i, v in enumerate(values)]
    root, proof = Trie(items).proof(items[index][0])
    if "0x" + root.hex() != block["receiptsRoot"]:
        raise SystemExit(f"the receipts of block {int(number, 16)} do not rebuild its receiptsRoot")
    h = encode_header(block)
    if "0x" + keccak256(h).hex() != block["hash"]:
        raise SystemExit(f"the header of block {int(number, 16)} does not encode to its hash")
    chain_id = int(_rpc(url, "eth_chainId", []), 16)
    deposits = [(i, l) for i, l in enumerate(tx["logs"])
                if _hb(l["topics"][0]) in (DEPOSIT_TOPIC0, LEGACY_DEPOSIT_TOPIC0)]
    return {
        "chainId": chain_id, "blockNumber": int(number, 16), "blockHash": block["hash"],
        "transactionHash": tx_hash, "txType": int(tx.get("type", "0x0"), 16),
        "logs": [{"index": i, "blockLogIndex": int(l["logIndex"], 16), "address": l["address"],
                  "topics": l["topics"], "data": l["data"]} for i, l in deposits],
        "finalizeDeposit": {"chainId": chain_id, "header": h.hex(), "txIndex": index,
                            "receiptProof": [p.hex() for p in proof],
                            "logIndex": deposits[0][0] if deposits else None},
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    lv = sub.add_parser("live")
    lv.add_argument("--rpc-url", required=True)
    lv.add_argument("--tx", required=True)
    lv.add_argument("-o", "--out", required=True)
    a = ap.parse_args()
    v = live(a.rpc_url, a.tx)
    with open(a.out, "w") as f:
        json.dump(v, f, indent=1)
        f.write("\n")
    fd = v["finalizeDeposit"]
    print(f"block {v['blockNumber']}: tx {fd['txIndex']}, receipt log {fd['logIndex']}, proof "
          f"{len(fd['receiptProof'])} nodes, {sum(len(p) // 2 for p in fd['receiptProof'])} bytes, "
          f"header {len(fd['header']) // 2} bytes")


if __name__ == "__main__":
    main()
