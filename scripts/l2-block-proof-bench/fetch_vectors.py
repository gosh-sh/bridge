#!/usr/bin/env python3
"""Builds a `proveBlock` vector for ArbitrumBlockVerifier from live chains.

Takes the newest `AssertionCreated` the rollup emitted on the parent chain,
the Arbitrum block M it ends at, and the EIP-2935 history slot of a block N
behind M, and writes every argument of `proveBlock` plus what the call must
produce. Each piece is checked against the nodes before it is written: the
header RLPs hash to the block hashes, the rebuilt receipt trie has the
header's receiptsRoot, and the storage slot holds N's hash.

    ./fetch_vectors.py --l1-rpc <parent chain> --l2-rpc <Arbitrum> [--l2-rpc ...] \
        [--rollup 0x...] [--behind 1000] [--follow MIN] -o fixtures/<name>.json

By default it takes the newest assertion that is already posted, and
`--l2-rpc` then needs the state at M: M is about 30 min (Arbitrum Sepolia) or
an hour (Arbitrum One) old by then, more than an ordinary node keeps. With
`--follow MIN` it rolls forward instead, as a relayer would: it takes the
proof at the last block of each batch as the batch is posted, while that
state is seconds old, and stops at the first assertion that ends at one of
them. Any node then does. Several `--l2-rpc` may be given; each call tries
them in turn. Pure python, no dependencies.
"""
import argparse
import json
import os
import sys
import time
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from keccak import keccak256  # noqa: E402

ASSERTION_CREATED = "0x901c3aee23cf4478825462caaab375c606ab83516060388344f0650340753630"
ASSERTION_BLOCK_HASH_WORD = 13
HISTORY = "0x0000F90827F1C53a10cb7A02335B175320002935"
HISTORY_WINDOW = 393168
# A batch's delivery on the parent chain, and the NodeInterface precompile
# call that maps an Arbitrum block to its batch.
SEQUENCER_INBOXES = {421614: "0x6c97864CE4bEf387dE0b3310A44230f7E3F1be0D",
                     42161: "0x1c479675ad559DC151F6Ec7ed3FbF8ceE79582B6"}
BATCH_DELIVERED = "0x" + keccak256(
    b"SequencerBatchDelivered(uint256,bytes32,bytes32,bytes32,uint256,(uint64,uint64,uint64,uint64),uint8)").hex()
NODE_INTERFACE = "0x00000000000000000000000000000000000000C8"
FIND_BATCH = keccak256(b"findBatchContainingBlock(uint64)")[:4].hex()
ROLLUPS = {421614: "0x042b2e6c5e99d4c521bd49beed5e99651d9b0cf4",   # Arbitrum Sepolia, on Sepolia
           42161: "0x4dceb440657f21083db8add07665f8ddbe1dcfc0"}    # Arbitrum One, on Ethereum


def call(rpc, method, params):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    req = urllib.request.Request(rpc, body, {"content-type": "application/json", "user-agent": "curl/8.7.1"})
    r = json.load(urllib.request.urlopen(req, timeout=120))
    if "error" in r:
        raise RuntimeError(f"{method} on {rpc}: {r['error']}")
    return r["result"]


def call_any(rpcs, method, params):
    errors = []
    for rpc in rpcs:
        for _ in range(3):
            try:
                return call(rpc, method, params)
            except Exception as e:  # noqa: BLE001 — try the next answer
                errors.append(str(e)[:160])
    raise SystemExit(f"{method}: no endpoint answered:\n  " + "\n  ".join(errors))


def rlp(x):
    if isinstance(x, bytes):
        if len(x) == 1 and x[0] < 0x80:
            return x
        return _len(len(x), 0x80) + x
    p = b"".join(rlp(i) for i in x)
    return _len(len(p), 0xc0) + p


def _len(n, off):
    if n < 56:
        return bytes([off + n])
    b = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return bytes([off + 55 + len(b)]) + b


def hb(h):
    h = h[2:]
    return bytes.fromhex(h if len(h) % 2 == 0 else "0" + h)


def ib(h):
    v = int(h, 16) if isinstance(h, str) else h
    return b"" if v == 0 else v.to_bytes((v.bit_length() + 7) // 8, "big")


# Header fields in RLP order, through Amsterdam; a field the node leaves out is
# not in the header. Same list the relayer's header_rlp.rs encodes.
HEADER_FIELDS = [("parentHash", hb), ("sha3Uncles", hb), ("miner", hb), ("stateRoot", hb),
                 ("transactionsRoot", hb), ("receiptsRoot", hb), ("logsBloom", hb), ("difficulty", ib),
                 ("number", ib), ("gasLimit", ib), ("gasUsed", ib), ("timestamp", ib), ("extraData", hb),
                 ("mixHash", hb), ("nonce", hb), ("baseFeePerGas", ib), ("withdrawalsRoot", hb),
                 ("blobGasUsed", ib), ("excessBlobGas", ib), ("parentBeaconBlockRoot", hb),
                 ("requestsHash", hb), ("blockAccessListHash", hb), ("slotNumber", ib)]


def encode_header(b):
    return rlp([f(b[k]) for k, f in HEADER_FIELDS if b.get(k) is not None])


def header_rlp(b):
    enc = encode_header(b)
    if "0x" + keccak256(enc).hex() != b["hash"]:
        raise SystemExit(f"header RLP of block {b['number']} does not hash to {b['hash']}")
    return enc


def receipt_rlp(r):
    logs = [[hb(lg["address"]), [hb(t) for t in lg["topics"]], hb(lg["data"])] for lg in r["logs"]]
    body = rlp([ib(r["status"]), ib(r["cumulativeGasUsed"]), hb(r["logsBloom"]), logs])
    t = int(r["type"], 16)
    return body if t == 0 else bytes([t]) + body


def nibbles(b):
    return [x for c in b for x in (c >> 4, c & 15)]


def hex_prefix(ns, leaf):
    flag = 2 if leaf else 0
    if len(ns) % 2:
        return bytes([(flag + 1) * 16 + ns[0]]) + bytes(ns[i] * 16 + ns[i + 1] for i in range(1, len(ns), 2))
    return bytes([flag * 16]) + bytes(ns[i] * 16 + ns[i + 1] for i in range(0, len(ns), 2))


class Trie:
    """A Merkle-Patricia trie over (nibble key, value) pairs, enough for a receipt proof."""

    def __init__(self, items):
        self.root = self._node(items, 0)

    def _node(self, items, d):
        if len(items) == 1:
            return ("leaf", items[0][0][d:], items[0][1])
        pre = 0
        while all(len(k) > d + pre for k, _ in items) and len({k[d + pre] for k, _ in items}) == 1:
            pre += 1
        if pre:
            return ("ext", items[0][0][d:d + pre], self._node(items, d + pre))
        children = [None] * 16
        for i in range(16):
            sub = [(k, v) for k, v in items if k[d] == i]
            if sub:
                children[i] = self._node(sub, d + 1)
        return ("branch", children)

    def _enc(self, n):
        if n[0] == "leaf":
            return rlp([hex_prefix(n[1], True), n[2]])
        if n[0] == "ext":
            return rlp([hex_prefix(n[1], False), self._ref(n[2])])
        return rlp([self._ref(c) if c else b"" for c in n[1]] + [b""])

    def _ref(self, n):
        e = self._enc(n)
        if len(e) < 32:
            raise SystemExit("an inline trie node: the verifier does not take those")
        return keccak256(e)

    def proof(self, key):
        out, n, d = [], self.root, 0
        while True:
            out.append(self._enc(n))
            if n[0] == "leaf":
                return keccak256(self._enc(self.root)), out
            if n[0] == "ext":
                d += len(n[1])
                n = n[2]
            else:
                n = n[1][key[d]]
                d += 1


def assertion_proof(l1_rpc, lg):
    """Parent block, transaction index, receipt proof and M's hash of one
    `AssertionCreated` log."""
    l1 = call(l1_rpc, "eth_getBlockByNumber", [lg["blockNumber"], False])
    receipts = call(l1_rpc, "eth_getBlockReceipts", [lg["blockNumber"]])
    tx_index = int(lg["transactionIndex"], 16)
    root, receipt_proof = Trie([(nibbles(rlp(ib(i))), receipt_rlp(r)) for i, r in enumerate(receipts)]) \
        .proof(nibbles(rlp(ib(tx_index))))
    if "0x" + root.hex() != l1["receiptsRoot"]:
        raise SystemExit("the rebuilt receipt trie does not have the header's receiptsRoot")
    return l1, tx_index, receipt_proof, m_hash_of(lg)


def m_hash_of(lg):
    data = hb(lg["data"])
    return "0x" + data[ASSERTION_BLOCK_HASH_WORD * 32:(ASSERTION_BLOCK_HASH_WORD + 1) * 32].hex()


def state_proof(l2_rpcs, m, behind):
    """The history slot of N = M - behind in M's state, and N's hash."""
    n_number = int(m["number"], 16) - behind
    slot = n_number % HISTORY_WINDOW
    proof = call_any(l2_rpcs, "eth_getProof", [HISTORY, [hex(slot)], m["number"]])
    n_hash = call_any(l2_rpcs, "eth_getBlockByNumber", [hex(n_number), False])["hash"]
    if int(proof["storageProof"][0]["value"], 16) != int(n_hash, 16):
        raise SystemExit(f"history slot {slot} at M does not hold the hash of block {n_number}")
    return proof, n_number, n_hash


def batch_of(l2_rpcs, number):
    """The batch that holds Arbitrum block `number`, or None while none does."""
    data = "0x" + FIND_BATCH + number.to_bytes(32, "big").hex()
    for rpc in l2_rpcs:
        try:
            return int(call(rpc, "eth_call", [{"to": NODE_INTERFACE, "data": data}, "latest"]), 16)
        except Exception:  # noqa: BLE001 — not in a posted batch yet, or a flaky node
            continue
    return None


def last_block_of(l2_rpcs, batch, head):
    """The last Arbitrum block of `batch`: an assertion can end only there."""
    lo, hi = head - 50_000, head
    if (batch_of(l2_rpcs, lo) or 0) > batch:
        return None
    while lo < hi:
        mid = (lo + hi + 1) // 2
        b = batch_of(l2_rpcs, mid)
        if b is not None and b <= batch:
            lo = mid
        else:
            hi = mid - 1
    return lo if batch_of(l2_rpcs, lo) == batch else None


def follow(a, rollup, inbox):
    """Rolls forward from now: takes the proof at the last block of every batch
    as the batch is posted, while that state is seconds old, and waits for an
    assertion that ends at one of them. No archive node: M is about 30 min
    (Arbitrum Sepolia) or an hour (Arbitrum One) old when its assertion lands,
    but its batch is posted within a minute of it."""
    snapshots = {}
    seen = int(call(a.l1_rpc, "eth_blockNumber", []), 16)
    deadline = time.time() + a.follow * 60
    print(f"following from parent-chain block {seen} for up to {a.follow} min", flush=True)
    while time.time() < deadline:
        time.sleep(12)
        head = int(call(a.l1_rpc, "eth_blockNumber", []), 16)
        if head <= seen:
            continue
        span = {"fromBlock": hex(seen + 1), "toBlock": hex(head)}
        for lg in call(a.l1_rpc, "eth_getLogs", [{"address": inbox, "topics": [BATCH_DELIVERED], **span}]):
            batch = int(lg["topics"][1], 16)
            for _ in range(20):           # the Arbitrum node learns of the batch from the parent chain
                l2_head = int(call_any(a.l2_rpc, "eth_blockNumber", []), 16)
                last = last_block_of(a.l2_rpc, batch, l2_head)
                if last is not None and (batch_of(a.l2_rpc, last + 1) or batch + 1) > batch:
                    break
                time.sleep(6)
            else:
                print(f"batch {batch}: its last block not found, skipped", flush=True)
                continue
            m = call_any(a.l2_rpc, "eth_getBlockByNumber", [hex(last), False])
            snapshots[m["hash"]] = (m, *state_proof(a.l2_rpc, m, a.behind))
            print(f"batch {batch} posted: proof taken at its last block {last}", flush=True)
        for lg in call(a.l1_rpc, "eth_getLogs", [{"address": rollup, "topics": [ASSERTION_CREATED], **span}]):
            m_hash = m_hash_of(lg)
            print(f"assertion in parent block {int(lg['blockNumber'], 16)} ends at {m_hash}"
                  f"{' — taken' if m_hash in snapshots else ' — before this run'}", flush=True)
            if m_hash in snapshots:
                return lg, snapshots[m_hash]
        seen = head
    raise SystemExit(f"no assertion ended at a batch taken within {a.follow} min")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--l1-rpc", required=True, help="the rollup's parent chain")
    ap.add_argument("--l2-rpc", required=True, action="append", help="the Arbitrum chain")
    ap.add_argument("--rollup", help="rollup contract; default from the Arbitrum chain id")
    ap.add_argument("--behind", type=int, default=1000, help="N = M - behind (default 1000)")
    ap.add_argument("--scan", type=int, default=3000, help="parent-chain blocks to search for the assertion")
    ap.add_argument("--follow", type=int, metavar="MIN",
                    help="roll forward instead: take proofs as batches are posted, for up to MIN minutes")
    ap.add_argument("-o", "--out", required=True)
    a = ap.parse_args()
    if not 0 < a.behind <= HISTORY_WINDOW:
        raise SystemExit(f"--behind must be in 1..{HISTORY_WINDOW}")

    l1_id = int(call(a.l1_rpc, "eth_chainId", []), 16)
    l2_id = int(call_any(a.l2_rpc, "eth_chainId", []), 16)
    rollup = (a.rollup or ROLLUPS.get(l2_id) or sys.exit(f"no known rollup for chain {l2_id}: pass --rollup")).lower()

    if a.follow:
        if l2_id not in SEQUENCER_INBOXES:
            raise SystemExit(f"no known SequencerInbox for chain {l2_id}")
        lg, (m, proof, n_number, n_hash) = follow(a, rollup, SEQUENCER_INBOXES[l2_id])
        l1, tx_index, receipt_proof, m_hash = assertion_proof(a.l1_rpc, lg)
    else:
        head = int(call(a.l1_rpc, "eth_blockNumber", []), 16)
        logs = call(a.l1_rpc, "eth_getLogs", [{"address": rollup, "topics": [ASSERTION_CREATED],
                                              "fromBlock": hex(head - a.scan), "toBlock": hex(head)}])
        if not logs:
            raise SystemExit(f"no AssertionCreated from {rollup} in the last {a.scan} blocks of chain {l1_id}")
        l1, tx_index, receipt_proof, m_hash = assertion_proof(a.l1_rpc, logs[-1])
        m = call_any(a.l2_rpc, "eth_getBlockByHash", [m_hash, False])
        if m is None:
            raise SystemExit(f"the Arbitrum node does not know block {m_hash}")
        proof, n_number, n_hash = state_proof(a.l2_rpc, m, a.behind)
    m_number = int(m["number"], 16)

    vector = {
        "l2ChainId": l2_id, "l1ChainId": l1_id, "rollup": rollup,
        "l1BlockNumber": int(l1["number"], 16), "mBlockNumber": m_number, "mBlockHash": m_hash,
        "proveBlock": {
            "l1Header": header_rlp(l1).hex(), "txIndex": tx_index,
            "receiptProof": [p.hex() for p in receipt_proof],
            "l2Header": header_rlp(m).hex(),
            "accountProof": [p[2:] for p in proof["accountProof"]],
            "blockNumber": n_number,
            "storageProof": [p[2:] for p in proof["storageProof"][0]["proof"]],
        },
        "expect": {"parentBlockHash": l1["hash"], "blockHash": n_hash},
    }
    with open(a.out, "w") as f:
        json.dump(vector, f, indent=1)
        f.write("\n")
    pb = vector["proveBlock"]
    size = lambda xs: sum(len(x) // 2 for x in xs)  # noqa: E731
    print(f"chain {l2_id} on {l1_id}: assertion in block {vector['l1BlockNumber']} (tx {tx_index}), "
          f"M = {m_number}, N = {n_number}")
    print(f"receipt proof {len(pb['receiptProof'])} nodes / {size(pb['receiptProof'])} B, "
          f"account {len(pb['accountProof'])} / {size(pb['accountProof'])} B, "
          f"storage {len(pb['storageProof'])} / {size(pb['storageProof'])} B")


if __name__ == "__main__":
    main()
