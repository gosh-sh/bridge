# keccak-tvm-bench

Executes `EthKeccak` on a real TVM instead of reasoning about it. This is the
bench behind the "keccak wall" recorded on `submitAncestry`
(gosh-sh/bridge#36): the library as deployed on shellnet never ran (exit 50
for any input), the fixed library computes correct digests, and one keccak-f
permutation costs ~12.9M gas against Acki Nacki's 10M per-transaction limit.

Everything runs offline in `tvm-cli debug run --tvc`: no network, no keys.
Needs `sold` (0.81.0 was used) and the AN `tvm-cli` (3.0.6 was used).

```sh
SOLD=/path/to/sold TVM_CLI=/path/to/tvm-cli ./run.sh                                  # ../../contracts/an/EthKeccak.sol
SOLD=... TVM_CLI=... ./run.sh --lib reference/EthKeccak_1.4.0_as_deployed.sol        # what shipped (acki-nacki 181b0c6a)
```

`KeccakCheck.sol` is an exit-code oracle around the `internal` library:
0 = digest matches, 201 = differs, 50 / 4 = the library threw, -14 = the
debugger's ~16.7M gas credit ran out. The second column of the trace tvm-cli
writes is cumulative gas; `run.sh` prints the value on the last executed
instruction.

## Measured (n21, sold 0.81.0, tvm-cli 3.0.6, 2026-09-15, `run.sh` output)

| call | `reference/EthKeccak_1.4.0_as_deployed.sol` (shellnet code hash `78905cf7...`) | `contracts/an/EthKeccak.sol` on this branch |
|---|---|---|
| `checkEmpty` - keccak256("") | exit 50 after 687 650 gas: first `bc[i] = ...` of theta, `uint64[5] bc` is zero-length | exit 0, 12 930 015 gas |
| `checkAbc` - keccak256("abc") | exit 50 | exit 0, 12 932 380 gas |
| `checkParent` - `rlpParentHash` of the real header | exit 0, 15 770 gas | exit 0, 15 770 gas |
| `checkHash` - 642-byte Sepolia header 11683168 | exit 50 | exit -14, debugger credit exhausted at 16 777 229 gas (5 permutations, about 65M) |

Fixing only the array moves the failure to `_rotl` (exit 4: `x << n` on
`uint64` is range-checked by sold); after that `~` on `uint64` and the
`uint8()` narrowing in `_squeeze32` fail the same way. The branch library
carries all three fixes and reproduces the block's own hash (checked with a
larger gas credit on 2026-09-13, see the PR thread).

Gas limit reference: `acki-nacki/node/blockchain.conf.json` p20/p21
`gas_limit = 10000000` (`special_gas_limit` p21 also 10M). One permutation is
1.3x the limit, a two-header `submitAncestry` about 130M, a 32-header epoch
about 2e9.

## Files

- `run.sh` - compile + execute, prints exit code and gas per call.
- `KeccakCheck.sol` - the wrapper contract (imports `EthKeccak.sol` from the work dir).
- `reference/EthKeccak_1.4.0_as_deployed.sol` - the library as shipped in acki-nacki
  `contracts/exchange` at `181b0c6a` (code hash `78905cf7...` on shellnet).
- `fixtures/sepolia_11683168_headers.json` - two consecutive Sepolia header RLPs
  (11683168 = the anchor `0x6b83c33d...` the daemon was walking on 2026-09-11, and its parent).
- `fetch_headers.py` - rebuilds header RLPs from any JSON-RPC (`ETH_RPC_URL`) the way
  the relayer's `header_rlp.rs` does, pure python, each RLP checked against the node's
  block hash; `--fields` prints rlp / keccak / parentHash of a fixture.
- `emulate_live.sh` - the same headers against the live light-client account via
  `tvm-cli runx` (local execution on the fetched state): reproduces the exit code
  the daemon gets from the chain without sending anything.
