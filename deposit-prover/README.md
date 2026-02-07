# Deposit Event Prover

A standalone ZK proof generator for Ethereum Deposit events using axiom-eth.

## Overview

This binary generates zero-knowledge proofs that a `Deposit` event was emitted by the AckiNackiBridge contract on Ethereum. It uses:

- **axiom-eth**: For Ethereum state proofs (receipt trie verification, RLP decoding)
- **halo2-base**: For circuit building (axiom-crypto's version)
- **Poseidon hash**: For commitment and nullifier computation

## Why Separate from Main Workspace?

The deposit prover uses **axiom-crypto's halo2-lib v0.4.1**, while the main workspace (withdrawal circuit) uses **scroll-tech's halo2-lib** for Acki Nacki compatibility. These two versions are incompatible and cannot coexist in the same Cargo workspace.

## Architecture

### Circuit Logic

The circuit proves:

1. **Receipt Trie Inclusion**: Verify the transaction receipt exists in Ethereum's receipt trie using MPT proof
2. **Event Log Extraction**: Parse RLP-encoded receipt and extract the Deposit event log
3. **Event Signature Verification**: Verify `log.topics[0] == keccak256("Deposit(bytes32,address,uint256,uint256)")`
4. **Contract Address Verification**: Verify the event was emitted by the correct bridge contract
5. **Event Data Extraction**: Extract `depositHash`, `sender`, `amount`, `timestamp` from the event
6. **Secret Knowledge Proof**: Prove knowledge of secrets that hash to `depositHash`:
   - `commitment = Poseidon(withdrawal_hash, nullifier_preimage)`
   - Verify `commitment == depositHash`
7. **Nullifier Computation**: Compute `nullifier = Poseidon(withdrawal_hash, nullifier_preimage)`
8. **Public Outputs**: Expose `nullifier`, `recipient`, `amount`, `contract_address`

### Public Inputs

```rust
pub struct PublicInputs {
    pub nullifier: [u8; 32],           // Prevents double-spending
    pub recipient: [u8; 20],           // Withdrawal recipient
    pub amount: u64,                   // Withdrawal amount
    pub contract_address: [u8; 20],    // Bridge contract address
}
```

### Private Inputs (Witnesses)

```rust
pub struct PrivateInputs {
    // User secrets
    pub withdrawal_hash: [u8; 32],
    pub nullifier_preimage: [u8; 32],

    // Ethereum proof data
    pub event_data: DepositEventData,
    pub receipt_proof: ReceiptProof,
}
```

## Setup

### 1. Download Trusted Setup

⚠️ **Required**: Before using the prover, download the trusted setup parameters:

```bash
./download_trusted_setup.sh
```

This downloads the KZG parameters from the Perpetual Powers of Tau ceremony (~288 MB).

**Why this matters**: Using `gen_srs()` to generate random parameters is **insecure** and allows anyone to forge proofs. See [TRUSTED_SETUP.md](TRUSTED_SETUP.md) for details.

### 2. Verify the Download (Optional)

```bash
cargo run --release --example verify_trusted_setup
```

## Usage

### Generate Proving/Verifying Keys

```bash
cargo run --release -- setup --output-dir ./keys
```

### Generate a Deposit Proof

```bash
cargo run --release -- prove \
  --withdrawal-hash 0x1234... \
  --nullifier-preimage 0x5678... \
  --tx-hash 0xabcd... \
  --rpc-url https://sepolia.infura.io/v3/YOUR_KEY \
  --contract-address 0x... \
  --output proof.json
```

### Verify a Proof

```bash
cargo run --release -- verify \
  --proof proof.json \
  --vkey keys/vkey.bin
```

## Integration with Main Bridge

The deposit prover is called by the Ethereum frontend when a user wants to withdraw:

1. User deposits on Ethereum → `Deposit` event emitted
2. User runs deposit-prover to generate ZK proof of the event
3. User submits proof to Acki Nacki withdrawal contract
4. Withdrawal contract verifies the proof and releases funds

## Implementation Status

- [x] Project structure and CLI
- [x] Ethereum client for fetching events
- [ ] MPT proof generation (requires eth_getProof or manual trie building)
- [ ] RLP encoding/decoding
- [ ] Circuit implementation using axiom-eth
- [ ] Poseidon hash integration
- [ ] Proof generation and verification
- [ ] Key generation

## Dependencies

- `axiom-eth`: Ethereum state proof primitives
- `halo2-base` (axiom-crypto v0.4.1): Circuit builder
- `snark-verifier-sdk`: Proof generation
- `ethers`: Ethereum RPC client
- `zkevm-hashes`: Poseidon hash implementation

## Development

Build:

```bash
cd deposit-prover
cargo build --release
```

Test:

```bash
cargo test
```

## References

- [axiom-eth](https://github.com/axiom-crypto/axiom-eth) - Ethereum state proof library
- [Ethereum Light Client Protocol](https://github.com/ethereum/annotated-spec/blob/master/altair/sync-protocol.md)
- [Merkle-Patricia Trie](https://ethereum.org/en/developers/docs/data-structures-and-encoding/patricia-merkle-trie/)
