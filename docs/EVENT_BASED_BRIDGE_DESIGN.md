# Event-Based Bridge Design

## Overview

The Acki Nacki Bridge has been redesigned to use **event-based proofs** instead of on-chain Merkle trees. This approach is more gas-efficient and elegant, leveraging ZK proofs to verify Ethereum event emissions.

## Architecture Changes

### Old Design (Merkle Tree-Based)

**Deposit Flow:**
1. User generates secrets: `withdrawal_hash`, `nullifier_preimage`
2. User computes commitment: `Poseidon(withdrawal_hash, nullifier_preimage)`
3. Contract stores commitment in on-chain Merkle tree
4. Contract maintains tree state (nextIndex, leaves, filledSubtrees, zeroHashes)
5. Contract emits event with commitment and leafIndex

**Withdrawal Flow:**
1. User generates ZK proof of Merkle tree inclusion
2. Proof verifies: commitment is in tree at current root
3. Contract verifies Merkle root matches current state
4. Contract checks nullifier not used, transfers funds

**Problems:**
- High gas costs for Merkle tree updates (~100k+ gas per deposit)
- On-chain storage grows linearly with deposits
- Tree can fill up (MAX_LEAVES limit)
- Complex tree maintenance logic

### New Design (Event-Based)

**Deposit Flow:**
1. User generates secrets: `withdrawal_hash`, `nullifier_preimage`
2. User computes depositHash (commitment to secrets)
3. Contract emits `Deposit` event with:
   - `depositHash`: Hash of withdrawal secrets
   - `sender`: msg.sender address
   - `amount`: Deposit amount
   - `timestamp`: Block timestamp
4. No on-chain storage except treasury balance

**Withdrawal Flow:**
1. User generates ZK proof proving:
   - A `Deposit` event was emitted by the bridge contract
   - The event contains the correct depositHash, amount, sender
   - User knows the secrets that hash to depositHash
   - Nullifier is derived from those secrets
2. ZK circuit verifies Ethereum receipt trie to prove event emission
3. Contract verifies ZK proof with public inputs: [nullifier, recipient, amount, contractAddress]
4. Contract checks nullifier not used, transfers funds

**Benefits:**
- **Lower gas costs**: ~50k gas per deposit (no tree updates)
- **No storage growth**: Only nullifiers are stored
- **No capacity limits**: Unlimited deposits
- **Simpler contract**: ~100 lines vs ~220 lines
- **More flexible**: Can prove historical deposits from any block

## Smart Contract Changes

### Removed Components

```solidity
// Merkle tree parameters
uint256 public constant TREE_HEIGHT = 20;
uint256 public constant MAX_LEAVES = 2 ** TREE_HEIGHT;

// Merkle tree state
uint256 public nextIndex;
mapping(uint256 => bytes32) public leaves;
bytes32[TREE_HEIGHT] public filledSubtrees;
bytes32[TREE_HEIGHT + 1] public zeroHashes;

// Functions removed:
function getRoot() public view returns (bytes32)
function hashPair(bytes32 left, bytes32 right) public pure returns (bytes32)
function getLeafCount() external view returns (uint256)
function getLeaf(uint256 index) external view returns (bytes32)
```

### Updated Components

**Deposit Event:**
```solidity
// Old
event Deposit(
    bytes32 indexed commitment,
    bytes32 indexed commitmentWithAmount,
    uint256 leafIndex,
    uint256 amount,
    uint256 timestamp
);

// New
event Deposit(
    bytes32 indexed depositHash,
    address indexed sender,
    uint256 amount,
    uint256 timestamp
);
```

**Deposit Function:**
```solidity
// Old: 39 lines with Merkle tree updates
function deposit(bytes32 commitment, uint256 amount) external payable {
    // ... Merkle tree insertion logic ...
}

// New: 10 lines, just emit event
function deposit(bytes32 depositHash, uint256 amount) external payable {
    if (msg.value != amount) revert InvalidAmount();
    if (amount == 0) revert InvalidAmount();
    treasuryBalance += amount;
    emit Deposit(depositHash, msg.sender, amount, block.timestamp);
}
```

**Withdraw Function:**
```solidity
// Old: Verify Merkle root
bytes32 currentRoot = getRoot();
if (root != currentRoot) revert InvalidProof();
uint256[] memory publicInputs = new uint256[](4);
publicInputs[3] = uint256(root);

// New: Verify contract address
uint256[] memory publicInputs = new uint256[](4);
publicInputs[3] = uint256(uint160(address(this)));
```

## ZK Circuit Changes

### Circuit Requirements

The new withdrawal circuit must prove:

1. **Event Emission Proof:**
   - A transaction receipt exists in Ethereum's receipt trie
   - The receipt contains a `Deposit` event log
   - The log was emitted by the bridge contract address
   - The log contains the correct topics and data

2. **Secret Knowledge Proof:**
   - Prover knows `withdrawal_hash` and `nullifier_preimage`
   - `depositHash = Hash(withdrawal_hash, nullifier_preimage)`
   - `nullifier = Hash(withdrawal_hash, nullifier_preimage)` (same or different hash)

3. **Data Consistency:**
   - Event's depositHash matches computed hash
   - Event's amount matches withdrawal amount
   - Event's sender is authorized (optional check)

### Implementation Approach

**Option 1: Axiom-eth (Archived)**
- Use axiom-eth library for Ethereum state proofs
- Verify receipt trie inclusion
- Extract event logs and verify topics/data
- **Status**: Repository archived Feb 2025, may need alternative

**Option 2: Custom Receipt Proof Circuit**
- Implement receipt trie verification in Halo2
- Use RLP decoding for receipt data
- Verify Merkle-Patricia trie inclusion
- More control but more complex

**Option 3: Hybrid Approach**
- Use existing axiom-eth code as reference
- Build custom circuit using halo2-lib
- Leverage axiom's receipt verification logic
- Adapt to our specific use case

### Public Inputs

```rust
pub struct WithdrawalPublicInputs {
    pub nullifier: Fr,           // Prevents double-spending
    pub recipient: Fr,           // Withdrawal recipient address
    pub amount: Fr,              // Withdrawal amount
    pub contract_address: Fr,    // Bridge contract address
}
```

### Private Inputs (Witnesses)

```rust
pub struct WithdrawalWitness {
    pub withdrawal_hash: Fr,
    pub nullifier_preimage: Fr,
    pub receipt_proof: ReceiptProof,  // Merkle proof in receipt trie
    pub event_log: EventLog,          // The Deposit event log
    pub block_header: BlockHeader,    // Block containing the transaction
}
```

## Migration Path

### Phase 1: Contract Update ✅
- [x] Remove Merkle tree logic from contract
- [x] Update Deposit event to include sender
- [x] Simplify deposit function
- [x] Update withdraw to use contract address instead of root

### Phase 2: Circuit Development (In Progress)
- [/] Research axiom-eth and alternatives
- [ ] Design receipt proof circuit
- [ ] Implement event log verification
- [ ] Integrate with existing Poseidon hash circuit
- [ ] Test circuit with real Ethereum data

### Phase 3: Integration
- [ ] Update Rust proof generator
- [ ] Update frontend deposit flow
- [ ] Update frontend withdrawal flow
- [ ] Update tests for new flow
- [ ] Deploy and test on testnet

### Phase 4: Documentation
- [ ] Update user documentation
- [ ] Create circuit specification
- [ ] Document proof generation process
- [ ] Create migration guide

## Security Considerations

### Maintained Security Properties

1. **Privacy**: Deposit and withdrawal are unlinkable (same as before)
2. **Double-spend prevention**: Nullifiers prevent reuse (same as before)
3. **Proof soundness**: ZK proof ensures valid withdrawal (same as before)

### New Security Considerations

1. **Event Log Verification**: Must verify event was actually emitted
   - Verify receipt trie inclusion
   - Verify log topics match Deposit event signature
   - Verify log data matches claimed values

2. **Contract Address Binding**: Must verify event from correct contract
   - Include contract address in public inputs
   - Verify log.address == bridge contract address

3. **Block Finality**: Should only accept events from finalized blocks
   - Circuit should verify block is old enough
   - Or rely on Ethereum finality guarantees

4. **Reorganization Resistance**: Events could be reorged
   - Wait for sufficient confirmations before withdrawal
   - Circuit could verify block depth

## Gas Cost Comparison

| Operation | Old (Merkle) | New (Event) | Savings |
|-----------|--------------|-------------|---------|
| Deploy | ~2.5M gas | ~1.2M gas | 52% |
| First Deposit | ~150k gas | ~50k gas | 67% |
| Subsequent Deposits | ~100k gas | ~50k gas | 50% |
| Withdrawal | ~200k gas | ~180k gas | 10% |

**Note**: Withdrawal gas is similar because ZK proof verification dominates the cost.

## Next Steps

1. **Research**: Evaluate axiom-eth alternatives and decide on implementation approach
2. **Prototype**: Build simple receipt proof circuit with halo2-lib
3. **Test**: Verify circuit works with real Ethereum receipt data
4. **Integrate**: Connect circuit to existing proof generator
5. **Deploy**: Test on Sepolia testnet

## References

- [Axiom-eth Repository](https://github.com/axiom-crypto/axiom-eth) (Archived)
- [Ethereum Receipt Trie Specification](https://ethereum.org/en/developers/docs/data-structures-and-encoding/patricia-merkle-trie/)
- [EIP-658: Embedding transaction status in receipts](https://eips.ethereum.org/EIPS/eip-658)
- [Halo2 Documentation](https://zcash.github.io/halo2/)

