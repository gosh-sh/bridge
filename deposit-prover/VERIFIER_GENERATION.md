# Solidity Verifier Generation Guide

This guide explains how to generate and deploy the Solidity verifier contract for the deposit prover.

## Overview

The Solidity verifier is a smart contract that can verify ZK proofs on-chain. It's generated from the circuit's verifying key and uses the SHPLONK multi-open scheme for efficient verification.

## Prerequisites

1. **Rust toolchain** - Install from https://rustup.rs/
2. **Circuit parameters** - The `configs/circuit_params.json` file must exist
3. **Disk space** - At least 2GB free for KZG parameters and proving keys
4. **Time** - First run takes 5-15 minutes to generate keys

## Quick Start

### Step 1: Generate the Verifier

Run the verifier generator:

```bash
cd deposit-prover
cargo run --example generate_verifier --release
```

**What happens:**
1. Loads circuit configuration from `configs/circuit_params.json`
2. Generates KZG parameters (if not cached in `data/kzg_params_18.srs`)
3. Generates proving key (if not cached in `data/deposit_prover_k18.pk`)
4. Extracts verifying key from proving key
5. Generates Solidity verifier contract
6. Saves to `../contracts/DepositVerifier.sol`

**Expected output:**
```
=== Deposit Prover: Solidity Verifier Generator ===

Circuit Configuration:
  - Degree (k): 18
  - Max data byte length: 256
  - Max log number: 20
  - Topic number bounds: (0, 4)

Generating Solidity verifier...
This may take several minutes on first run (generating proving key)...

Loading KZG parameters...
Generating KZG parameters for k=18 (this may take a few minutes)...
Loading proving key...
Generating proving key (this may take a few minutes)...
Proving key generated and saved to "data/deposit_prover_k18.pk"
Generating Solidity code...
✅ Solidity verifier generated at: "../contracts/DepositVerifier.sol"
   Contract size: 12345 bytes

✅ SUCCESS!
Solidity verifier contract generated at: "../contracts/DepositVerifier.sol"

Next steps:
1. Review the generated contract
2. Deploy it to Ethereum using Hardhat/Foundry
3. Update AckiNackiBridge.sol to use the verifier address
```

### Step 2: Review the Generated Contract

The generated contract will look like this:

```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

contract Halo2Verifier {
    // Verifier implementation
    function verifyProof(
        bytes calldata proof,
        uint256[] calldata instances
    ) public view returns (bool) {
        // ... verification logic ...
    }
}
```

**Key points:**
- The contract is self-contained (no external dependencies)
- It uses assembly for gas optimization
- The `instances` parameter contains the public inputs: `[depositId, sender, amount, contract_address]`

### Step 3: Deploy the Verifier

#### Option A: Using Hardhat

```bash
cd contracts
npx hardhat run scripts/deploy-verifier.js --network sepolia
```

#### Option B: Using Foundry

```bash
cd contracts
forge create --rpc-url $RPC_URL --private-key $PRIVATE_KEY DepositVerifier
```

### Step 4: Update the Bridge Contract

Update `AckiNackiBridge.sol` to use the verifier:

```solidity
contract AckiNackiBridge {
    IDepositVerifier public depositVerifier;
    
    constructor(address _verifier) {
        depositVerifier = IDepositVerifier(_verifier);
    }
    
    function processWithdrawal(
        bytes calldata proof,
        uint256 depositId,
        address sender,
        uint256 amount,
        address contractAddress
    ) external {
        // Prepare public inputs
        uint256[] memory instances = new uint256[](4);
        instances[0] = depositId;
        instances[1] = uint256(uint160(sender));
        instances[2] = amount;
        instances[3] = uint256(uint160(contractAddress));
        
        // Verify the proof
        require(
            depositVerifier.verifyProof(proof, instances),
            "Invalid proof"
        );
        
        // Process withdrawal...
    }
}
```

## Configuration

### Circuit Parameters

Edit `configs/circuit_params.json` to customize the circuit:

```json
{
  "base": {
    "k": 18,                              // Circuit degree (2^18 = 256K rows)
    "num_advice_per_phase": [50, 25],    // Advice columns per phase
    "num_fixed": 1,                       // Fixed columns
    "num_lookup_advice_per_phase": [1, 1, 0],
    "lookup_bits": 8,
    "num_instance_columns": 1
  },
  "num_rlc_columns": 3
}
```

**Important:** If you change `k`, you must regenerate the verifier!

### Custom Configuration

You can also generate a verifier with custom configuration:

```rust
use deposit_prover::prover::{generate_solidity_verifier, CircuitConfig};
use std::path::Path;

let config = CircuitConfig {
    degree: 20,                    // Larger circuit
    max_data_byte_len: 512,        // More data
    max_log_num: 50,               // More logs
    topic_num_bounds: (0, 4),
};

generate_solidity_verifier(&config, Path::new("MyVerifier.sol"))?;
```

## Troubleshooting

### Error: "Failed to open config file"

**Solution:** Make sure `configs/circuit_params.json` exists:
```bash
ls -la configs/circuit_params.json
```

### Error: "Failed to create data directory"

**Solution:** Create the directory manually:
```bash
mkdir -p data
```

### Error: Out of memory

**Solution:** The circuit is too large. Try reducing `k` in the config:
- k=18 requires ~4GB RAM
- k=19 requires ~8GB RAM
- k=20 requires ~16GB RAM

### Verifier contract too large (>24KB)

**Solution:** The circuit is too complex. Options:
1. Reduce `k` in the config
2. Reduce `max_data_byte_len` or `max_log_num`
3. Use a proxy contract pattern
4. Deploy on a chain with larger contract size limits

## Performance

### Generation Time

| Step | First Run | Cached |
|------|-----------|--------|
| KZG Parameters (k=18) | 2-5 min | instant |
| Proving Key | 5-10 min | instant |
| Solidity Code | 10-30 sec | 10-30 sec |
| **Total** | **7-15 min** | **10-30 sec** |

### File Sizes

| File | Size (k=18) |
|------|-------------|
| KZG Parameters | ~500 MB |
| Proving Key | ~200 MB |
| Solidity Verifier | ~10-20 KB |

### Gas Costs

Estimated gas costs for on-chain verification:
- Deployment: ~3-5M gas
- Verification: ~300-500K gas per proof

## Advanced Usage

### Programmatic Generation

```rust
use deposit_prover::prover::{generate_solidity_verifier, CircuitConfig};
use std::path::Path;

fn main() -> Result<(), String> {
    let config = CircuitConfig::default();
    let output = Path::new("MyVerifier.sol");
    
    generate_solidity_verifier(&config, output)?;
    
    println!("Verifier generated!");
    Ok(())
}
```

### Batch Generation

Generate verifiers for multiple configurations:

```rust
for k in [16, 17, 18, 19] {
    let config = CircuitConfig {
        degree: k,
        ..Default::default()
    };
    
    let path = format!("verifier_k{}.sol", k);
    generate_solidity_verifier(&config, Path::new(&path))?;
}
```

## Next Steps

After generating and deploying the verifier:

1. **Test the verifier** - Generate a proof and verify it on-chain
2. **Integrate with bridge** - Update the bridge contract to use the verifier
3. **End-to-end testing** - Test the full deposit → proof → withdrawal flow
4. **Gas optimization** - Profile and optimize the verification gas costs
5. **Security audit** - Have the verifier contract audited

## References

- [SHPLONK Paper](https://eprint.iacr.org/2020/081)
- [Halo2 Documentation](https://zcash.github.io/halo2/)
- [snark-verifier](https://github.com/axiom-crypto/snark-verifier)
- [axiom-eth](https://github.com/axiom-crypto/axiom-eth)

