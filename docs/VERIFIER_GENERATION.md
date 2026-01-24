# Halo2 Verifier Generation

This document explains how to generate and deploy the Halo2 Yul verifier for the Acki Nacki Bridge.

## Overview

The verifier generation process creates a Yul-based verifier contract that can verify zero-knowledge proofs on Ethereum. The process involves:

1. **Generating the Yul verifier** - Using the `generate-verifier` binary
2. **Compiling to bytecode** - Using the Solidity compiler (solc)
3. **Testing the verifier** - Using Foundry tests

## Quick Start

### Generate Verifier

```bash
make generate-verifier
```

This command will:
- Remove old verifier files
- Generate a new Yul verifier at `contracts/ethereum/Halo2Verifier.yul`
- Compile it to bytecode at `contracts/ethereum/verifier_bytecode.bin`
- Create a hex representation at `contracts/ethereum/verifier_bytecode.hex`
- Generate a test proof at `contracts/ethereum/test/test_proof.json`

### Generate Test Proof

```bash
make generate-proof
```

This generates a test proof with sample inputs:
- Secret: 12345
- Nullifier: 67890
- Recipient: 43981
- Amount: 1000
- Root: 4660

### Test the Verifier

```bash
cd contracts/ethereum
forge test --match-contract Halo2VerifierDirectTest -vv
```

## Manual Process

If you need to run the steps manually:

### 1. Generate Yul Verifier

```bash
cargo run --bin generate-verifier
```

This creates:
- `contracts/ethereum/Halo2Verifier.yul` - The Yul verifier contract
- `contracts/ethereum/test/test_proof.json` - Test proof data

### 2. Compile to Bytecode

```bash
cd contracts/ethereum

# Generate hex bytecode
solc --yul --bin Halo2Verifier.yul 2>&1 | grep "Binary representation" -A 1 | tail -1 > verifier_bytecode.hex

# Convert to binary
cat verifier_bytecode.hex | xxd -r -p > verifier_bytecode.bin
```

### 3. Check Bytecode Size

```bash
wc -c verifier_bytecode.bin
```

The bytecode must be under 24,576 bytes (24KB) to deploy on Ethereum (EIP-170 limit).

Current size: **18,716 bytes** (5,860 bytes under limit)

## Circuit Configuration

The verifier is generated for the withdrawal circuit with the following configuration:

- **Circuit type**: Withdrawal circuit with Poseidon hash
- **KZG parameter k**: 10 (2^10 = 1024 rows)
- **Advice columns**: 4
- **Instance column**: 1 (for public inputs)
- **Poseidon configuration**: 
  - State columns: `advice[1..4]`
  - Partial S-box column: `advice[0]`
  - Spec: P128Pow5T3 (128-bit security, power-of-5 S-box, T=3)

## Public Inputs

The verifier expects 4 public inputs in this order:

1. **nullifier** - Hash of (secret, nullifier_preimage)
2. **recipient** - Ethereum address of the recipient
3. **amount** - Amount to withdraw
4. **root** - Merkle tree root

## Verifier Interface

The Yul verifier is called with:

```solidity
bytes memory calldata_ = abi.encodePacked(
    bytes32(nullifier),
    bytes32(recipient),
    bytes32(amount),
    bytes32(root),
    proof  // 2272 bytes
);

(bool success, ) = verifier.call{gas: 30000000}(calldata_);
// success == true means proof is valid
// success == false (revert) means proof is invalid
```

## Files Generated

| File | Description | Size |
|------|-------------|------|
| `Halo2Verifier.yul` | Yul verifier source code | ~200KB |
| `verifier_bytecode.hex` | Hex-encoded bytecode | ~37KB |
| `verifier_bytecode.bin` | Binary bytecode (deployable) | ~18.7KB |
| `test/test_proof.json` | Test proof with public inputs | ~5KB |

**Note**: These files are build artifacts and are excluded from git (see `.gitignore`). They can be regenerated at any time using `make generate-verifier`.

## Testing

### Unit Tests

The verifier has comprehensive tests in `test/Halo2VerifierDirect.t.sol`:

1. **testVerifierWithGeneratedProof** - Verifies a valid proof succeeds
2. **testVerifierRejectsInvalidProof** - Verifies an invalid proof is rejected

Run tests:

```bash
cd contracts/ethereum
forge test --match-contract Halo2VerifierDirectTest -vv
```

### Integration Tests

To test the full flow:

1. Generate a new verifier: `make generate-verifier`
2. Generate a test proof: `make generate-proof`
3. Run Solidity tests: `cd contracts/ethereum && forge test`

## Troubleshooting

### Verifier Generation Fails

**Problem**: `generate-verifier` binary fails to compile or run

**Solution**: 
- Ensure all Rust dependencies are up to date: `cargo update`
- Check that KZG parameters exist: `ls params/kzg_bn254_10.srs`
- Rebuild the workspace: `cargo build --workspace`

### Bytecode Compilation Fails

**Problem**: `solc` fails to compile the Yul file

**Solution**:
- Check solc version: `solc --version` (should be 0.8.19+)
- Install/update solc: `sudo apt-get install solc`
- Verify the Yul file exists: `ls contracts/ethereum/Halo2Verifier.yul`

### Bytecode Too Large

**Problem**: Bytecode exceeds 24KB Ethereum limit

**Solution**:
- Reduce circuit complexity (fewer constraints)
- Use a smaller k parameter (fewer rows)
- Optimize the circuit configuration

### Test Proof Verification Fails

**Problem**: Valid proof is rejected by the verifier

**Solution**:
- Ensure verifier and proof are generated with the same circuit configuration
- Regenerate both: `make generate-verifier && make generate-proof`
- Check public inputs match the circuit expectations
- Verify the proof format is correct (2272 bytes)

## Production Deployment

### Prerequisites

1. Verifier bytecode is under 24KB
2. All tests pass
3. Circuit configuration is finalized

### Deployment Steps

1. Generate production verifier:
   ```bash
   make generate-verifier
   ```

2. Verify bytecode size:
   ```bash
   wc -c contracts/ethereum/verifier_bytecode.bin
   ```

3. Test thoroughly:
   ```bash
   cd contracts/ethereum
   forge test
   ```

4. Deploy using Foundry:
   ```solidity
   // In your deployment script
   bytes memory bytecode = vm.readFileBinary("verifier_bytecode.bin");
   address verifier;
   assembly {
       verifier := create(0, add(bytecode, 0x20), mload(bytecode))
   }
   require(verifier != address(0), "Deployment failed");
   ```

## Technical Details

### Poseidon Hash Implementation

The verifier uses the production scroll-tech/poseidon-circuit implementation:

- **Repository**: https://github.com/scroll-tech/poseidon
- **Branch**: main
- **Crates**: 
  - `poseidon-circuit` - In-circuit Poseidon hash
  - `poseidon-base` - Native Poseidon hash

### Proof Format

The proof is 2272 bytes and contains:

- Commitment points (G1 elements)
- Evaluation proofs
- KZG opening proofs
- Polynomial evaluations

### Gas Costs

Approximate gas costs for verification:

- **Valid proof**: ~500,000 gas
- **Invalid proof**: ~500,000 gas (reverts)

Note: Gas costs may vary depending on the proof and public inputs.

## References

- [Halo2 Documentation](https://zcash.github.io/halo2/)
- [Scroll Tech Halo2](https://github.com/scroll-tech/halo2)
- [Poseidon Hash](https://github.com/scroll-tech/poseidon)
- [EIP-170: Contract Size Limit](https://eips.ethereum.org/EIPS/eip-170)

