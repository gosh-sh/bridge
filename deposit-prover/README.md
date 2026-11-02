# Deposit Event Prover

Standalone ZK proof generator for Ethereum `Deposit` events using axiom-eth. Proves that a `Deposit`
event was emitted by the `AckiNackiBridge` contract on Ethereum.

*Verified against the sources in this crate at commit `a69ba36`, 2026-08-18. Claims about the
Acki Nacki side are marked where they cannot be checked from this repository.*

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
6. **Block hash binding** — the proof is tied to a specific Ethereum block hash.
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

`promiseCommit` names the table of keccak results the deposit proof takes on trust: the block hash,
the trie roots, the receipt and transaction leaves all come from it. The deposit proof does not
show that table is real keccak; a second proof does. `export_blake2b_proof` proves the same table
with the zkevm-hashes `KeccakComponentShardCircuit` (k = 18, capacity 128, the same Hermez SRS)
and writes that proof next to the deposit proof. Its one public input is `promiseCommit`, and
`finalizeDeposit` verifies it against the keccak VkBlob the bridge embeds (`keccak_vk_blob.bin`,
`CircuitShape::KeccakShard`). A deposit proof without it is refused.

*Earlier revisions of this file documented seven inputs ending at `promiseCommit`; the destination
and chain-binding inputs were added later.*

### Fixed-size witness padding

The private witness is longer than those 12 public inputs. axiom-eth passes the receipt and the
enclosing transaction to the MPT chip as fixed-length byte vectors. `MPTInput::assign` appends
zeros until `value` reaches `value_max_byte_len` — `receipt_max_byte_len` for the receipt,
`calc_max_val_len` for the transaction — and until each proof node and key fragment fills
`max_depth` and `max_key_byte_len`. The RLP decoder does the same for a log: `RlpArrayWitness.rlp_array`
and each `RlpFieldWitness.field_cells` are padded out to that field's maximum.

The receipt hash, and every byte copied into a public input, uses only the prefix named by the
length witness (`value_byte_len`, `rlp_len`, `field_len`). Bytes after that length are range-checked
as bytes and are not copied into `depositId`, `sender`, `amount`, `contractAddress`, `chainId`,
the destination, or `blockHash`. A different assignment of that tail is another valid witness of
the same 12 public inputs, and so another Halo2 proof of the same deposit. `finalizeDeposit`
reads the public inputs, not the witness, so the tail cannot mint a second deposit or a deposit
that was not in the receipt. The relayer submits the proof and the public inputs. The tail stays in the witness.

The header buffer and a short log are not this tail:

* The block-header buffer is `MAX_BLOCK_HEADER_BYTES` (792) bytes, but keccak hashes only the true
  header, and that length must equal the RLP list length. Appending a byte and counting it as part
  of the header is rejected (`mock_fixture --mutate header-pad`).
* A short log is not read past its payload. The address is 20 bytes, the topics list is 99 bytes,
  and the data is 96 bytes. Those lengths are constrained, so `depositId` and `sender` cannot come
  from the padding.

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
event and keeps custody. The proof is consumed on the Acki Nacki side by `USDCBridge`, whose ABI is
bundled in this repo at `crates/bridge-prover-libraries/python/contracts/USDCBridge.abi.json`:

```
finalizeDeposit(bytes proof, bytes publicInputs, bytes keccakProof)
```

No verifying key is passed by the caller. The contract parses the deposit fields out of
`publicInputs` itself, verifies `proof` through `ZKHALO2VERIFYWITHVK` against the deposit VK
embedded in its own code, and then verifies `keccakProof` against its embedded keccak VK with
`publicInputs[11]` as the one public input. The opcode needs a node that knows
`CircuitShape::KeccakShard` (VkBlob shape byte 2).

<!-- UNVERIFIED 2026-08-18: what USDCBridge enforces beyond the proof check — bridge-address
equality, per-depositId replay protection, the canonical-block-hash gate, and the mint itself — is
implemented in the Acki Nacki contract, whose source is not in this repository. Only the ABI is
bundled here. Confirm against the acki-nacki repo before relying on any of it. -->

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
| `export_blake2b_proof` | Export a Blake2b-transcript deposit proof, its public inputs and the keccak proof (`--keccak-proof-out`) |
| `export_keccak_shard_set` | Write `keccak_vk_blob.bin` and a `keccak_proof.bin` for every proof of a fixture set |

## Development

```bash
cargo build --release
cargo test                 # 42 `#[test]` functions in src/ + tests/ (static count, not a run)
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
