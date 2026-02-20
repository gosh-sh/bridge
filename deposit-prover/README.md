# Deposit Event Prover

Standalone ZK proof generator for Ethereum Deposit events using axiom-eth. Proves that a `Deposit` event was emitted by the AckiNackiBridge contract on Ethereum, then wraps the Halo2 proof in Groth16 for efficient on-chain verification.

## Why Separate from Main Workspace?

This crate uses **axiom-crypto's halo2-lib v0.4.1** (via axiom-eth), which is incompatible with the halo2-axiom 0.5.x used by `poseidon-proof`. They cannot coexist in the same Cargo workspace.

## Circuit: `DepositEventCircuitV2`

The Halo2 circuit (`src/circuit_v2.rs`) proves:

1. **Receipt Trie Inclusion** — Transaction receipt exists in Ethereum's receipt trie (MPT proof)
2. **Event Log Extraction** — RLP-encoded receipt is parsed, Deposit event log is extracted
3. **Event Signature** — `log.topics[0] == keccak256("Deposit(uint256,address,uint256,uint256)")`
4. **Contract Address** — Event was emitted by the correct bridge contract
5. **Event Data** — depositId, sender, amount extracted and exposed as public inputs
6. **Block Hash Binding** — Proof is tied to a specific Ethereum block hash

### Public Inputs (7 field elements)

| Index | Field             | Description                        |
| ----- | ----------------- | ---------------------------------- |
| 0     | `depositId`       | Unique deposit identifier          |
| 1     | `sender`          | Depositor's Ethereum address       |
| 2     | `amount`          | Deposit amount in wei              |
| 3     | `contractAddress` | Bridge contract address            |
| 4     | `blockHashHigh`   | Upper 128 bits of block hash       |
| 5     | `blockHashLow`    | Lower 128 bits of block hash       |
| 6     | `promiseCommit`   | Commitment for cross-chain promise |

### Circuit Parameters

```rust
MAX_DATA_BYTE_LEN: 128    // Max event data length
MAX_LOG_NUM: 3             // Max logs per receipt
TOPIC_NUM_BOUNDS: (0, 4)   // Min/max topics per log
RECEIPT_PF_MAX_DEPTH: 10   // Max MPT proof depth
```

## Groth16 Wrapper (`gnark-wrapper/`)

The Halo2 verifier exceeds Ethereum's 24KB contract size limit. The gnark wrapper (Go) solves this by wrapping the Halo2 SNARK in a Groth16 proof with a ~2KB verifier.

### Setup (one-time)

```bash
cd gnark-wrapper
go run . setup
```

Generates: `circuit.r1cs`, `proving.key`, `verification.key`, `Groth16Verifier.sol`

### Prove

```bash
go run . prove \
  --snark-proof ../path/to/halo2_proof.bin \
  --snark-vk ../path/to/vk.bin \
  --snark-instances ../path/to/instances.json
```

Output: 288 bytes = 256-byte Groth16 proof + 32-byte promise_commit

## Example Binaries

| Binary                          | Description                                       |
| ------------------------------- | ------------------------------------------------- |
| `fetch_deposit_data`            | Fetch deposit event + MPT proof from Ethereum RPC |
| `export_proof_for_gnark`        | Export Halo2 proof artifacts for gnark wrapper    |
| `generate_verifier`             | Generate Solidity verifier bytecode               |
| `generate_aggregation_verifier` | Generate aggregation verifier                     |
| `test_with_real_data`           | Test circuit with real Ethereum data              |
| `inspect_snark`                 | Inspect SNARK proof structure                     |
| `parse_proof_detailed`          | Parse and display proof components                |
| `analyze_proof_structure`       | Analyze proof structure for debugging             |

## Development

```bash
# Build
cargo build --release

# Run tests (19 tests)
cargo test

# Run specific example
cargo run --release --example fetch_deposit_data -- --help
```

## Dependencies

- **axiom-eth**: Ethereum state proof primitives (receipt trie, MPT, RLP)
- **halo2-base** (axiom-crypto v0.4.1): Circuit builder
- **snark-verifier-sdk**: SNARK proof generation and verification
- **ethers**: Ethereum RPC client

## References

- [axiom-eth](https://github.com/axiom-crypto/axiom-eth) — Ethereum state proof library
- [gnark](https://github.com/ConsenSys/gnark) — Go ZK proof library
- [Merkle-Patricia Trie](https://ethereum.org/en/developers/docs/data-structures-and-encoding/patricia-merkle-trie/)
