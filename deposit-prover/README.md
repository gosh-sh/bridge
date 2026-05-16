# Deposit Event Prover

Standalone ZK proof generator for Ethereum `Deposit` events using axiom-eth. Proves that a `Deposit` event was emitted by the `AckiNackiBridge` contract on Ethereum.

> **Phase 4.3 status (2026-05-17)**: this crate emits a raw Halo2 SHPLONK proof; the proof is consumed natively on the AN side via the future `VERHALO2SHPLONK` TVM opcode (in development in `tvm-sdk`). The legacy Go gnark wrapper that used to live under `gnark-wrapper/` plus the Rust `groth16_wrapper` adapter under `src/groth16_wrapper/` were retired together with the ETH-side `AckiNackiBridge.withdraw()` chain. See Decision Log 2026-05-17 in `docs/an_partner_integration_plan.md` for the rationale.

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

## On-chain consumption

There is **no on-chain ETH-side ZK consumer** for this proof. The proof is consumed natively on the AN side via the future `VERHALO2SHPLONK` TVM opcode (work-in-progress in `tvm-sdk`). The corresponding AN-side `TokenBridge.finalizeDeposit(halo2Proof, publicInputs, vk)` will:

1. Verify the Halo2 SHPLONK proof under the immutable VK.
2. Enforce `publicInputs[3] == ETH_BRIDGE_ADDRESS_FR` (wrong-bridge proofs revert).
3. Check the per-`depositId` nullifier (replay reverts).
4. Mint the user's tokens.

See `docs/verifying_eth_proof_on_an.md` for the operational verification flow.

## Example Binaries

| Binary                          | Description                                       |
| ------------------------------- | ------------------------------------------------- |
| `fetch_deposit_data`            | Fetch deposit event + MPT proof from Ethereum RPC |
| `generate_verifier`             | Generate Solidity verifier bytecode (legacy reference; AN side uses native verification) |
| `generate_aggregation_verifier` | Generate aggregation verifier                     |
| `test_with_real_data`           | Test circuit with real Ethereum data              |
| `inspect_snark`                 | Inspect SNARK proof structure                     |
| `parse_proof_detailed`          | Parse and display proof components                |

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
- [Merkle-Patricia Trie](https://ethereum.org/en/developers/docs/data-structures-and-encoding/patricia-merkle-trie/)
- `docs/an_partner_integration_plan.md` Decision Log 2026-05-17 — Phase 4.3 demolition rationale
- `docs/verifying_eth_proof_on_an.md` — Verification flow on the AN side
