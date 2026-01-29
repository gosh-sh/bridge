# Deposit Event Proof Circuit Architecture

## Overview

The deposit event proof circuit is a **separate standalone binary** (`deposit-prover`) that generates ZK proofs for Ethereum Deposit events. It proves that a `Deposit` event was emitted by the AckiNackiBridge contract on Ethereum.

## Why Separate from Main Workspace?

The bridge uses **two different ZK proof systems** with incompatible dependencies:

| Component | Purpose | Halo2 Version | Location |
|-----------|---------|---------------|----------|
| **Deposit Circuit** | Prove Ethereum event emission | axiom-crypto's halo2-lib v0.4.1 | `deposit-prover/` |
| **Withdrawal Circuit** | Prove withdrawal on Acki Nacki | scroll-tech's halo2-lib | `crates/zk-proofs/` |

These two versions cannot coexist in the same Cargo workspace, so the deposit prover is a standalone package with its own `[workspace]` declaration.

## Architecture

### Two-Circuit Design

```
┌─────────────────────────────────────────────────────────────┐
│                    ETHEREUM (Source Chain)                   │
│                                                              │
│  User deposits ETH → Contract emits Deposit event           │
│                                                              │
│  Event: Deposit(depositHash, sender, amount, timestamp)     │
└──────────────────────┬───────────────────────────────────────┘
                       │
                       │ User fetches event + receipt proof
                       │
                       ▼
┌─────────────────────────────────────────────────────────────┐
│              DEPOSIT PROVER (Off-chain)                      │
│                                                              │
│  ┌────────────────────────────────────────────────────────┐ │
│  │  Deposit Event Proof Circuit (axiom-eth + halo2)       │ │
│  │                                                         │ │
│  │  Proves:                                                │ │
│  │  1. Receipt exists in Ethereum receipt trie (MPT)      │ │
│  │  2. Event log extracted from receipt (RLP decode)      │ │
│  │  3. Event emitted by correct contract                  │ │
│  │  4. User knows secrets: depositHash = Poseidon(w, n)   │ │
│  │  5. Computes nullifier = Poseidon(w, n)                │ │
│  │                                                         │ │
│  │  Public Outputs: [nullifier, recipient, amount, addr]  │ │
│  └────────────────────────────────────────────────────────┘ │
│                                                              │
│  Generates: deposit_proof.json                              │
└──────────────────────┬───────────────────────────────────────┘
                       │
                       │ User submits proof to Acki Nacki
                       │
                       ▼
┌─────────────────────────────────────────────────────────────┐
│                ACKI NACKI (Destination Chain)                │
│                                                              │
│  ┌────────────────────────────────────────────────────────┐ │
│  │  Withdrawal Circuit (scroll-tech halo2)                 │ │
│  │                                                         │ │
│  │  Verifies deposit proof + generates withdrawal proof   │ │
│  │  Contract checks nullifier not used                    │ │
│  │  Releases funds to recipient                           │ │
│  └────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
```

## Deposit Circuit Details

### Circuit Logic

The deposit event proof circuit (in `deposit-prover/src/circuit.rs`) proves:

1. **Receipt Trie Inclusion**
   - Verify transaction receipt exists in Ethereum's receipt trie
   - Use Merkle-Patricia Trie (MPT) proof
   - Implemented using axiom-eth's MPT verification components

2. **Event Log Extraction**
   - Parse RLP-encoded receipt
   - Extract logs array
   - Find Deposit event at specified log_index
   - Implemented using axiom-eth's RLP decoder

3. **Event Signature Verification**
   - Compute `keccak256("Deposit(bytes32,address,uint256,uint256)")`
   - Verify `log.topics[0] == event_signature`
   - Implemented using axiom-eth's keccak chip

4. **Contract Address Verification**
   - Verify `log.address == contract_address` (public input)
   - Ensures event was emitted by the correct bridge contract

5. **Event Data Extraction**
   - `depositHash = log.topics[1]` (indexed parameter)
   - `sender = log.topics[2]` (indexed parameter)
   - `amount = decode_uint256(log.data[0:32])` (non-indexed)
   - `timestamp = decode_uint256(log.data[32:64])` (non-indexed)

6. **Secret Knowledge Proof**
   - User provides: `withdrawal_hash`, `nullifier_preimage` (private)
   - Compute: `commitment = Poseidon(withdrawal_hash, nullifier_preimage)`
   - Verify: `commitment == depositHash`
   - Implemented using zkevm-hashes Poseidon

7. **Nullifier Computation**
   - Compute: `nullifier = Poseidon(withdrawal_hash, nullifier_preimage)`
   - Same as commitment in our design
   - Exposed as public output

8. **Public Outputs**
   - `nullifier`: Prevents double-spending
   - `recipient`: Withdrawal recipient address
   - `amount`: Withdrawal amount
   - `contract_address`: Bridge contract address

### Public Inputs

```rust
pub struct PublicInputs {
    pub nullifier: [u8; 32],           // Fr field element
    pub recipient: [u8; 20],           // Ethereum address
    pub amount: u64,                   // Wei amount
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
    pub block_number: u64,
    pub transaction_index: u64,
    pub log_index: usize,
    pub receipt_rlp: Vec<u8>,
    pub receipt_proof: Vec<Vec<u8>>,  // MPT proof nodes
    pub receipt_root: [u8; 32],       // From block header
    pub block_header_rlp: Vec<u8>,
}
```

## Dependencies

### Deposit Prover (`deposit-prover/`)

```toml
axiom-eth = { git = "https://github.com/axiom-crypto/axiom-eth" }
halo2-base = { git = "https://github.com/axiom-crypto/halo2-lib.git", tag = "v0.4.1-git" }
halo2-ecc = { git = "https://github.com/axiom-crypto/halo2-lib.git", tag = "v0.4.1-git" }
snark-verifier = { git = "https://github.com/axiom-crypto/snark-verifier.git", tag = "v0.1.7-git" }
zkevm-hashes = { git = "https://github.com/axiom-crypto/halo2-lib.git", tag = "v0.4.1-git" }
```

### Withdrawal Circuit (`crates/zk-proofs/`)

```toml
halo2-base = { git = "https://github.com/scroll-tech/halo2-lib", branch = "develop" }
halo2-ecc = { git = "https://github.com/scroll-tech/halo2-lib", branch = "develop" }
snark-verifier = { git = "https://github.com/scroll-tech/snark-verifier", branch = "develop" }
poseidon = { git = "https://github.com/scroll-tech/poseidon.git" }
```

## Usage Flow

### 1. User Deposits on Ethereum

```solidity
// User calls bridge.deposit()
bridge.deposit(depositHash, amount);

// Contract emits event
emit Deposit(depositHash, msg.sender, amount, block.timestamp);
```

### 2. User Generates Deposit Proof

```bash
cd deposit-prover

# Generate proof
cargo run --release -- prove \
  --withdrawal-hash 0x1234... \
  --nullifier-preimage 0x5678... \
  --tx-hash 0xabcd... \
  --rpc-url https://sepolia.infura.io/v3/YOUR_KEY \
  --contract-address 0x... \
  --output deposit_proof.json
```

This:
- Fetches transaction receipt from Ethereum RPC
- Finds Deposit event in receipt logs
- Generates MPT proof for receipt inclusion
- Generates ZK proof using the circuit
- Outputs proof + public inputs to JSON file

### 3. User Submits to Acki Nacki

```rust
// User submits deposit proof to Acki Nacki withdrawal contract
withdrawal_contract.withdraw(
    recipient,
    amount,
    nullifier,
    deposit_proof  // From deposit_proof.json
);
```

The withdrawal contract:
- Verifies the deposit proof
- Checks nullifier hasn't been used
- Marks nullifier as used
- Transfers funds to recipient

## Implementation Status

### Completed ✅

- [x] Project structure for `deposit-prover/`
- [x] CLI interface (prove, setup, verify commands)
- [x] Type definitions (DepositEventData, ReceiptProof, etc.)
- [x] Ethereum client skeleton (fetch events)
- [x] Circuit structure and documentation
- [x] Separate workspace configuration

### In Progress 🚧

- [ ] MPT proof generation (requires eth_getProof or manual trie building)
- [ ] RLP encoding/decoding for receipts
- [ ] Circuit implementation using axiom-eth components
- [ ] Poseidon hash integration (zkevm-hashes)
- [ ] Proof generation using snark-verifier-sdk
- [ ] Key generation (setup command)
- [ ] Proof verification (verify command)

### Blocked ⏸️

- Dependency version conflicts (ethers, ruint)
- Need to resolve axiom-eth compatibility issues

## Next Steps

1. **Fix dependency conflicts**
   - Resolve ethers version issue
   - Ensure all axiom-eth dependencies are compatible

2. **Implement MPT proof generation**
   - Use `eth_getProof` RPC method, or
   - Build receipt trie manually from all receipts in block

3. **Implement RLP encoding/decoding**
   - Use axiom-eth's RLP components
   - Encode receipts and decode logs

4. **Implement circuit synthesis**
   - Use halo2-base's CircuitBuilder
   - Integrate axiom-eth chips (MPT, RLP, keccak)
   - Integrate zkevm-hashes for Poseidon

5. **Implement proof generation**
   - Generate proving/verifying keys
   - Create proof using snark-verifier-sdk
   - Serialize proof to JSON

6. **Test end-to-end**
   - Deploy test contract on Sepolia
   - Make test deposit
   - Generate proof
   - Verify proof on-chain

## References

- [axiom-eth GitHub](https://github.com/axiom-crypto/axiom-eth)
- [Ethereum MPT Specification](https://ethereum.org/en/developers/docs/data-structures-and-encoding/patricia-merkle-trie/)
- [Ethereum Receipt Structure](https://ethereum.org/en/developers/docs/data-structures-and-encoding/rlp/)
- [halo2-lib Documentation](https://github.com/axiom-crypto/halo2-lib)

