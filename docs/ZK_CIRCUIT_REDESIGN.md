# ZK Circuit Redesign for Event-Based Proofs

## Overview

This document outlines the redesign of the withdrawal ZK circuit to prove Ethereum event emissions instead of Merkle tree inclusion.

## Current Status

✅ **Contract Updated**: `AckiNackiBridge.sol` has been simplified to remove Merkle tree logic
🔄 **Circuit Design**: In progress - designing event proof circuit
⏳ **Implementation**: Pending - waiting for circuit design completion
⏳ **Testing**: Pending - will update tests after implementation

## Architecture Decision

### Option 1: Use Axiom-eth Library (RECOMMENDED)

**Status**: axiom-eth repository is archived (Feb 4, 2025) but code is still usable

**Approach**:
- Fork or vendor the axiom-eth crates into our project
- Use `axiom-eth` crate for Ethereum state proof primitives
- Leverage existing receipt trie verification logic
- Build custom circuit on top of axiom-eth components

**Pros**:
- Battle-tested code from production Axiom system
- Comprehensive Ethereum state proof primitives
- Well-documented circuit patterns
- Handles RLP decoding, MPT verification, block header validation

**Cons**:
- Repository is archived (no active maintenance)
- May need to update dependencies ourselves
- Large dependency footprint

**Implementation Plan**:
1. Add axiom-eth as git dependency or vendor the code
2. Study axiom-eth's receipt proof circuits
3. Create custom circuit that:
   - Verifies receipt trie inclusion
   - Extracts event log from receipt
   - Verifies log.address == bridge contract
   - Verifies log topics match Deposit event signature
   - Extracts depositHash, sender, amount from log data
   - Computes nullifier from private inputs
   - Outputs: nullifier, recipient, amount, contractAddress

### Option 2: Build Custom Receipt Proof Circuit

**Approach**:
- Implement receipt trie verification from scratch using halo2-lib
- Implement RLP decoding in-circuit
- Implement Merkle-Patricia Trie verification
- Build event log extraction and verification

**Pros**:
- Full control over circuit design
- Minimal dependencies
- Can optimize for our specific use case

**Cons**:
- Significant development effort (weeks/months)
- Risk of bugs in cryptographic code
- Need to implement complex Ethereum data structures
- Reinventing the wheel

**Not Recommended**: Too much work for uncertain benefit

### Option 3: Hybrid Approach

**Approach**:
- Use axiom-eth code as reference
- Extract only the components we need
- Simplify for our specific use case
- Maintain our own fork

**Pros**:
- Smaller dependency footprint than Option 1
- Less work than Option 2
- Can customize for our needs

**Cons**:
- Still need to understand axiom-eth internals
- Maintenance burden
- May miss optimizations from full axiom-eth

## Recommended Approach: Option 1 (Use Axiom-eth)

We will use axiom-eth as a library dependency. Even though it's archived, the code is stable and production-tested.

## Circuit Design

### Public Inputs

```rust
pub struct WithdrawalPublicInputs {
    pub nullifier: Fr,           // Prevents double-spending
    pub recipient: Fr,           // Withdrawal recipient address (as field element)
    pub amount: Fr,              // Withdrawal amount (as field element)
    pub contract_address: Fr,    // Bridge contract address (as field element)
}
```

### Private Inputs (Witnesses)

```rust
pub struct WithdrawalWitness {
    // User secrets
    pub withdrawal_hash: Fr,
    pub nullifier_preimage: Fr,
    
    // Ethereum proof data
    pub block_number: u64,
    pub block_header: BlockHeader,
    pub receipt: Receipt,
    pub receipt_proof: Vec<Vec<u8>>,  // MPT proof for receipt
    pub log_index: usize,              // Index of Deposit event in receipt
}
```

### Circuit Logic

The circuit must prove:

1. **Block Header Validity** (optional - can assume finalized blocks)
   - Block header is well-formed
   - Block number is within acceptable range
   - Block is old enough (finality)

2. **Receipt Trie Inclusion**
   - Receipt is included in block's receipt trie
   - Receipt root in block header matches computed root
   - MPT proof is valid

3. **Event Log Verification**
   - Receipt contains a log at log_index
   - Log address == contract_address (public input)
   - Log topics[0] == keccak256("Deposit(bytes32,address,uint256,uint256)")
   - Extract from log:
     - depositHash = topics[1]
     - sender = topics[2]
     - amount = data[0:32]
     - timestamp = data[32:64]

4. **Secret Knowledge Proof**
   - Compute: commitment = Poseidon(withdrawal_hash, nullifier_preimage)
   - Verify: commitment == depositHash (from event log)
   - Compute: nullifier = Poseidon(withdrawal_hash, nullifier_preimage)
   - Output nullifier as public output

5. **Amount Consistency**
   - amount (from event log) == amount (public input)

### Circuit Constraints

```rust
// Pseudo-code for circuit constraints

// 1. Verify receipt trie inclusion
let receipt_root = verify_mpt_proof(receipt, receipt_proof);
assert_equal(receipt_root, block_header.receipts_root);

// 2. Extract event log
let log = receipt.logs[log_index];

// 3. Verify log address
assert_equal(log.address, contract_address);

// 4. Verify event signature
let deposit_event_sig = keccak256("Deposit(bytes32,address,uint256,uint256)");
assert_equal(log.topics[0], deposit_event_sig);

// 5. Extract event data
let depositHash = log.topics[1];
let sender = log.topics[2];
let amount_from_log = decode_uint256(log.data[0:32]);
let timestamp = decode_uint256(log.data[32:64]);

// 6. Verify secret knowledge
let commitment = poseidon_hash(withdrawal_hash, nullifier_preimage);
assert_equal(commitment, depositHash);

// 7. Compute nullifier
let computed_nullifier = poseidon_hash(withdrawal_hash, nullifier_preimage);
assert_equal(computed_nullifier, nullifier);  // nullifier is public input

// 8. Verify amount consistency
assert_equal(amount_from_log, amount);  // amount is public input
```

## Implementation Steps

### Phase 1: Setup Axiom-eth Dependency

- [ ] Add axiom-eth to Cargo.toml as git dependency
- [ ] Verify axiom-eth compiles with our project
- [ ] Study axiom-eth examples and documentation
- [ ] Identify which axiom-eth components we need

### Phase 2: Build Receipt Proof Circuit

- [ ] Create new circuit struct `EventProofCircuit`
- [ ] Implement receipt trie verification using axiom-eth
- [ ] Implement event log extraction
- [ ] Implement event signature verification
- [ ] Test with real Ethereum receipt data

### Phase 3: Integrate Secret Proof

- [ ] Add Poseidon hash computation (already have this)
- [ ] Combine receipt proof with secret knowledge proof
- [ ] Verify nullifier computation
- [ ] Test end-to-end circuit

### Phase 4: Proof Generation

- [ ] Update `crates/verifier-generator/src/proof_gen.rs`
- [ ] Add command to generate event-based withdrawal proof
- [ ] Fetch receipt data from Ethereum RPC
- [ ] Generate MPT proof for receipt
- [ ] Generate ZK proof

### Phase 5: Verifier Update

- [ ] Generate new verifier contract
- [ ] Update `IAckiNackiVerifier.sol` interface if needed
- [ ] Deploy new verifier
- [ ] Update bridge contract to use new verifier

### Phase 6: Testing

- [ ] Unit tests for circuit components
- [ ] Integration tests with real Ethereum data
- [ ] Update Solidity tests
- [ ] End-to-end test: deposit → event → proof → withdrawal

## Ethereum Data Structures

### Receipt Structure

```solidity
struct Receipt {
    uint8 status;           // 1 = success, 0 = failure
    uint256 cumulativeGasUsed;
    bytes32 logsBloom;
    Log[] logs;
}

struct Log {
    address address;        // Contract that emitted the log
    bytes32[] topics;       // Indexed event parameters
    bytes data;             // Non-indexed event parameters
}
```

### Deposit Event Encoding

```solidity
event Deposit(
    bytes32 indexed depositHash,    // topics[1]
    address indexed sender,          // topics[2]
    uint256 amount,                  // data[0:32]
    uint256 timestamp                // data[32:64]
);

// topics[0] = keccak256("Deposit(bytes32,address,uint256,uint256)")
//           = 0x...  (computed off-chain)
```

### Receipt Trie

Ethereum stores receipts in a Merkle-Patricia Trie:
- Key: RLP(transaction_index)
- Value: RLP(receipt)
- Root: block_header.receipts_root

To prove a receipt exists:
1. Provide the receipt data
2. Provide MPT proof (list of trie nodes from root to leaf)
3. Verify in circuit that proof is valid

## Gas Cost Analysis

### Current (Merkle Tree)

- Deposit: ~100k gas (tree updates)
- Withdrawal: ~200k gas (proof verification)

### New (Event-Based)

- Deposit: ~50k gas (just emit event)
- Withdrawal: ~250k gas (more complex proof verification)

**Trade-off**: Deposits become cheaper, withdrawals become slightly more expensive. Overall gas savings because deposits are more frequent.

## Security Considerations

### Attack Vectors

1. **Fake Event**: Attacker creates fake event in different contract
   - **Mitigation**: Circuit verifies log.address == bridge contract

2. **Wrong Event**: Attacker uses different event signature
   - **Mitigation**: Circuit verifies topics[0] == Deposit event signature

3. **Replayed Event**: Attacker reuses same event multiple times
   - **Mitigation**: Nullifier prevents double-spending (same as before)

4. **Reorganization**: Event gets reorged out of chain
   - **Mitigation**: Only accept events from finalized blocks (>64 blocks old)

5. **Modified Event Data**: Attacker modifies amount or depositHash
   - **Mitigation**: Receipt trie proof ensures data integrity

### Security Properties

✅ **Privacy**: Deposit and withdrawal unlinkable (same as before)
✅ **Double-spend prevention**: Nullifiers prevent reuse (same as before)
✅ **Proof soundness**: ZK proof ensures valid withdrawal (same as before)
✅ **Event authenticity**: Receipt trie proof ensures event was actually emitted
✅ **Contract binding**: Circuit verifies event from correct contract
✅ **Data integrity**: MPT proof ensures event data not modified

## Next Steps

1. **Add axiom-eth dependency** to Cargo.toml
2. **Study axiom-eth examples** to understand receipt proof patterns
3. **Create prototype circuit** with simplified receipt verification
4. **Test with real data** from Sepolia testnet
5. **Implement full circuit** with all security checks
6. **Update proof generator** to fetch receipt data and generate proofs
7. **Update tests** to use new proof format
8. **Deploy and test** on testnet

## References

- [Axiom-eth Repository](https://github.com/axiom-crypto/axiom-eth)
- [Ethereum Yellow Paper](https://ethereum.github.io/yellowpaper/paper.pdf) - Section 4.3 (Receipts)
- [Ethereum MPT Specification](https://ethereum.org/en/developers/docs/data-structures-and-encoding/patricia-merkle-trie/)
- [Halo2 Book](https://zcash.github.io/halo2/)
- [RLP Encoding](https://ethereum.org/en/developers/docs/data-structures-and-encoding/rlp/)

