# Security Audit: Trusted Setup Implementation

**Date**: 2026-02-07  
**Issue**: Critical security vulnerability in KZG parameter generation  
**Severity**: CRITICAL  
**Status**: DOCUMENTED (Mitigation implemented for testing, production solution documented)

## Executive Summary

The deposit prover currently uses `gen_srs()` to generate random KZG parameters locally. This is a **critical security vulnerability** that makes the entire bridge insecure for production use. Anyone who runs the code knows the "toxic waste" (secret τ) and can forge proofs to steal all bridge funds.

## The Problem

### Current Implementation

```rust
// ⚠️ INSECURE - DO NOT USE IN PRODUCTION
let params = gen_srs(k);  // Generates random, untrusted parameters
```

### Why This Is Critical

1. **Toxic Waste Exposure**: The secret value τ (tau) used to generate the parameters is known to anyone who runs the code
2. **Proof Forgery**: Anyone with τ can create fake deposit proofs without actual deposits
3. **Fund Theft**: Attackers can drain all funds from the bridge
4. **No Security**: The entire ZK proof system provides zero security

### Attack Scenario

1. Attacker runs the deposit prover code locally
2. Attacker knows the secret τ from `gen_srs()`
3. Attacker creates fake deposit proofs claiming arbitrary amounts
4. Attacker withdraws funds from the bridge without ever depositing
5. Bridge is drained

## The Solution

### Trusted Setup Ceremony

For production, KZG parameters MUST come from a trusted setup ceremony where:

1. **Multi-Party Computation (MPC)**: Multiple independent participants contribute randomness
2. **Toxic Waste Destruction**: Each participant destroys their secret after contributing
3. **Security Guarantee**: As long as ONE participant is honest, the setup is secure
4. **Public Verifiability**: All contributions can be cryptographically verified

### Recommended Setup: Perpetual Powers of Tau

- **Source**: https://github.com/privacy-scaling-explorations/perpetualpowersoftau
- **Curve**: BN254 (compatible with Halo2)
- **Participants**: 100+ independent contributors
- **Verification**: Publicly auditable ceremony transcript
- **Usage**: Production systems (Polygon zkEVM, Hermez, etc.)

## Implementation Status

### ✅ Completed

1. **Downloaded Trusted Setup**:
   - File: `deposit-prover/trusted_setup/powersOfTau28_hez_final_18.ptau`
   - Size: 288 MB
   - Max degree: 2^18 = 262,144 constraints
   - Source: Hermez/Polygon ceremony

2. **Added Security Warnings**:
   - Updated `get_or_create_kzg_params()` with prominent warnings
   - Added documentation in `deposit-prover/TRUSTED_SETUP.md`
   - Created `deposit-prover/trusted_setup/README.md`

3. **Documented Solution**:
   - Detailed instructions for production deployment
   - Verification steps for trusted setup
   - Security considerations and best practices

### ⚠️ Remaining Work for Production

1. **Convert .ptau to Halo2 Format**:
   - The downloaded file is in `.ptau` format (snarkjs/circom)
   - Needs conversion to Halo2's native `.srs` format
   - Options:
     - Use `ppot-rs` crate to read .ptau and convert
     - Download pre-converted Halo2 parameters (if available)
     - Implement custom conversion tool

2. **Implement Production Mode**:
   ```rust
   #[cfg(not(feature = "insecure-testing"))]
   {
       // In production, NEVER generate params - always fail if not found
       return Err("KZG parameters not found. See TRUSTED_SETUP.md");
   }
   ```

3. **Add Verification**:
   - Checksum verification for downloaded files
   - Automated tests with trusted setup
   - Documentation of exact ceremony used

## Current Behavior

### Development/Testing Mode (Current)

When KZG parameters are not found:
```
╔══════════════════════════════════════════════════════════════╗
║  ⚠️  CRITICAL SECURITY WARNING  ⚠️                           ║
╠══════════════════════════════════════════════════════════════╣
║  Generating RANDOM, UNTRUSTED KZG parameters!                ║
║                                                              ║
║  This is ONLY SAFE for TESTING and DEVELOPMENT.             ║
║  DO NOT USE IN PRODUCTION!                                  ║
║                                                              ║
║  Anyone who runs this code knows the 'toxic waste' and      ║
║  can FORGE PROOFS to STEAL ALL BRIDGE FUNDS!                ║
║                                                              ║
║  For production, download trusted setup parameters from:    ║
║  https://github.com/privacy-scaling-explorations/...        ║
║                                                              ║
║  See TRUSTED_SETUP.md for detailed instructions.            ║
╚══════════════════════════════════════════════════════════════╝
```

### Production Mode (Recommended)

When KZG parameters are not found:
```
ERROR: KZG parameters not found at data/kzg_params_18.srs

For production, you MUST download trusted setup parameters.
See TRUSTED_SETUP.md for instructions.

DO NOT use gen_srs() in production!
```

## Files Modified

1. **`deposit-prover/src/prover.rs`**:
   - Added security warnings to `get_or_create_kzg_params()`
   - Updated documentation with security considerations
   - Added prominent warning box when generating random params

2. **`deposit-prover/TRUSTED_SETUP.md`** (NEW):
   - Comprehensive guide to trusted setup
   - Security explanation
   - Download and verification instructions
   - Production implementation guide

3. **`deposit-prover/trusted_setup/README.md`** (NEW):
   - Documentation of downloaded trusted setup file
   - Verification steps
   - Security considerations

4. **`SECURITY_AUDIT_TRUSTED_SETUP.md`** (THIS FILE):
   - Security audit documentation
   - Problem description and solution
   - Implementation status

## Testing

All existing tests pass with the updated code:

```bash
cd deposit-prover && cargo test --lib
```

Result: ✅ 11 tests passed

The tests currently use the insecure `gen_srs()` for convenience, which is acceptable for testing.

## Recommendations

### Immediate (Before Production)

1. ✅ **DONE**: Download trusted setup from Perpetual Powers of Tau
2. ✅ **DONE**: Add security warnings to code
3. ✅ **DONE**: Document the issue and solution
4. ⚠️ **TODO**: Convert .ptau to Halo2 format
5. ⚠️ **TODO**: Implement production mode that fails without trusted setup
6. ⚠️ **TODO**: Add checksum verification

### Long-term

1. Consider running your own trusted setup ceremony for additional security
2. Implement automated verification of trusted setup parameters
3. Add monitoring for parameter file integrity
4. Document the exact ceremony used in audit reports

## References

- [Perpetual Powers of Tau](https://github.com/privacy-scaling-explorations/perpetualpowersoftau)
- [KZG Polynomial Commitments](https://dankradfeist.de/ethereum/2020/06/16/kate-polynomial-commitments.html)
- [Trusted Setup Ceremonies](https://vitalik.ca/general/2022/03/14/trustedsetup.html)
- [Hermez Ceremony](https://blog.hermez.io/hermez-cryptographic-setup/)

## Conclusion

The current implementation is **UNSAFE FOR PRODUCTION** but has been documented and mitigated for development/testing:

- ✅ Security issue identified and documented
- ✅ Trusted setup downloaded
- ✅ Warnings added to code
- ✅ Solution documented
- ⚠️ Conversion to Halo2 format pending
- ⚠️ Production mode implementation pending

**DO NOT DEPLOY TO PRODUCTION** until the trusted setup is properly integrated and verified.

