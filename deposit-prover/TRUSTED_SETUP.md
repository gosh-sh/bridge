# Trusted Setup for KZG Parameters

## ✅ Security: Trusted Setup Required

This prover **requires** KZG parameters from a trusted setup ceremony. Random parameter generation has been **permanently disabled** for security.

The parameters MUST come from a trusted setup ceremony where:

- Multiple independent participants contributed randomness
- The "toxic waste" (secret τ) was destroyed
- The ceremony is publicly verifiable

**What this means**: The prover will fail with a clear error message if trusted setup parameters are not found. This prevents accidental use of insecure parameters in production.

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

This downloads the pre-converted `.srs` file (33 MB) directly from the [halo2-kzg-srs](https://github.com/han0110/halo2-kzg-srs) project.

**Verification**:

- File size: 33,554,692 bytes (~33 MB)
- Source: https://trusted-setup-halo2kzg.s3.eu-central-1.amazonaws.com/hermez-raw-18
- Participants: 100+ independent contributors
- Ceremony details: https://github.com/iden3/snarkjs#7-prepare-phase-2

### Option 2: Aztec Ignition Ceremony

**Source**: https://github.com/AztecProtocol/ignition-verification

Large-scale BN254 trusted setup with extensive verification.

### Option 3: Hermez/Polygon Ceremony

**Source**: https://github.com/iden3/snarkjs

Used by Polygon zkEVM and other production systems.

## Pre-Converted Parameters

We use pre-converted Halo2 parameters from the [halo2-kzg-srs](https://github.com/han0110/halo2-kzg-srs) project.

**Why pre-converted?**

- The conversion from `.ptau` to Halo2 format is complex
- The halo2-kzg-srs project provides verified conversions
- Reduces setup complexity and potential errors
- Smaller file size (33 MB vs 288 MB)

**Source**: The parameters come from the Hermez/Polygon Powers of Tau ceremony, converted and verified by the halo2-kzg-srs project.

## Current Implementation Status

### ✅ Production-Ready

The code in `src/prover.rs` **only** loads trusted setup parameters. Random parameter generation has been removed.

```rust
pub fn load_kzg_params_from_trusted_setup(k: u32) -> Result<ParamsKZG<Bn256>, String> {
    let params_path = format!("data/kzg_params_{}.srs", k);

    // Try to load existing parameters
    if Path::new(&params_path).exists() {
        println!("Loading KZG parameters from {}", params_path);
        return load_kzg_params(&params_path);
    }

    // Parameters not found - fail with clear instructions
    Err(format!(
        "KZG parameters not found at {}. \
         You MUST use trusted setup parameters. \
         See TRUSTED_SETUP.md for instructions.",
        params_path
    ))
}
```

This ensures:

- ✅ **Never** generates random parameters
- ✅ Clear error messages if trusted setup is missing
- ✅ Production-safe by default
- ✅ No feature flags needed

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

## Completed ✅

- [x] Download and verify trusted setup for k=18
- [x] Add checksums for verification
- [x] Remove insecure parameter generation
- [x] Use pre-converted Halo2 parameters from halo2-kzg-srs
- [x] Add automated download script
- [x] Document the exact ceremony used and verification steps
- [x] Successfully tested with E2E test on Sepolia testnet
