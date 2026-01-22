# ZK Proof Verifier Architecture

This document describes the verifier architecture for the Acki Nacki Bridge.

## Overview

The bridge uses a modular verifier architecture where the ZK proof verification is separated into its own contract. This allows for:

1. **Flexibility**: Easy to swap verifier implementations (e.g., from dummy to real Halo2 verifier)
2. **Testability**: Can test bridge logic independently with a dummy verifier
3. **Upgradeability**: Can upgrade the verifier without redeploying the entire bridge
4. **Separation of Concerns**: Bridge logic is separate from cryptographic verification

## Architecture

```
┌─────────────────────┐
│  AckiNackiBridge    │
│                     │
│  - deposit()        │
│  - withdraw()       │
│  - Merkle tree      │
│  - Treasury         │
└──────────┬──────────┘
           │
           │ uses
           ▼
┌─────────────────────┐
│ IAckiNackiVerifier  │  (Interface)
│                     │
│ - verifyWithdrawal  │
│   Proof()           │
└──────────┬──────────┘
           │
           │ implements
           ▼
┌─────────────────────┐       ┌─────────────────────┐
│  DummyVerifier      │       │  Halo2Verifier      │
│                     │       │                     │
│  (for testing)      │       │  (production)       │
│  - Basic validation │       │  - Real ZK proof    │
│  - Accepts all      │       │    verification     │
│    valid inputs     │       │  - Generated from   │
│                     │       │    Halo2 circuits   │
└─────────────────────┘       └─────────────────────┘
```

## Contracts

### IAckiNackiVerifier (Interface)

**Location**: `src/IAckiNackiVerifier.sol`

**Purpose**: Defines the interface that all verifier implementations must follow.

**Methods**:
- `verifyWithdrawalProof(bytes proof, uint256[] publicInputs) returns (bool)`: Verifies a withdrawal proof
- `getPublicInputsCount() returns (uint256)`: Returns the expected number of public inputs (4)

**Public Inputs** (in order):
1. `nullifier` (bytes32 as uint256): Unique identifier to prevent double-spending
2. `recipient` (address as uint256): Address to receive withdrawn funds
3. `amount` (uint256): Amount to withdraw
4. `root` (bytes32 as uint256): Merkle root at the time of withdrawal

### DummyVerifier (Testing Implementation)

**Location**: `src/DummyVerifier.sol`

**Purpose**: Dummy implementation for testing that performs basic validation but accepts all proofs.

**⚠️ WARNING**: DO NOT USE IN PRODUCTION! This verifier does not perform actual ZK proof verification.

**Validation Performed**:
- Proof must not be empty
- Must have exactly 4 public inputs
- Nullifier must not be zero
- Recipient must not be zero
- Amount must not be zero
- Root must not be zero

**Use Cases**:
- Unit testing the bridge contract
- Integration testing the deposit/withdrawal flow
- Development and debugging
- Testing error conditions

### Halo2Verifier (Production Implementation)

**Location**: To be generated from Halo2 circuits

**Purpose**: Real ZK proof verification using Halo2/PLONK.

**Status**: Not yet implemented. Will be generated from the Halo2 circuits in `crates/zk-proofs`.

**Implementation Steps**:
1. Generate Solidity verifier from Halo2 circuits using `halo2-solidity-verifier` or similar tool
2. Wrap the generated verifier to implement `IAckiNackiVerifier` interface
3. Ensure public inputs match the expected format
4. Deploy and test with real proofs

## AckiNackiBridge Integration

The bridge contract accepts the verifier address in its constructor:

```solidity
constructor(address _verifier) {
    if (_verifier == address(0)) revert InvalidVerifier();
    verifier = IAckiNackiVerifier(_verifier);
    // ... initialize Merkle tree
}
```

During withdrawal, the bridge calls the verifier:

```solidity
function withdraw(
    bytes32 nullifier,
    address payable recipient,
    uint256 amount,
    bytes32 root,
    bytes calldata proof
) external {
    // ... check nullifier and root
    
    // Prepare public inputs
    uint256[] memory publicInputs = new uint256[](4);
    publicInputs[0] = uint256(nullifier);
    publicInputs[1] = uint256(uint160(address(recipient)));
    publicInputs[2] = amount;
    publicInputs[3] = uint256(root);
    
    // Verify proof
    bool isValid = verifier.verifyWithdrawalProof(proof, publicInputs);
    if (!isValid) revert InvalidProof();
    
    // ... process withdrawal
}
```

## Deployment

### Testing Deployment

1. Deploy DummyVerifier:
   ```bash
   forge create --rpc-url <RPC_URL> \
     --private-key <PRIVATE_KEY> \
     src/DummyVerifier.sol:DummyVerifier
   ```

2. Deploy AckiNackiBridge with DummyVerifier address:
   ```bash
   forge create --rpc-url <RPC_URL> \
     --private-key <PRIVATE_KEY> \
     --constructor-args <VERIFIER_ADDRESS> \
     src/AckiNackiBridge.sol:AckiNackiBridge
   ```

### Production Deployment

1. Generate Halo2Verifier from circuits
2. Deploy Halo2Verifier
3. Deploy AckiNackiBridge with Halo2Verifier address

## Testing

The test suite includes comprehensive tests for the verifier:

- `testInvalidVerifierAddress()`: Ensures bridge rejects zero address
- `testVerifierIntegration()`: Verifies verifier is properly connected
- `testVerifierRejectsEmptyProof()`: Tests empty proof rejection
- `testVerifierRejectsInvalidPublicInputs()`: Tests wrong number of inputs
- `testVerifierRejectsZeroNullifier()`: Tests zero nullifier rejection
- `testVerifierAcceptsValidProof()`: Tests valid proof acceptance

Run tests with:
```bash
cd contracts/ethereum
forge test
```

## Future Improvements

1. **Verifier Upgradeability**: Add ability to upgrade verifier without redeploying bridge
2. **Multiple Verifiers**: Support different verifier versions for different proof types
3. **Gas Optimization**: Optimize public input encoding
4. **Batch Verification**: Support verifying multiple proofs in one transaction
5. **Proof Caching**: Cache verification results for identical proofs

## Security Considerations

1. **Verifier Validation**: Always validate verifier address in constructor
2. **Public Input Encoding**: Ensure consistent encoding between Rust and Solidity
3. **Proof Format**: Ensure proof format matches what verifier expects
4. **Nullifier Uniqueness**: Bridge checks nullifier before calling verifier
5. **Root Validation**: Bridge validates root before calling verifier

## References

- Halo2 Documentation: https://zcash.github.io/halo2/
- Tornado Cash: https://github.com/tornadocash/tornado-core
- PLONK Paper: https://eprint.iacr.org/2019/953

