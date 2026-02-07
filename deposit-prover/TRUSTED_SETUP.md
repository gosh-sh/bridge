# Trusted Setup for KZG Parameters

## ⚠️ CRITICAL SECURITY ISSUE

**DO NOT USE `gen_srs()` IN PRODUCTION!**

The current code uses `gen_srs(k)` from `halo2_base::utils::fs`, which generates **random, untrusted** KZG parameters locally. This is a **critical security vulnerability**:

1. **Anyone can forge proofs** if they know the "toxic waste" (secret randomness τ used during generation)
2. **No security guarantees** - the setup is not trustworthy
3. **Bridge funds are at risk** - attackers could create fake deposit proofs and steal all funds

## ✅ Solution: Use Trusted Setup

For production, you MUST use KZG parameters from a trusted setup ceremony where:

- Multiple independent participants contributed randomness
- The "toxic waste" (secret τ) was destroyed
- The ceremony is publicly verifiable

## Recommended Trusted Setups for BN254

### Option 1: Perpetual Powers of Tau (Recommended)

**Source**: https://github.com/privacy-scaling-explorations/perpetualpowersoftau

This is a community-run ceremony specifically for the BN254 curve (used by Halo2).

**Download**:

Use the provided download script (recommended):

```bash
cd deposit-prover
./download_trusted_setup.sh
```

Or manually download:

```bash
# For k=18 (2^18 = 262,144 rows)
cd deposit-prover/trusted_setup
wget https://storage.googleapis.com/zkevm/ptau/powersOfTau28_hez_final_18.ptau
```

**Verification**:

- File size: 302,072,984 bytes (~288 MB)
- SHA256: `e970efa7774da80101e0ac336d083ef3339855c98112539338d706b2b89ac694`
- Participants: 100+ independent contributors
- Ceremony details: https://github.com/iden3/snarkjs#7-prepare-phase-2

### Option 2: Aztec Ignition Ceremony

**Source**: https://github.com/AztecProtocol/ignition-verification

Large-scale BN254 trusted setup with extensive verification.

### Option 3: Hermez/Polygon Ceremony

**Source**: https://github.com/iden3/snarkjs

Used by Polygon zkEVM and other production systems.

## Converting .ptau to Halo2 Format

The `.ptau` format (used by snarkjs/circom) needs to be converted to Halo2's native format.

### Method 1: Use Pre-Converted Parameters (Easiest)

Download pre-converted Halo2 parameters from a trusted source:

```bash
# TODO: Add link to pre-converted Halo2 params
# wget https://trusted-source.com/halo2_bn254_k18.srs \
#   -O deposit-prover/data/kzg_params_18.srs
```

### Method 2: Convert Yourself (Advanced)

If you want to convert the .ptau file yourself:

1. **Use ppot-rs crate** (Rust library for reading .ptau files):

   ```toml
   [dependencies]
   ppot-rs = "0.1.1"
   ```

2. **Implement conversion** (see `src/trusted_setup.rs` for example code)

3. **Verify the conversion** by comparing with known test vectors

## Current Implementation Status

### ⚠️ Development/Testing Only

The current code in `src/prover.rs` uses `gen_srs(k)` which is **ONLY SAFE FOR TESTING**.

```rust
// ⚠️ INSECURE - DO NOT USE IN PRODUCTION
let params = gen_srs(k);  // Generates random, untrusted parameters
```

### ✅ Production-Ready Implementation

To use trusted setup, replace the `get_or_create_kzg_params()` function:

```rust
pub fn get_or_create_kzg_params(k: u32) -> Result<ParamsKZG<Bn256>, String> {
    let params_path = format!("data/kzg_params_{}.srs", k);

    // Try to load existing parameters
    if Path::new(&params_path).exists() {
        println!("Loading KZG parameters from {}", params_path);
        return load_kzg_params(&params_path);
    }

    // ⚠️ CRITICAL: In production, NEVER generate params - always fail if not found
    #[cfg(not(feature = "insecure-testing"))]
    {
        return Err(format!(
            "KZG parameters not found at {}. \
             For production, you MUST download trusted setup parameters. \
             See TRUSTED_SETUP.md for instructions.",
            params_path
        ));
    }

    // Only allow generation in testing mode
    #[cfg(feature = "insecure-testing")]
    {
        println!("⚠️  WARNING: Generating INSECURE random KZG parameters for TESTING ONLY");
        println!("⚠️  DO NOT USE IN PRODUCTION!");
        let params = gen_srs(k);
        save_kzg_params(&params, &params_path)?;
        Ok(params)
    }
}
```

## Verification

After downloading or converting trusted setup parameters, verify them:

1. **Check file size**:

   ```bash
   ls -lh deposit-prover/data/kzg_params_18.srs
   # Should be ~2.6 GB for k=18
   ```

2. **Verify checksum**:

   ```bash
   sha256sum deposit-prover/data/kzg_params_18.srs
   # Compare with known good checksum
   ```

3. **Test with known proof**:
   ```bash
   cargo test --release test_with_trusted_setup
   ```

## Security Considerations

### Why Trusted Setup Matters

In KZG polynomial commitment schemes:

- The setup generates public parameters from a secret value τ (tau)
- If anyone knows τ, they can forge proofs
- A trusted setup ceremony ensures τ is destroyed after generating parameters

### Multi-Party Computation (MPC)

Trusted setup ceremonies use MPC where:

- Multiple participants each contribute randomness
- As long as ONE participant is honest and destroys their secret, the setup is secure
- The Perpetual Powers of Tau had 100+ participants

### Verification

You can verify the trusted setup:

1. Check the ceremony transcript
2. Verify the cryptographic proofs
3. Confirm participant contributions

## References

- [Perpetual Powers of Tau](https://github.com/privacy-scaling-explorations/perpetualpowersoftau)
- [snarkjs Trusted Setup Guide](https://github.com/iden3/snarkjs#7-prepare-phase-2)
- [Aztec Ignition Ceremony](https://github.com/AztecProtocol/ignition-verification)
- [KZG Polynomial Commitments](https://dankradfeist.de/ethereum/2020/06/16/kate-polynomial-commitments.html)
- [Trusted Setup Ceremonies](https://vitalik.ca/general/2022/03/14/trustedsetup.html)

## TODO

- [ ] Download and verify trusted setup for k=18
- [ ] Convert .ptau to Halo2 format (or find pre-converted)
- [ ] Add checksums for verification
- [ ] Implement `insecure-testing` feature flag
- [ ] Add automated tests with trusted setup
- [ ] Document the exact ceremony used and verification steps
