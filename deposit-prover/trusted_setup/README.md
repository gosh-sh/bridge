# Trusted Setup Files

This directory contains trusted setup files for KZG polynomial commitments.

⚠️ **Note**: The trusted setup files are **NOT** stored in git due to their large size (~288 MB).

## Download Instructions

To download the trusted setup, run from the `deposit-prover` directory:

```bash
./download_trusted_setup.sh
```

Or manually download:

```bash
cd trusted_setup
wget https://storage.googleapis.com/zkevm/ptau/powersOfTau28_hez_final_18.ptau
```

## Current Files

### `powersOfTau28_hez_final_18.ptau`

- **Source**: Perpetual Powers of Tau Ceremony (Hermez)
- **URL**: https://storage.googleapis.com/zkevm/ptau/powersOfTau28_hez_final_18.ptau
- **Size**: 288 MB (302,072,984 bytes)
- **Curve**: BN254 (bn128)
- **Max Degree**: 2^18 = 262,144 constraints
- **Format**: .ptau (snarkjs/circom format)
- **Participants**: 100+ independent contributors
- **Date**: November 22, 2023

## Verification

To verify the integrity of the downloaded file:

```bash
# Check file size
ls -lh powersOfTau28_hez_final_18.ptau
# Should be: 288M (302,072,984 bytes)

# Compute SHA256 checksum
sha256sum powersOfTau28_hez_final_18.ptau
# Expected: e970efa7774da80101e0ac336d083ef3339855c98112539338d706b2b89ac694
```

## Usage

This file is in `.ptau` format (used by snarkjs and circom). To use it with Halo2:

1. **Option A**: Convert to Halo2's native `.srs` format
   - See `../TRUSTED_SETUP.md` for conversion instructions
   - Use `ppot-rs` crate or custom conversion tool

2. **Option B**: Use pre-converted Halo2 parameters
   - Download from trusted source (if available)
   - Place in `../data/kzg_params_18.srs`

## Security

### Why This Matters

The trusted setup ceremony ensures that:

- The "toxic waste" (secret τ) was destroyed
- No single party can forge proofs
- The parameters are cryptographically secure

### Ceremony Details

The Perpetual Powers of Tau ceremony:

- Used multi-party computation (MPC)
- Had 100+ independent participants
- Each participant contributed randomness
- As long as ONE participant was honest, the setup is secure
- All contributions are publicly verifiable

### Verification Steps

1. **Download from official source**:

   ```bash
   wget https://storage.googleapis.com/zkevm/ptau/powersOfTau28_hez_final_18.ptau
   ```

2. **Verify checksum** (prevents tampering):

   ```bash
   sha256sum powersOfTau28_hez_final_18.ptau
   ```

3. **Verify ceremony transcript** (optional, advanced):
   - Check the ceremony logs
   - Verify participant contributions
   - Confirm cryptographic proofs

## References

- [Perpetual Powers of Tau Repository](https://github.com/privacy-scaling-explorations/perpetualpowersoftau)
- [snarkjs Documentation](https://github.com/iden3/snarkjs)
- [Hermez Ceremony Details](https://blog.hermez.io/hermez-cryptographic-setup/)
- [Understanding Trusted Setups](https://vitalik.ca/general/2022/03/14/trustedsetup.html)

## DO NOT

- ❌ Use `gen_srs()` in production (generates random, insecure parameters)
- ❌ Generate your own parameters without a proper ceremony
- ❌ Trust parameters from unknown sources
- ❌ Skip verification steps

## DO

- ✅ Download from official sources
- ✅ Verify checksums
- ✅ Use parameters from well-known ceremonies
- ✅ Document the provenance of your parameters
- ✅ Keep this file for audit purposes
