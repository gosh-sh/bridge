# l2-block-proof-bench

Runs `ArbitrumBlockVerifier` on a real TVM instead of reasoning about it. `run.sh` takes the tracked
`contracts/an/0.81.0_compiled/exchange/ArbitrumBlockVerifier.tvc` and gives it the code salt that
`eccUSDCBridge.deployL2Verifier` gives it. It then sends the constructor and `initialize` as internal
messages from the bridge address, calls `proveBlock` and decodes the message the verifier sends to
the bridge. It does this once per vector: a synthetic one, and one the relayer wrote from live
Arbitrum Sepolia. Separately, it runs the two proofs inside `proveBlock` on live Arbitrum One data,
whose state trie is deeper.

Everything runs offline in `tvm-debugger run`: no network, no keys. You need:

- `sold` 0.81.0 built from
  [gosh-sh/TVM-Solidity-Compiler `feature/keccak256-builtin`](https://github.com/gosh-sh/TVM-Solidity-Compiler/tree/feature/keccak256-builtin).
  A stock 0.81.0 does not assemble `KECCAK256`. The same compiler builds the tracked artefact,
  see `contracts/an/exchange/Makefile`.
- `tvm_debugger` from [tvmlabs/tvm-sdk](https://github.com/tvmlabs/tvm-sdk) `fd76a8a9` or later
  (`cargo build --release -p tvm_debugger`). 3.0.4 fails the instruction with exit 6.

```sh
SOLD=/path/to/sold TVM_DEBUGGER=/path/to/tvm-debugger ./run.sh     # exits non-zero if any check fails
SOLD=... TVM_DEBUGGER=... VECTORS="a.json b.json" ./run.sh          # other vectors
```

## Measured (sold 0.81.0+commit.132c6ec8, tvm-debugger 3.1.0, 2026-10-25, `run.sh` output)

| call | exit | synthetic | live Arbitrum Sepolia |
|---|---|---|---|
| constructor from a stranger | 310 | 2 564 | 2 564 |
| constructor from the bridge | 0 | 3 567 | 3 567 |
| `proveBlock` before `initialize` | 314 | 3 449 | 3 449 |
| `initialize` with a chain id that is not the code salt | 312 | 4 448 | 4 448 |
| `initialize(421614, 11155111, 0x042b…0cf4)` | 0 | 5 627 | 5 627 |
| `initialize` again | 311 | 2 805 | 2 805 |
| **`proveBlock`**: one message to the bridge, `acceptBlockHashFromL2Verifier(421614, parent hash, N)` | 0 | 317 254 | **600 538** |
| the same with one flipped byte in a receipt, account or storage node, or in the parent header's `receiptsRoot` | 302 | 20 743 – 257 220 | 20 968 – 476 769 |
| the same with a flipped byte in block M's header | 306 | 102 379 | 97 623 |
| the same for another block number | 302 | 262 296 | 412 435 |
| the same for N + 393 168: past M, and N outside the window — the same slot, which still holds N's hash | 0 | 317 254 | 600 538 |
| a second verifier initialized with another rollup | 307 | 102 029 | 97 273 |

On Arbitrum One data, measured one step at a time through `ProofBench.sol`:

- Step 1: Sepolia block 11991511, Arbitrum Sepolia's `AssertionCreated` (tx 64) → M: 83 939 gas.
- Step 2: Arbitrum One block 518174193, the history slot of 518171193 → N: 458 810 gas. The account
  proof there has 8 nodes (3.4 KB) and the storage proof 8 (2.5 KB).

A whole proof costs about 0.6M gas against the 10M per-transaction limit
(`acki-nacki/node/blockchain.conf.json` p20/p21). Every node costs one `KECCAK256` (1 000 gas)
plus the cell reads that walk it.

`blockNumber` only picks the history slot, and the message carries no block number. Whatever a
slot holds in M's state was written there by the chain itself, so it is the hash of an ancestor of
M. A number past M, or one outside the window, cannot admit the hash of a block that is not in
the chain; an empty slot is refused (315).

## Vectors

- `fixtures/arbitrum_sepolia_11992300.json` was written by `arbitrum-vector` on 2026-10-25. It
  holds the assertion in Sepolia block 11992300 (tx 26), block M = 322755844, and the history slot
  of N = 322753172. The relayer took the slot proof when M's batch was posted, about
  20 s after M; the assertion came 34 minutes later, when ordinary nodes no longer serve M's state.
  To write another, run it with ordinary RPCs. It returns after one or two assertion intervals,
  35–70 minutes on Arbitrum Sepolia:

  ```sh
  eth-lc-relayer arbitrum-vector --parent-rpc-url <Sepolia> --arbitrum-rpc-url <Arbitrum Sepolia> \
      --out fixtures/<name>.json
  ```

- `fixtures/synthetic_421614.json` comes from `make_synthetic.py`. It is a parent block whose
  receipt trie has the rollup's `AssertionCreated` among 70 receipts, a block M that the assertion
  names, and M's state with the history contract among 200 accounts and 61 slots.
- `fixtures/live_steps.json` holds the two halves on live data from different chains, taken from
  public RPCs on 2026-10-25: an Arbitrum Sepolia assertion and the Arbitrum One slot above.

Each vector's parent block hash is in `expect`, so `test_arbitrum_block_proof.py` can owner-anchor
it on a test bridge before it calls `proveBlock`.

## Files

- `run.sh` — salts, deploys and calls the verifier per vector; runs the live steps; prints exit
  code and gas per call.
- `ProofBench.sol` — `salted` (the bridge's code salt) and the two `EthTrie` steps as separate calls.
- `make_synthetic.py` — the deterministic synthetic vector.
- `keccak.py` — pure-python Keccak-256 for `make_synthetic.py`.
- `fixtures/` — the vectors above.
