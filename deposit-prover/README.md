# Deposit Event Prover

Standalone ZK proof generator for Ethereum `Deposit` events. Proves that a `Deposit` event was emitted by `AckiNackiBridge` on Ethereum and binds the AN recipient in-circuit.

The proof is consumed **natively on Acki Nacki** via the `ZKHALO2VERIFYWITHVK` TVM opcode — there is no Ethereum-side ZK verifier.

> Separate workspace: uses axiom-crypto's halo2-lib v0.4.1 (incompatible with the gosh fork in `an-bridge-prover`).

## Circuit: `DepositEventCircuitV2`

The Halo2 circuit (`src/circuit_v2.rs`) proves:

1. Receipt trie inclusion (MPT proof)
2. RLP receipt parsing and event log extraction
3. Event signature match (`Deposit(uint256,address,uint256,int8,bytes32,uint256)`)
4. Contract address binding
5. Event data extraction (depositId, sender, amount, anWorkchain, anAccount)
6. Block hash binding
7. Keccak coprocessor promise commitments

### Public inputs (11 field elements)

| Index | Field | Description |
|-------|-------|-------------|
| 0 | `depositId` | Unique deposit identifier |
| 1 | `sender` | Depositor's Ethereum address |
| 2 | `amount` | USDC amount (6 decimals) |
| 3 | `contractAddress` | Bridge contract address |
| 4 | `dappIdHigh` | Upper 128 bits of AN dApp id (config-supplied) |
| 5 | `dappIdLow` | Lower 128 bits of AN dApp id |
| 6 | `anAccountHigh` | Upper 128 bits of 256-bit AN recipient |
| 7 | `anAccountLow` | Lower 128 bits of 256-bit AN recipient |
| 8 | `blockHashHigh` | Upper 128 bits of block hash |
| 9 | `blockHashLow` | Lower 128 bits of block hash |
| 10 | `promiseCommit` | Keccak coprocessor commitment |

### Circuit parameters

```rust
MAX_DATA_BYTE_LEN: 256
MAX_LOG_NUM: 20
RECEIPT_PF_MAX_DEPTH: 10   // pinned for witness-independent VK
```

Degree K=18; SRS from Acki Nacki chain ceremony (`params/kzg_bn254_18.srs`).

## On-chain consumption (AN side)

`USDCBridge.finalizeDeposit(bytes proof, bytes publicInputs)`:

1. Calls `gosh.zkhalo2VerifyWithVK(VK_BLOB, publicInputs, proof)`
2. Parses deposit fields from public inputs
3. Deploys voucher → `confirmDeposit` → mints ECC to recipient

See [docs/operations/verifying_eth_proof_on_an.md](../docs/operations/verifying_eth_proof_on_an.md) and [docs/operations/evm_an_deposit_e2e_runbook.md](../docs/operations/evm_an_deposit_e2e_runbook.md).

## Example binaries

| Binary | Description |
|--------|-------------|
| `fetch_deposit_data` | Fetch deposit event + MPT proof from Ethereum RPC |
| `export_vk_blob` | Export RLC VkBlob v2 for AN embedding |
| `export_blake2b_proof` | Generate Blake2b SHPLONK proof |
| `export_deposit_proof_set` | Regenerate full `fixtures/deposit_10proofs/` set |
| `verify_opcode_triple` | Round-trip VK + PI + proof |

## Build

```bash
cd deposit-prover
cargo build --release
cargo test
```

Requires Ethereum RPC for integration tests. SRS: run `download_trusted_setup.sh` or place chain ceremony file at `params/kzg_bn254_18.srs`.
