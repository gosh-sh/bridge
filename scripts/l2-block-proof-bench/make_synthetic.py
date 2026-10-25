#!/usr/bin/env python3
"""Builds a self-consistent `proveBlock` vector without any node.

A parent-chain block whose receipt trie holds an `AssertionCreated` from the
rollup, an Arbitrum block M that assertion ends at, and M's state with the
EIP-2935 history contract holding the hash of block N. The tries are filled
with other receipts, accounts and slots, so the proofs walk branch and
extension nodes as on a live chain. Deterministic: the same arguments write
the same file.

    ./make_synthetic.py -o fixtures/synthetic_421614.json [--l2-chain-id 421614] [--txs 70] [--tx-index 64]

Use it where no archive node is at hand: the bench runs the real verifier on
it end to end, and the parent block hash it names can be owner-anchored on a
test bridge before the verifier is called. `fetch_vectors.py` builds the same
file from live chains.
"""
import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fetch_vectors import (ASSERTION_BLOCK_HASH_WORD, ASSERTION_CREATED, HISTORY,  # noqa: E402
                           HISTORY_WINDOW, ROLLUPS, Trie, encode_header, ib, nibbles, rlp)
from keccak import keccak256  # noqa: E402

PARENT = {421614: 11155111, 42161: 1}


def h(tag, i=0):
    return keccak256(f"{tag}/{i}".encode())


def header(number, state_root, receipts_root, parent, timestamp):
    return encode_header({
        "parentHash": "0x" + parent.hex(), "sha3Uncles": "0x" + keccak256(rlp([])).hex(),
        "miner": "0x" + h("miner")[:20].hex(), "stateRoot": "0x" + state_root.hex(),
        "transactionsRoot": "0x" + h("txs", number).hex(), "receiptsRoot": "0x" + receipts_root.hex(),
        "logsBloom": "0x" + "00" * 256, "difficulty": "0x0", "number": hex(number), "gasLimit": hex(60_000_000),
        "gasUsed": hex(21_000_000), "timestamp": hex(timestamp), "extraData": "0x",
        "mixHash": "0x" + h("mix", number).hex(), "nonce": "0x0000000000000000", "baseFeePerGas": hex(1_000_000),
        "withdrawalsRoot": "0x" + keccak256(rlp(b"")).hex(), "blobGasUsed": "0x0", "excessBlobGas": "0x0",
        "parentBeaconBlockRoot": "0x" + h("beacon", number).hex(),
        "requestsHash": "0x" + keccak256(b"").hex(),
    })


def receipt(i, logs):
    body = rlp([ib(1), ib(21_000 * (i + 1)), b"\0" * 256,
                [[addr, topics, data] for addr, topics, data in logs]])
    return bytes([2]) + body


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--l2-chain-id", type=int, default=421614)
    ap.add_argument("--txs", type=int, default=70, help="receipts in the parent block")
    ap.add_argument("--tx-index", type=int, default=64, help="the assertion transaction")
    ap.add_argument("--m", type=int, default=210_000_000, help="number of block M")
    ap.add_argument("--behind", type=int, default=1000, help="N = M - behind")
    ap.add_argument("-o", "--out", required=True)
    a = ap.parse_args()
    rollup = bytes.fromhex(ROLLUPS[a.l2_chain_id][2:])
    n_number = a.m - a.behind
    slot = n_number % HISTORY_WINDOW
    n_hash = h("block N", n_number)

    # Block M's state: the history contract among other accounts, its storage
    # holding N's hash among other slots.
    storage = [(nibbles(keccak256(slot.to_bytes(32, "big"))), rlp(n_hash))]
    storage += [(nibbles(keccak256(((slot + 1 + j) % HISTORY_WINDOW).to_bytes(32, "big"))), rlp(h("older", j)))
                for j in range(60)]
    storage_trie = Trie(storage)
    storage_root, storage_proof = storage_trie.proof(storage[0][0])
    history = bytes.fromhex(HISTORY[2:])
    account = rlp([ib(1), b"", storage_root, h("history code")])
    state = [(nibbles(keccak256(history)), account)]
    state += [(nibbles(keccak256(h("account", j)[:20])), rlp([ib(j), ib(10 ** 18), keccak256(rlp(b"")),
                                                             keccak256(b"")])) for j in range(200)]
    state_root, account_proof = Trie(state).proof(state[0][0])
    m_header = header(a.m, state_root, h("m receipts"), h("block M-1"), 1_760_000_000)
    m_hash = keccak256(m_header)

    # The parent block: the assertion transaction among others; its receipt
    # carries a log from another contract before the rollup's.
    words = [h("assertion word", w) for w in range(25)]
    words[ASSERTION_BLOCK_HASH_WORD] = m_hash
    assertion_log = (rollup, [bytes.fromhex(ASSERTION_CREATED[2:]), h("assertion"), h("parent assertion")],
                     b"".join(words))
    other_log = (h("token")[:20], [h("Transfer")], h("amount"))
    receipts = [receipt(i, [other_log] if i % 3 else []) for i in range(a.txs)]
    receipts[a.tx_index] = receipt(a.tx_index, [other_log, assertion_log])
    items = [(nibbles(rlp(ib(i))), r) for i, r in enumerate(receipts)]
    receipts_root, receipt_proof = Trie(items).proof(items[a.tx_index][0])
    l1_header = header(9_000_000, h("l1 state"), receipts_root, h("l1 parent"), 1_760_000_100)

    vector = {
        "synthetic": True,
        "l2ChainId": a.l2_chain_id, "l1ChainId": PARENT[a.l2_chain_id], "rollup": ROLLUPS[a.l2_chain_id],
        "l1BlockNumber": 9_000_000, "mBlockNumber": a.m, "mBlockHash": "0x" + m_hash.hex(),
        "proveBlock": {
            "l1Header": l1_header.hex(), "txIndex": a.tx_index,
            "receiptProof": [p.hex() for p in receipt_proof],
            "l2Header": m_header.hex(),
            "accountProof": [p.hex() for p in account_proof],
            "blockNumber": n_number,
            "storageProof": [p.hex() for p in storage_proof],
        },
        "expect": {"parentBlockHash": "0x" + keccak256(l1_header).hex(), "blockHash": "0x" + n_hash.hex()},
    }
    with open(a.out, "w") as f:
        json.dump(vector, f, indent=1)
        f.write("\n")
    pb = vector["proveBlock"]
    print(f"synthetic {a.l2_chain_id}: M = {a.m}, N = {n_number} (slot {slot}); receipt proof "
          f"{len(pb['receiptProof'])} nodes, account {len(pb['accountProof'])}, storage {len(pb['storageProof'])}")


if __name__ == "__main__":
    main()
