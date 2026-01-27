# Deposit Event Proof Circuit Design

## Overview

This document describes the design of the deposit event proof circuit using axiom-eth.

## Circuit Architecture

Based on axiom-eth's architecture, we will use the **Component Framework** approach:

```
DepositEventCircuit
├── Phase 0: Receipt MPT Proof
│   ├── Load receipt proof nodes
│   ├── Verify MPT inclusion
│   ├── Extract receipt RLP
│   └── Decode receipt to get logs
│
└── Phase 1: Event Verification + Secret Proof
    ├── Find Deposit event in logs
    ├── Verify event signature (keccak)
    ├── Extract event data (depositHash, sender, amount, timestamp)
    ├── Verify contract address
    ├── Prove secret knowledge: Poseidon(w, n) == depositHash
    └── Compute nullifier: Poseidon(w, n)
```

## Axiom-eth Components Used

### 1. **MPTChip** (Receipt Trie Verification)

```rust
use axiom_eth::mpt::MPTChip;

// Verify receipt is in receipt trie
let mpt_chip = MPTChip::new(rlp_chip, keccak_chip);
let receipt_rlp = mpt_chip.parse_mpt_inclusion_proof(
    ctx,
    receipt_proof_nodes,
    receipt_root,
    tx_index_rlp
);
```

### 2. **RlpChip** (RLP Decoding)

```rust
use axiom_eth::rlp::RlpChip;

// Decode receipt to extract logs
let rlp_chip = RlpChip::new(range_chip, max_len);
let receipt_fields = rlp_chip.decompose_rlp_array_phase0(
    ctx,
    receipt_rlp,
    &[status_len, gas_len, bloom_len, logs_len],
    false
);
```

### 3. **KeccakChip** (Event Signature Verification)

```rust
use axiom_eth::keccak::KeccakChip;

// Verify event signature
let keccak_chip = KeccakChip::new(range_chip);
let event_sig = keccak_chip.keccak_fixed_len(
    ctx,
    "Deposit(bytes32,address,uint256,uint256)".as_bytes()
);
// Compare with log.topics[0]
```

### 4. **Poseidon Hash** (Secret Proof + Nullifier)

```rust
use zkevm_hashes::poseidon::PoseidonChip;

// Prove secret knowledge
let poseidon_chip = PoseidonChip::new(ctx, spec);
let commitment = poseidon_chip.hash_fix_len_array(
    ctx,
    &[withdrawal_hash, nullifier_preimage]
);
// Verify commitment == depositHash

// Compute nullifier (same as commitment in our design)
let nullifier = commitment;
```

## Circuit Inputs

### Private Inputs (Witnesses)

```rust
pub struct DepositCircuitInput {
    // User secrets
    withdrawal_hash: Fr,
    nullifier_preimage: Fr,
    
    // Receipt proof
    receipt_rlp: Vec<u8>,
    receipt_proof_nodes: Vec<Vec<u8>>,  // MPT proof
    receipt_root: [u8; 32],             // From block header
    tx_index: u64,
    
    // Event data (for witness generation)
    log_index: usize,
    deposit_hash: [u8; 32],
    sender: [u8; 20],
    amount: u64,
    timestamp: u64,
    contract_address: [u8; 20],
}
```

### Public Inputs (Outputs)

```rust
pub struct DepositCircuitOutput {
    nullifier: Fr,              // Prevents double-spending
    recipient: Fr,              // Withdrawal recipient (from sender)
    amount: Fr,                 // Withdrawal amount
    contract_address: Fr,       // Bridge contract address
}
```

## Circuit Logic

### Phase 0: MPT Verification

1. **Load receipt proof**
   - Load receipt RLP as bytes
   - Load MPT proof nodes
   - Load receipt root from block header

2. **Verify MPT inclusion**
   - Encode tx_index as RLP
   - Verify MPT proof: `verify_mpt_proof(proof_nodes, receipt_root, tx_index_rlp, receipt_rlp)`
   - This proves the receipt exists in the receipt trie

3. **Decode receipt**
   - Parse RLP structure: `[status, cumulativeGasUsed, logsBloom, logs]`
   - Extract logs array
   - Find log at `log_index`

4. **Extract log fields**
   - Parse log structure: `[address, topics, data]`
   - Extract `address` (contract address)
   - Extract `topics[0]` (event signature)
   - Extract `topics[1]` (depositHash)
   - Extract `topics[2]` (sender)
   - Extract `data[0:32]` (amount)
   - Extract `data[32:64]` (timestamp)

### Phase 1: Event Verification + Secret Proof

5. **Verify event signature**
   - Compute `expected_sig = keccak256("Deposit(bytes32,address,uint256,uint256)")`
   - Constrain `topics[0] == expected_sig`

6. **Verify contract address**
   - Constrain `log.address == contract_address` (public input)

7. **Prove secret knowledge**
   - Compute `commitment = Poseidon(withdrawal_hash, nullifier_preimage)`
   - Constrain `commitment == depositHash` (from topics[1])

8. **Compute nullifier**
   - `nullifier = Poseidon(withdrawal_hash, nullifier_preimage)`
   - Same as commitment in our design

9. **Expose public outputs**
   - `nullifier` (Fr)
   - `recipient` (Fr, derived from sender)
   - `amount` (Fr)
   - `contract_address` (Fr)

## Implementation Strategy

### Option 1: RlcKeccakCircuitImpl (Simpler, Standalone)

Use `RlcKeccakCircuitImpl` for a standalone circuit:

```rust
impl EthCircuitInstructions for DepositEventCircuit {
    fn virtual_assign_phase0(&self, builder: &mut RlcCircuitBuilder) {
        // MPT verification
        // RLP decoding
        // Extract event data
    }
    
    fn virtual_assign_phase1(&self, builder: &mut RlcCircuitBuilder, challenge: Value<Fr>) {
        // Event signature verification (keccak)
        // Secret proof (Poseidon)
        // Nullifier computation
    }
}
```

**Pros:**
- Simpler to implement
- Standalone circuit (no aggregation needed)
- Keccak is directly constrained

**Cons:**
- Larger circuit (includes keccak sub-circuit)
- Slower proving time

### Option 2: EthCircuitImpl (Optimized, Requires Aggregation)

Use `EthCircuitImpl` with keccak table lookup:

```rust
impl EthCircuitInstructions for DepositEventCircuit {
    // Same as Option 1, but keccak is looked up in a table
}
```

**Pros:**
- Smaller circuit (keccak is in separate circuit)
- Faster proving time

**Cons:**
- Requires aggregation circuit
- More complex setup

### Recommendation: Start with Option 1

For initial implementation, use `RlcKeccakCircuitImpl` for simplicity. We can optimize later with `EthCircuitImpl` if needed.

## Circuit Parameters

Based on axiom-eth examples:

```rust
const K: u32 = 18;  // Circuit size (2^18 rows)
const LOOKUP_BITS: usize = 16;  // Lookup table size
const MAX_RECEIPT_LEN: usize = 2048;  // Max receipt size
const MAX_PROOF_DEPTH: usize = 10;  // Max MPT proof depth
```

## Testing Strategy

1. **Unit tests**: Test individual components (MPT, RLP, Poseidon)
2. **Integration test**: Test full circuit with real Sepolia data
3. **Proof generation**: Generate actual proof and verify
4. **Gas cost**: Measure on-chain verification cost

## Next Steps

1. Implement basic circuit structure using `RlcKeccakCircuitImpl`
2. Implement MPT verification using `MPTChip`
3. Implement RLP decoding using `RlpChip`
4. Implement event verification using `KeccakChip`
5. Implement secret proof using Poseidon
6. Generate proving/verifying keys
7. Test with real Sepolia deposit transaction
8. Generate Solidity verifier
9. Deploy and test on-chain

## References

- [axiom-eth README](https://github.com/axiom-crypto/axiom-eth/blob/main/axiom-eth/README.md)
- [axiom-eth examples](https://github.com/axiom-crypto/axiom-eth/tree/main/axiom-query/examples)
- [halo2-lib](https://github.com/axiom-crypto/halo2-lib)
- [Component Framework](https://github.com/axiom-crypto/axiom-eth/blob/main/axiom-eth/src/utils/README.md)

