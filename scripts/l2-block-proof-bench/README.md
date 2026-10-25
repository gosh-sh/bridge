# l2-block-proof-bench

Runs `ArbitrumBlockVerifier` on a real TVM instead of reasoning about it. `run.sh` takes the tracked
`contracts/an/0.81.0_compiled/exchange/ArbitrumBlockVerifier.tvc` and gives it the code salt that
`eccUSDCBridge.deployL2Verifier` gives it. It then sends the constructor and `initialize` as internal
messages from the bridge address, calls `proveBlock`, and decodes the message the verifier sends to
the bridge. Separately, it runs the two proofs inside `proveBlock` on live chain data and reports
their gas.

Everything runs offline in `tvm-debugger run`: no network, no keys. You need:

- `sold` 0.81.0 built from
  [gosh-sh/TVM-Solidity-Compiler `feature/keccak256-builtin`](https://github.com/gosh-sh/TVM-Solidity-Compiler/tree/feature/keccak256-builtin).
  A stock 0.81.0 does not assemble `KECCAK256`. The same compiler builds the tracked artefact,
  see `contracts/an/exchange/Makefile`.
- `tvm_debugger` from [tvmlabs/tvm-sdk](https://github.com/tvmlabs/tvm-sdk) `fd76a8a9` or later
  (`cargo build --release -p tvm_debugger`). 3.0.4 fails the instruction with exit 6.

```sh
SOLD=/path/to/sold TVM_DEBUGGER=/path/to/tvm-debugger ./run.sh     # exits non-zero if any check fails
```

## Measured (sold 0.81.0+commit.132c6ec8, tvm-debugger 3.1.0, 2026-10-25, `run.sh` output)

| call | exit | gas |
|---|---|---|
| constructor from a stranger | 310 | 2 564 |
| constructor from the bridge | 0 | 3 567 |
| `proveBlock` before `initialize` | 314 | 3 449 |
| `initialize` with a chain id that is not the code salt | 312 | 4 448 |
| `initialize(421614, 11155111, 0x042b…0cf4)` | 0 | 5 627 |
| `initialize` again | 311 | 2 805 |
| `proveBlock`, synthetic 421614: one message to the bridge, `acceptBlockHashFromL2Verifier(421614, parent hash, N)` | 0 | 317 254 |
| the same with one flipped byte in a receipt, account or storage node, or in the parent header's `receiptsRoot` | 302 | 20 743 – 257 220 |
| the same with a flipped byte in block M's header | 306 | 102 379 |
| the same for another block number | 302 | 262 296 |
| the same for N + 393 168: past M, and N outside the window — the same slot, which still holds N's hash | 0 | 317 254 |
| a second verifier initialized with another rollup | 307 | 102 029 |
| step 1 on live data: Sepolia block 11991511, Arbitrum Sepolia's `AssertionCreated` (tx 64) → M | 0 | 83 939 |
| step 2 on live data: Arbitrum One block 518174193, history slot of 518171193 → N | 0 | 458 810 |

`blockNumber` only picks the history slot, and the message carries no block number. Whatever a
slot holds in M's state was written there by the chain itself, so it is the hash of an ancestor of
M. A number past M, or one outside the window, cannot admit the hash of a block that is not in
the chain; an empty slot is refused (315).

On live data, one whole proof is the two steps together: about 0.55M gas against the 10M
per-transaction limit (`acki-nacki/node/blockchain.conf.json` p20/p21). Step 2 uses Arbitrum One
because its state trie is deeper than Arbitrum Sepolia's: the account proof has 8 nodes (3.4 KB)
and the storage proof 8 (2.5 KB). Every node costs one `KECCAK256` (1 000 gas) plus the cell
reads that walk it.

## Why two kinds of fixture

`proveBlock` needs `eth_getProof` at block M, the block an assertion ends at. M is usually an hour
or more behind the head. On 2026-10-25 no public Arbitrum Sepolia endpoint kept that state:
`sepolia-rollup.arbitrum.io`, publicnode, zan.top and pocket all answer `missing trie node` or
`historical state … is not available`, and Tenderly's gateway limits the proof window. A real
proof therefore needs an archive node. So:

- `fixtures/synthetic_421614.json` comes from `make_synthetic.py`. It is a consistent vector: a
  parent block whose receipt trie has the rollup's `AssertionCreated` among 70 receipts, a block M
  that the assertion names, and M's state with the history contract among 200 accounts and 61
  slots. `run.sh` runs the real verifier on it end to end. Its parent block hash is in `expect`,
  so it can also be owner-anchored on a test bridge before `proveBlock` is called.
- `fixtures/live_steps.json` holds the two halves on live data, taken from public RPCs on
  2026-10-25. They come from different chains, so they run one step at a time through
  `ProofBench.sol`.

`fetch_vectors.py --l1-rpc <Sepolia> --l2-rpc <Arbitrum Sepolia archive> -o fixtures/<name>.json`
writes a live vector in the same format as the synthetic one. It checks every piece against the
nodes. Point `SYNTH=` at the result to run it.

## Files

- `run.sh` — salts, deploys and calls the verifier; runs the live steps; prints exit code and gas per call.
- `ProofBench.sol` — `salted` (the bridge's code salt) and the two `EthTrie` steps as separate calls.
- `make_synthetic.py` — the deterministic synthetic vector.
- `fetch_vectors.py` — a live vector from a parent-chain RPC and an Arbitrum archive RPC.
- `keccak.py` — pure-python Keccak-256 for the two scripts.
- `fixtures/` — the vectors above.
