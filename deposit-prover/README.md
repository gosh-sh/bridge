# Deposit Event Prover

Standalone ZK proof generator for Ethereum `Deposit` events using axiom-eth. Proves that a `Deposit`
event was emitted by the `AckiNackiBridge` contract on Ethereum.

*The padding section below was checked on 2026-10-30 against this crate at commit `5e6a2569`,
axiom-eth `1d61be0` (`gosh-stable-rlcmanager-assignment`) and halo2-lib `026e176`. At `a69ba36`
(2026-08-18) the header buffer was 717 bytes and the pinned log data length was 128; both are
792 and 96 in this tree. The rest of this file was not re-checked as a whole.*

> **Status.** This crate emits a raw Halo2 SHPLONK proof. It is consumed natively on the Acki Nacki
> side through the **`ZKHALO2VERIFYWITHVK`** TVM opcode — the name `VERHALO2SHPLONK` used in older
> notes never shipped and appears nowhere in this tree. 

## Why Separate from Main Workspace?

This crate uses **axiom-crypto's halo2-lib v0.4.1** (via axiom-eth), which is incompatible with the
halo2 backend the AN→ETH circuits use. They cannot coexist in one Cargo workspace, so this crate is
excluded from the root workspace and built standalone.

## Circuit: `DepositEventCircuitV2`

The Halo2 circuit (`src/circuit_v2.rs`) proves:

1. **Receipt trie inclusion** — the transaction receipt exists in Ethereum's receipt trie (MPT
   proof), with `max_key_byte_len: 3` (`impl ToMPTInput for ReceiptProof` in `src/circuit_v2.rs`;
   the leaf bound is `receipt_max_byte_len`).
2. **Event log extraction** — the RLP-encoded receipt is parsed and the `Deposit` log extracted.
3. **Event signature** — `log.topics[0] == keccak256("Deposit(uint256,address,uint256,bytes32,uint256)")`
   (`get_deposit_event_signature` in `src/circuit_v2.rs`). Five fields: the event carries the Acki Nacki destination.
4. **Contract address** — the event was emitted by the expected bridge contract.
5. **Event data** — `depositId`, `sender`, `amount`, and the AN destination are extracted and exposed
   as public inputs.
6. **Block-hash public input** — inputs 9 and 10 are the output of the header's keccak
   promise, split in half. `finalizeDeposit` checks those halves against
   `_acceptedBlockHash`. The circuit does not evaluate keccak. `EthCircuitImpl` records
   the calls as promises and publishes only their commitment, public input 11. The prover
   fills those rows from the witness (`mock_fulfill_keccak_promises` in `src/prover.rs` and
   `src/provable.rs`). `finalizeDeposit` does not read input 11, and nothing in this
   repository checks that commitment against a keccak evaluation. The receipt hash and the
   MPT node hashes are promises of the same kind.
7. **Chain binding** — the source chain id is a public input, so a proof from one chain cannot be
   replayed as another.

### Public inputs (12 field elements)

One source of truth: `DEPOSIT_PUBLIC_INPUT_LAYOUT` (`src/circuit_v2.rs:68`);
`NUM_PUBLIC_INPUTS` derives from it (`src/types.rs:11`).

| Index | Field | Description |
|---:|---|---|
| 0 | `depositId` | Unique deposit identifier |
| 1 | `sender` | Depositor's Ethereum address |
| 2 | `amount` | Deposit amount in USDC base units (6 decimals) |
| 3 | `contractAddress` | Bridge contract address |
| 4 | `chainId` | Source chain id, proven in-circuit |
| 5 | `dappIdHigh` | Destination dapp id, upper half |
| 6 | `dappIdLow` | Destination dapp id, lower half |
| 7 | `anAccountHigh` | AN destination account, upper 128 bits |
| 8 | `anAccountLow` | AN destination account, lower 128 bits |
| 9 | `blockHashHigh` | Upper 128 bits of the block hash |
| 10 | `blockHashLow` | Lower 128 bits of the block hash |
| 11 | `promiseCommit` | Keccak-coprocessor promise commitment |

*Earlier revisions of this file documented seven inputs ending at `promiseCommit`; the destination
and chain-binding inputs were added later.*

### Fixed-size witness padding

Several witnesses can produce the same 12 public inputs. They are not one buffer, and they are
not all past a length.

* **Phase-0 bytes of the instance, not a tail.** `depositId`, `sender`, `amount`,
  `contractAddress` and `anAccount` are loaded with `load_witness` and folded by `bytes_to_field`
  (Horner, base 256) with no range check (`src/circuit_v2.rs`). Phase 1 checks the folded field,
  not each byte. Raising one byte by 256 and lowering the byte before it by 1 leaves the field
  element, and so that public input, unchanged. The 12 leading zeros of `sender` and
  `contractAddress` are constants. `dappId` bytes are range-checked to 8 bits, so this carry does
  not apply to them. `chainId` comes from the typed-transaction decode, not from these witnesses.

* **MPT value, path and nodes** (`MPTInput::assign` in axiom-eth). The value is zero-padded to
  `value_max_byte_len` (`receipt_max_byte_len` for the receipt, `calc_max_val_len` for the
  transaction). The path is zero-padded to `max_key_byte_len`. The leaf is zero-padded to
  `max_leaf_bytes`. Each proof node is zero-padded to `max_node_bytes`, and the node list is then
  filled to `max_depth - 1` with `DUMMY_BRANCH`
  (`d18080808080808080808080808080808080`), not with zeros. Each key fragment is zero-padded to
  `2 * max_key_byte_len` nibbles, and the fragment list is filled to `max_depth` with zero
  fragments. The MPT chip range-checks the key bytes, value bytes, root hash, leaf bytes and node
  bytes to 8 bits, and the key-fragment nibbles to 4 bits. The receipt hash uses the prefix named
  by `value_byte_len`. `blockHash` does not: it comes from the header promise, whose length is
  `block_header_len`, tied to the header's RLP `rlp_len`. The log fields are bound through
  `field_len`.

  The hash and the RLC use a length, so bytes past that length can change without changing the
  12 public inputs. They stay range-checked:

  * value bytes past `value_byte_len`;
  * leaf and node bytes past that node's RLP length (`mpt_hash_phase0` is `keccak_var_len` over
    `bytes[..len]`);
  * key-fragment nibbles past `frag_len`, and fragments after `pseudo_depth` (`depth`, or 1
    when the proof is empty). `constrain_rlc_concat` keeps only that prefix.

  `node_type` is 1 for an extension and 0 for a branch. Both parses run, but `select` gives the
  slot's own bytes to the chosen type only. The other parse receives the constant `DUMMY_EXT` or
  `DUMMY_BRANCH`. A padding slot is a branch, so its own bytes are the branch input and the
  extension input is `DUMMY_EXT`. `DUMMY_BRANCH` is 18 bytes. The promise and the RLC take that
  RLP length, not the whole `max_node_bytes` buffer. Bytes after those 18 can be any byte value
  and leave all 12 inputs, including `promiseCommit`, unchanged. Replacing the 18 encoded bytes
  changes `promiseCommit`.

* **RLP copies.** `witness_subarray` copies the parsed slice into `field_cells`, sized to the
  field maximum, and into `encoded_item`, sized to `max_rlp_encoding_len(max_field_len)` — the
  payload plus its prefix and length bytes, not the field maximum. Phase 1 takes the RLC of each
  copy over its length and checks that `encoded_item` is their concatenation. Cells inside that
  length are bound. That is what ties `depositId`, `sender` and `amount` to the log. Cells past
  `field_len` or `encoded_item_len` are not in the RLC. The log address is 20 bytes, the topics
  list is 99, and the data is 96, so those fields are not read from past that payload. Phase 1
  makes no guarantee on the original RLP buffer past `rlp_len`.

* **Block header.** The witness is `MAX_BLOCK_HEADER_BYTES` (792) cells, `load_witness` only,
  zero-filled past the true header. They are not passed to `range_check`. The header is decoded
  as a variable-length list against the 24-slot table. For a slot past the list the decoder still
  reads `header[header_len]` as the next prefix, runs `parse_rlp_prefix` on it (8-bit
  `is_less_than`) and checks that prefix's `len_len` against the slot before treating the slot as
  unused. Every unused slot in `BLOCK_HEADER_MAX_FIELD_LENS` is at most 32 bytes, so `len_len`
  must be 0. Any byte with that length-of-length passes: `0x00`–`0xb7` and `0xc0`–`0xf7`. A long
  prefix (`0xb8`–`0xbf`, `0xf8`–`0xff`), such as `0xb9`, fails. The comment on
  `decompose_rlp_array_phase0` says the cells past the list should be 0. That is an assumption of
  the caller, not a constraint. When the first tail byte has `len_len` 0 the index does not
  advance, so `[header_len+1..792]` is not read. Phase 1's RLC covers `rlp_len` bytes. BC-D03 forces
  `header_len` to equal the RLP list length, so counting an extra byte as part of the header is
  rejected (`mock_fixture --mutate header-pad`).

`keccak_var_len` does not hash the header. It records a promise and returns that promise's output
bytes. `ensure_0_padding` (halo2-lib) multiplies a copy by a length mask for the promise input; it
does not zero the original cells. The same call range-checks `block_header_len`, and the output
bytes are range-checked when they are split. The RLP chip range-checks every `len_len`,
`field_len` and list payload length. Apart from the MPT chip and those checks, the deposit
circuit range-checks `log_index` and each `dappId` byte.

What input 11 does and does not bind is item 6 above. The relayer submits the proof and the
public inputs. The witness stays off-chain.

### Circuit parameters

Code defaults (`CircuitConfig::default`, unit tests) are 128 B of log data and
3 logs. Those are **not** the production shape. The verifying key is keyed on
the production rows below.

```rust
// Receipt chip — production (baked into the VK / relayer defaults)
PRODUCTION_MAX_DATA_BYTE_LEN: 2048  // two-signer SafeL2 MultiSend is ~1120 B
PRODUCTION_MAX_ENCODED_LOG_LEN: 2211 // axiom-eth per-log bound; more data if fewer topics
PRODUCTION_MAX_LOG_NUM: 20          // chip max; the keccak budget allows ~6 logs at 2048 B
TOPIC_NUM_BOUNDS: (0, 4)
RECEIPT_PF_MAX_DEPTH: 10
FIXED_KECCAK_CAPACITY: 128          // ~17 KB of preimages: a receipt up to ~13.5 KB

// Enclosing tx chip — also baked into the VK
MAX_TX_CALLDATA_BYTE_LEN: 2048      // 1-of-1 Safe execTransaction is 580 B
MAX_TX_ACCESS_LIST_LEN: 512
ENABLE_TX_TYPES: [false, true, true]  // type 1 and 2 only; every other type is refused
```

Fitting every bound above is necessary, not sufficient: the header, both MPT
paths and the receipt must also fit the 128 keccak permutations. On a real
trie with a small transaction, six logs of 2048 B fit and seven do not; a
type-1 transaction near its ~2.8 KB leaf costs about one log more. The
combined cap also applies to the enclosing transaction: a type-1 tx's calldata
and access list may each be up to 2048 B, but together they must fit the
~2805 B leaf.

`fetch_deposit_data` exits **3** when the enclosing transaction is any
type other than 1 or 2 (legacy 0, blob 3, EIP-7702 4, an L2 system type
such as OP-stack 0x7e), or its leaf, calldata or access list is over the cap (that used to
be exit 1, which the relayer retried). `export_blake2b_proof` and
`export_vk_blob` then check the rest before loading the SRS or the key
(`src/provable.rs`): log count, every log's topics and data, receipt and
header length, MPT depths and the keccak budget. A deposit the circuit
cannot prove exits with code **3** and the reason on stderr; any other
failure is a different code. Callers treat 3 as final: the `deposit` CLI
ends at its exit 35, and `deposit-relayer` skips the deposit at once
instead of retrying, recording it in `unprovable_deposit_ids` (not
`parked_deposit_ids`). Only a refusal under the key's own shape (`--degree 18
--max-data-byte-len 2048 --max-log-num 20`, the defaults) is final: under any
other flags the tools exit 1 instead, which callers retry once the flags are
fixed.

A type 0 (legacy) or type 4 (EIP-7702) deposit is neither provable nor
refundable. Send again as type 1 or 2. EIP-155 `chainId = (v − 35) / 2` would
make type 0 provable; that is a product decision, not this circuit.

In-circuit synthesis at the production shape lives in
`tests/circuit_synthesis.rs` (`#[ignore]`, ~2 min each):

```bash
cargo test --release --test circuit_synthesis -- --ignored --nocapture
```

## On-chain consumption

There is **no on-chain ETH-side ZK consumer** for this proof — the Ethereum contract only emits the
event and keeps custody. The proof is consumed by `finalizeDeposit` in
`contracts/an/exchange/eccUSDCBridge.sol`. The caller passes `(proof, publicInputs)`; the verifying
key is not an argument. The contract verifies that pair with `ZKHALO2VERIFYWITHVK` against the VK
embedded in its code, reads the first 9 public inputs, and checks the block-hash halves against
`_acceptedBlockHash`. It does not read public input 11 (`promiseCommit`). The ABI of the older
`USDCBridge` name is still bundled at
`crates/bridge-prover-libraries/python/contracts/USDCBridge.abi.json`.

The relayer that carries a proof across is `crates/deposit-relayer-daemon/`.

## Example binaries

Run with `cargo run --release --example <name>`. Present under `examples/`:

| Example | Description |
|---|---|
| `fetch_deposit_data` | Fetch a deposit event + MPT proof from an Ethereum RPC |
| `test_with_real_data` | Exercise the circuit against real Ethereum data |
| `export_deposit_proof_set` | Export a proof set for downstream consumers |
| `export_vk_blob` | Export the VK blob the AN side embeds |
| `verify_opcode_triple` | Check the `(proof, publicInputs, VK)` triple the opcode consumes |
| `inspect_snark` | Inspect SNARK proof structure |
| `parse_proof_detailed` | Parse and display proof components |
| `downsize_srs` | Reduce an SRS to a smaller degree |
| `mock_fixture` | Run one saved witness through `MockProver` (~2 min) — the pre-flight after a constraint change, before paying for keygen; `--mutate header-pad` checks a constraint rejects what it exists for |
| `generate_verifier` | Generate Solidity verifier bytecode — legacy reference; the AN side verifies natively |
| `generate_aggregation_verifier` | Generate an aggregation verifier — same caveat |
| `export_blake2b_proof` | Export a Blake2b-transcript proof |
| `generate_deposit_fixtures` | Write synthetic `input.json` fixtures for a deposit-id range |

## Development

```bash
cargo build --release
cargo test                 # 79 `#[test]` functions in src/ + tests/ (static count, not a run)
cargo run --release --example fetch_deposit_data -- --help
```

Some tests reach an Ethereum RPC and will not run offline.

## Dependencies

- **axiom-eth** — Ethereum state-proof primitives (receipt trie, MPT, RLP)
- **halo2-base** (axiom-crypto v0.4.1) — circuit builder
- **snark-verifier-sdk** — proof generation and verification
- **ethers** — Ethereum RPC client

## References

- [axiom-eth](https://github.com/axiom-crypto/axiom-eth)
- [Merkle-Patricia Trie](https://ethereum.org/en/developers/docs/data-structures-and-encoding/patricia-merkle-trie/)
- `docs/EVM-contracts-spec.md` §6 — the `Deposit` event and the Ethereum-side deposit path
