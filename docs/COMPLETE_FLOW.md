# Complete Bridge Flow with Deposit Event Proofs

## Overview

This document describes the complete end-to-end flow of the Acki Nacki Bridge using the new event-based deposit proof system.

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         ETHEREUM                                 │
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │  AckiNackiBridge Contract                                  │ │
│  │                                                             │ │
│  │  function deposit(bytes32 depositHash, uint256 amount)     │ │
│  │  {                                                          │ │
│  │      treasuryBalance += amount;                            │ │
│  │      emit Deposit(depositHash, msg.sender, amount, time);  │ │
│  │  }                                                          │ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                  │
└──────────────────────────┬───────────────────────────────────────┘
                           │
                           │ Event emitted
                           │
                           ▼
┌─────────────────────────────────────────────────────────────────┐
│                    USER'S MACHINE                                │
│                                                                  │
│  Step 1: Generate secrets                                       │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │  withdrawal_hash = random()                                 │ │
│  │  nullifier_preimage = random()                              │ │
│  │  depositHash = Poseidon(withdrawal_hash, nullifier_preimage)│ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                  │
│  Step 2: Deposit on Ethereum                                    │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │  bridge.deposit(depositHash, 1 ETH)                         │ │
│  │  → Transaction mined in block N                             │ │
│  │  → Event: Deposit(depositHash, user, 1 ETH, timestamp)      │ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                  │
│  Step 3: Generate deposit proof                                 │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │  deposit-prover prove \                                     │ │
│  │    --withdrawal-hash 0x... \                                │ │
│  │    --nullifier-preimage 0x... \                             │ │
│  │    --tx-hash 0x... \                                        │ │
│  │    --rpc-url https://sepolia.infura.io/... \                │ │
│  │    --contract-address 0x... \                               │ │
│  │    --output deposit_proof.json                              │ │
│  │                                                              │ │
│  │  This generates a ZK proof that:                            │ │
│  │  - Event was emitted by the contract                        │ │
│  │  - User knows secrets that hash to depositHash              │ │
│  │  - Computes nullifier = Poseidon(w, n)                      │ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                  │
└──────────────────────────┬───────────────────────────────────────┘
                           │
                           │ Submit proof
                           │
                           ▼
┌─────────────────────────────────────────────────────────────────┐
│                      ACKI NACKI                                  │
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │  Withdrawal Contract                                        │ │
│  │                                                             │ │
│  │  function withdraw(                                         │ │
│  │      address recipient,                                     │ │
│  │      uint256 amount,                                        │ │
│  │      bytes32 nullifier,                                     │ │
│  │      bytes calldata depositProof                            │ │
│  │  ) {                                                        │ │
│  │      // Verify deposit proof                                │ │
│  │      require(verifyDepositProof(depositProof));             │ │
│  │                                                             │ │
│  │      // Check nullifier not used                            │ │
│  │      require(!usedNullifiers[nullifier]);                   │ │
│  │      usedNullifiers[nullifier] = true;                      │ │
│  │                                                             │ │
│  │      // Transfer funds                                      │ │
│  │      recipient.transfer(amount);                            │ │
│  │  }                                                          │ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

## Detailed Steps

### 1. User Prepares Deposit

**Location:** User's wallet (Electron app or CLI)

```javascript
// Generate random secrets
const withdrawal_hash = randomBytes(32);
const nullifier_preimage = randomBytes(32);

// Compute commitment using Rust binary
const { commitment } = await computeCommitment(
    withdrawal_hash,
    nullifier_preimage
);

// Save secrets securely
saveSecrets({
    withdrawal_hash,
    nullifier_preimage,
    commitment
});
```

### 2. User Deposits on Ethereum

**Location:** Ethereum blockchain

```solidity
// User calls deposit function
AckiNackiBridge.deposit(commitment, 1 ether);

// Contract emits event
event Deposit(
    bytes32 indexed depositHash,    // = commitment
    address indexed sender,          // = msg.sender
    uint256 amount,                  // = 1 ether
    uint256 timestamp                // = block.timestamp
);
```

**Contract Changes:**
- ✅ Removed Merkle tree storage
- ✅ Removed `nextIndex`, `leaves`, `filledSubtrees`
- ✅ Simplified deposit function (10 lines vs 39 lines)
- ✅ Only tracks `treasuryBalance` and `usedNullifiers`

### 3. User Generates Deposit Proof

**Location:** User's machine (off-chain)

```bash
# Run deposit-prover
cd deposit-prover
./target/release/deposit-prover prove \
  --withdrawal-hash 0x1234567890abcdef... \
  --nullifier-preimage 0xfedcba0987654321... \
  --tx-hash 0xabcdef1234567890... \
  --rpc-url https://sepolia.infura.io/v3/YOUR_KEY \
  --contract-address 0x... \
  --output deposit_proof.json
```

**What happens:**
1. Fetch transaction receipt from Ethereum RPC
2. Find Deposit event in receipt logs
3. Generate MPT proof for receipt inclusion
4. Generate ZK proof using axiom-eth circuit:
   - Verify receipt in receipt trie
   - Verify event signature
   - Verify contract address
   - Prove knowledge of secrets
   - Compute nullifier

**Output:** `deposit_proof.json`
```json
{
  "proof": "0x...",
  "nullifier": "0x...",
  "recipient": "0x...",
  "amount": 1000000000000000000,
  "contract_address": "0x..."
}
```

### 4. User Submits to Acki Nacki

**Location:** Acki Nacki blockchain

```rust
// User submits withdrawal transaction
withdrawal_contract.withdraw(
    recipient_address,
    amount,
    nullifier,
    deposit_proof
);
```

**Contract verifies:**
1. Deposit proof is valid (ZK verification)
2. Nullifier hasn't been used
3. Amount matches proof
4. Contract address matches

**If valid:**
- Mark nullifier as used
- Transfer funds to recipient

## Security Properties

### Privacy ✅

- Deposits and withdrawals are unlinkable
- No one can tell which deposit corresponds to which withdrawal
- Only the user knows the secrets (withdrawal_hash, nullifier_preimage)

### Double-Spend Prevention ✅

- Each deposit can only be withdrawn once
- Nullifier is derived from secrets
- Contract tracks used nullifiers

### Proof Soundness ✅

- ZK proof ensures:
  - Event was actually emitted on Ethereum
  - User knows the secrets
  - Nullifier is correctly computed
- Cannot forge proofs without knowing secrets

### Event Authenticity ✅

- MPT proof ensures receipt is in Ethereum's receipt trie
- Receipt root is in block header
- Block header is part of Ethereum's canonical chain

## Gas Costs

### Old System (Merkle Tree)

- Deposit: ~100-150k gas (Merkle tree updates)
- Withdrawal: ~200k gas (Merkle proof verification)

### New System (Event-Based)

- Deposit: ~50k gas (just emit event) - **50-67% savings**
- Withdrawal: ~250k gas (ZK proof verification)

**Net savings:** ~50-100k gas per deposit

## Implementation Status

### Ethereum Contract ✅

- [x] Removed Merkle tree logic
- [x] Simplified deposit function
- [x] Updated Deposit event
- [x] Updated withdraw function to use contract address

### Deposit Prover 🚧

- [x] Project structure
- [x] CLI interface
- [x] Ethereum client (fetch events)
- [ ] MPT proof generation
- [ ] Circuit implementation
- [ ] Proof generation

### Withdrawal Contract ⏸️

- [ ] Update to accept deposit proofs
- [ ] Deploy deposit proof verifier
- [ ] Test on Sepolia

## Testing Plan

### Unit Tests

1. Test Deposit event emission
2. Test event data extraction
3. Test MPT proof generation
4. Test circuit constraints
5. Test proof generation/verification

### Integration Tests

1. Deploy contracts on Sepolia
2. Make test deposit
3. Generate deposit proof
4. Submit to Acki Nacki testnet
5. Verify withdrawal succeeds

### End-to-End Test

```bash
# 1. Deploy contracts
forge script script/Deploy.s.sol --rpc-url sepolia --broadcast

# 2. Make deposit
cast send $BRIDGE_ADDRESS "deposit(bytes32,uint256)" \
  $COMMITMENT 1000000000000000000 --value 1ether

# 3. Generate proof
cd deposit-prover
./target/release/deposit-prover prove \
  --withdrawal-hash $WH \
  --nullifier-preimage $NP \
  --tx-hash $TX_HASH \
  --rpc-url $RPC_URL \
  --contract-address $BRIDGE_ADDRESS \
  --output proof.json

# 4. Submit to Acki Nacki
# (TBD - depends on Acki Nacki interface)
```

## Next Steps

1. **Implement MPT proof generation** (Priority 1)
   - Add RLP encoding
   - Build receipt trie
   - Extract proof path

2. **Implement circuit** (Priority 2)
   - Integrate axiom-eth components
   - Add Poseidon hash
   - Wire up all constraints

3. **Generate Solidity verifier** (Priority 3)
   - Use snark-verifier-sdk
   - Deploy to Sepolia
   - Test on-chain verification

4. **Update withdrawal contract** (Priority 4)
   - Accept deposit proofs
   - Verify proofs on-chain
   - Test full flow

## Timeline

- **Week 1-2:** MPT proof generation + RLP encoding
- **Week 3-4:** Circuit implementation
- **Week 5:** Proof generation + testing
- **Week 6:** Integration + deployment

**Total:** ~6 weeks to production-ready implementation

