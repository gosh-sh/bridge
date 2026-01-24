# Acki Nacki Bridge - Development Progress

## Current Status

### ✅ Completed Components

#### 1. Smart Contracts (Solidity)
- **AckiNackiBridge.sol**: Main bridge contract with deposit and withdrawal functionality
- **DummyVerifier.sol**: Verifier wrapper that delegates to Halo2Verifier
- **Halo2Verifier.sol**: Real ZK verifier with BN254 pairing checks (generated from Halo2 circuit)
- **PoseidonT3.sol**: Poseidon hash implementation for ZK-friendly hashing
- **IncrementalMerkleTree.sol**: Tornado Cash-style sparse Merkle tree (height 20)

#### 2. Rust Crates
- **zk-proofs**: Core ZK proof library with Poseidon hash and circuit definitions
- **verifier-generator**: Binary to generate Halo2Verifier.sol from circuit
- **proof-generator**: Binary to generate real Halo2 proofs for testing

#### 3. Testing Infrastructure
- **FFI Integration**: Solidity tests can call Rust proof generator via FFI
- **Proof Generation Scripts**: Shell scripts to generate proofs from test data
- **End-to-End Tests**: Tests that perform deposit→proof generation→withdrawal flow

### 📊 Test Results
- **16 out of 23 tests passing** (70% pass rate)
- All basic functionality tests pass:
  - ✅ Deposits
  - ✅ Merkle tree operations
  - ✅ Nullifier tracking
  - ✅ Access control
  - ✅ Edge cases (empty proofs, invalid inputs, etc.)

### ❌ Failing Tests (7 tests)
All failures are related to proof verification:
- `testEndToEndWithRealProof`: End-to-end deposit and withdrawal
- `testProofVerificationOnly`: Isolated proof verification test
- `testVerifierAcceptsValidProof`: Verifier should accept valid proofs
- `testVerifierComputesNullifierCorrectly`: Nullifier computation test
- `testWithdrawal`: Basic withdrawal test
- `testWithdrawalDoubleSpendPrevention`: Double-spend prevention test
- `testWithdrawalInsufficientTreasury`: Insufficient treasury test

**Root Cause**: The `WithdrawalCircuit` now properly computes the nullifier inside the circuit, but the test data needs to be updated to match the new circuit structure. The circuit now requires:
- Private inputs: `withdrawal_hash`, `nullifier_preimage` (instead of just `nullifier`)
- The circuit computes: `nullifier = Poseidon(withdrawal_hash, nullifier_preimage)`
- Tests need to be updated to provide the correct private inputs

## Architecture

### ZK Proof Flow
```
Private Inputs (Witnesses):
  - withdrawal_hash: Hash of withdrawal details
  - nullifier_preimage: Secret value known only to user
  - merkle_proof: Proof of inclusion in Merkle tree

Public Inputs:
  - recipient: Ethereum address
  - amount: Wei amount
  - root: Merkle tree root

Public Outputs:
  - nullifier: Poseidon(withdrawal_hash, nullifier_preimage)

Circuit Constraints:
  1. Compute nullifier = Poseidon(withdrawal_hash, nullifier_preimage)
  2. Verify Merkle proof of inclusion
  3. Constrain public inputs to instance column
```

### Deposit Flow
1. User calls `deposit(amount)` on Ethereum
2. Contract computes `commitment = Poseidon(withdrawal_hash, nullifier_preimage)`
3. Contract inserts commitment into Merkle tree
4. Event emitted with commitment and leaf index

### Withdrawal Flow
1. User generates ZK proof off-chain with private inputs
2. User calls `withdraw(proof, recipient, amount, root)` on Ethereum
3. Contract extracts nullifier from proof
4. Contract verifies:
   - Proof is valid (via Halo2Verifier)
   - Root exists in history
   - Nullifier hasn't been used
   - Treasury has sufficient balance
5. Contract marks nullifier as used
6. Contract transfers funds to recipient

## Technical Details

### Halo2 Circuit API (halo2-axiom)
```rust
// Correct API usage:
let cell = region.assign_advice(column, row, value);  // Returns AssignedCell directly
layouter.constrain_instance(cell.cell(), instance_column, row);  // No Result, no ?
```

### Poseidon Hash Parameters
- **Curve**: BN254 (alt_bn128)
- **T**: 3 (state size)
- **RATE**: 2 (inputs per permutation)
- **r_f**: 8 (full rounds)
- **r_p**: 57 (partial rounds)

### Proof Format
- **Size**: 800 bytes
- **Format**: Hex string with 0x prefix
- **Calldata**: `[nullifier (32) || recipient (32) || amount (32) || root (32) || proof (800)]`

### Solidity Version
- **Version**: 0.8.19 (exact, not ^0.8.19)
- **Optimizer**: Disabled globally
- **via_ir**: Disabled globally

## Next Steps

### 1. Implement Real Withdrawal Circuit
The current `SimpleCircuit` needs to be replaced with a real withdrawal circuit that:

**Private Inputs:**
- `withdrawal_hash: Fr` - Hash of (recipient, amount)
- `nullifier_preimage: Fr` - Secret value
- `merkle_proof: Vec<Fr>` - Merkle proof path (20 levels)
- `merkle_path_indices: Vec<bool>` - Left/right indicators

**Public Inputs:**
- `recipient: Fr` - Ethereum address as field element
- `amount: Fr` - Wei amount as field element
- `root: Fr` - Merkle tree root

**Public Outputs:**
- `nullifier: Fr` - Computed as Poseidon(withdrawal_hash, nullifier_preimage)

**Circuit Logic:**
```rust
// 1. Compute withdrawal_hash from public inputs
let computed_withdrawal_hash = poseidon_hash([recipient, amount]);

// 2. Constrain that computed_withdrawal_hash equals private withdrawal_hash
assert_equal(computed_withdrawal_hash, withdrawal_hash);

// 3. Compute nullifier from private inputs
let nullifier = poseidon_hash([withdrawal_hash, nullifier_preimage]);

// 4. Verify Merkle proof
let leaf = withdrawal_hash;
let mut current = leaf;
for (i, (sibling, is_right)) in merkle_proof.iter().zip(merkle_path_indices).enumerate() {
    if is_right {
        current = poseidon_hash([sibling, current]);
    } else {
        current = poseidon_hash([current, sibling]);
    }
}
assert_equal(current, root);

// 5. Constrain public inputs/outputs to instance column
constrain_instance(nullifier, 0);
constrain_instance(recipient, 1);
constrain_instance(amount, 2);
constrain_instance(root, 3);
```

### 2. Update Proof Generator
Once the circuit is implemented, update `crates/verifier-generator/src/proof_gen.rs` to:
- Accept Merkle proof as input
- Compute withdrawal_hash correctly
- Generate proofs with real private inputs

### 3. Regenerate Verifier
After updating the circuit:
```bash
# Temporarily replace solc with fake version
sudo cp scripts/fake_solc.sh /usr/bin/solc
sudo chmod +x /usr/bin/solc

# Regenerate verifier
rm contracts/ethereum/src/Halo2Verifier.sol
cargo run --bin generate-verifier

# Restore original solc
sudo mv /usr/bin/solc.bak /usr/bin/solc
```

### 4. Update Tests
Update test data to include:
- Real Merkle proofs from deposits
- Correct nullifier preimages
- Matching withdrawal hashes

### 5. Run Full Test Suite
```bash
cd contracts/ethereum
forge test
```

All 23 tests should pass once the circuit is implemented correctly.

## Files Modified

### Circuit Implementation
- `crates/verifier-generator/src/main.rs` - Verifier generator with SimpleCircuit
- `crates/verifier-generator/src/proof_gen.rs` - Proof generator with SimpleCircuit

### Generated Files
- `contracts/ethereum/src/Halo2Verifier.sol` - Generated ZK verifier

### Test Files
- `contracts/ethereum/test/AckiNackiBridge.t.sol` - Comprehensive test suite

### Scripts
- `scripts/generate_proof.sh` - Generate proof via FFI
- `scripts/generate_proof_to_file.sh` - Generate proof to file
- `scripts/fake_solc.sh` - Fake solc for verifier generation

## Known Issues

### 1. Solc Version Mismatch
The snark-verifier generates Solidity code for 0.8.19, but the system has 0.8.30. Workaround: use fake_solc.sh script.

### 2. Simplified Circuit
The current circuit is a placeholder. Real withdrawal logic needs to be implemented.

### 3. Test Proof Data
Test proofs are generated with dummy data. Need to generate proofs with real Merkle tree data.

## Resources

- **Halo2 Book**: https://zcash.github.io/halo2/
- **Axiom Halo2**: https://github.com/axiom-crypto/halo2-lib
- **Snark Verifier**: https://github.com/axiom-crypto/snark-verifier
- **Poseidon**: https://github.com/privacy-scaling-explorations/poseidon
- **Tornado Cash**: https://github.com/tornadocash/tornado-core

