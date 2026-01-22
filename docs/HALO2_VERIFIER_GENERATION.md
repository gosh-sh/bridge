# Generating Halo2 Solidity Verifier

This document explains what's needed to generate a real Halo2 Solidity verifier from our circuits.

## Current Status

✅ **What We Have:**
- Complete Halo2 circuit structure in `crates/zk-proofs/src/withdrawal.rs`
- Circuit configuration and witness assignment
- Placeholder proof generation
- DummyVerifier for testing

❌ **What's Missing:**
- Actual circuit synthesis implementation
- Proper proof generation using Halo2 prover
- Solidity verifier generation from the circuit

## What Prevents Us (Steps 1 & 2)

### Step 1: Implement Proper Circuit Synthesis

**Current State:**
Our `WithdrawalCircuit::synthesize()` method is a placeholder:

```rust
fn synthesize(
    &self,
    _config: Self::Config,
    mut _layouter: impl Layouter<Fr>,
) -> std::result::Result<(), Error> {
    // TODO: Implement proper circuit synthesis using halo2-base
    // This requires:
    // 1. Create BaseCircuitBuilder
    // 2. Assign witness values (withdrawal_hash, nullifier)
    // 3. Compute commitment = Poseidon(withdrawal_hash, nullifier)
    // 4. Verify Merkle proof of commitment inclusion
    // 5. Verify burn proof
    // 6. Expose amount and merkle_root as public inputs

    // For now, this is a placeholder
    Ok(())
}
```

**What's Needed:**
1. **Use halo2-base API** to implement the circuit logic:
   ```rust
   use halo2_base::gates::circuit::builder::BaseCircuitBuilder;
   use halo2_base::gates::GateInstructions;
   use halo2_base::AssignedValue;
   ```

2. **Implement the circuit constraints:**
   - Assign witness values (withdrawal_hash, nullifier)
   - Compute commitment using Poseidon hash chip
   - Verify Merkle proof using Poseidon hash chip
   - Constrain public inputs (nullifier, recipient, amount, root)

3. **Configure the circuit properly:**
   - Set up advice columns, fixed columns, lookup tables
   - Configure gates and selectors
   - Set up instance columns for public inputs

**Complexity:** Medium-High
- Requires understanding of Halo2's constraint system
- Need to use halo2-base's vertical gate API
- Must integrate Poseidon hash chip from pse-poseidon

### Step 2: Generate Solidity Verifier

**Tool:** `halo2-solidity-verifier` (https://github.com/privacy-scaling-explorations/halo2-solidity-verifier)

**Process:**
1. **Add dependency** to `crates/zk-proofs/Cargo.toml`:
   ```toml
   [dependencies]
   halo2-solidity-verifier = { git = "https://github.com/privacy-scaling-explorations/halo2-solidity-verifier" }
   ```

2. **Generate verifier** after keygen:
   ```rust
   use halo2_solidity_verifier::{SolidityGenerator, Bdfg21};
   
   // After generating verification key
   let generator = SolidityGenerator::new(&params, &vk, Bdfg21, num_instances);
   let (verifier_solidity, vk_solidity) = generator.render_separately().unwrap();
   
   // Write to files
   std::fs::write("contracts/ethereum/src/Halo2Verifier.sol", verifier_solidity)?;
   std::fs::write("contracts/ethereum/src/Halo2VerifyingKey.sol", vk_solidity)?;
   ```

3. **Wrap generated verifier** to implement `IAckiNackiVerifier`:
   ```solidity
   // contracts/ethereum/src/Halo2VerifierWrapper.sol
   import "./IAckiNackiVerifier.sol";
   import "./Halo2Verifier.sol";
   
   contract Halo2VerifierWrapper is IAckiNackiVerifier {
       Halo2Verifier public verifier;
       
       constructor(address _verifier) {
           verifier = Halo2Verifier(_verifier);
       }
       
       function verifyWithdrawalProof(
           bytes calldata proof,
           uint256[] calldata publicInputs
       ) external view override returns (bool) {
           // Convert publicInputs to format expected by Halo2Verifier
           // Call verifier.verifyProof(proof, publicInputs)
           return verifier.verifyProof(proof, publicInputs);
       }
       
       function getPublicInputsCount() external pure override returns (uint256) {
           return 4;
       }
   }
   ```

**Complexity:** Low-Medium
- Tool is well-documented and maintained
- Main challenge is ensuring public input format matches

## Detailed Implementation Plan

### Phase 1: Implement Circuit Synthesis (1-2 weeks)

1. **Study halo2-base examples:**
   - Review axiom-crypto/halo2-lib examples
   - Understand BaseCircuitBuilder pattern
   - Learn vertical gate API

2. **Implement Poseidon hash chip integration:**
   - Use pse-poseidon with halo2-base
   - Create helper functions for hashing in circuit

3. **Implement Merkle proof verification:**
   - Create circuit logic to verify Merkle path
   - Use Poseidon hash for each level
   - Constrain root to match public input

4. **Implement commitment verification:**
   - Compute commitment = Poseidon(withdrawal_hash, nullifier)
   - Constrain commitment to be in Merkle tree

5. **Set up public inputs:**
   - Expose nullifier, recipient, amount, root as instance columns
   - Ensure proper ordering matches Solidity contract

6. **Test the circuit:**
   - Generate mock proofs
   - Verify proofs in Rust
   - Ensure constraints are satisfied

### Phase 2: Generate Solidity Verifier (1-3 days)

1. **Add halo2-solidity-verifier dependency**

2. **Create verifier generation script:**
   - Generate verification key
   - Generate Solidity verifier
   - Write to files

3. **Create wrapper contract:**
   - Implement IAckiNackiVerifier interface
   - Handle public input encoding

4. **Test verifier:**
   - Deploy to local Anvil
   - Generate proofs in Rust
   - Verify proofs in Solidity
   - Compare gas costs

### Phase 3: Integration (2-3 days)

1. **Update bridge contract:**
   - Deploy with Halo2VerifierWrapper address
   - Test withdrawal flow end-to-end

2. **Update integration tests:**
   - Generate real proofs
   - Test with actual verifier

3. **Documentation:**
   - Document verifier generation process
   - Update deployment instructions

## Key Challenges

### 1. Circuit Complexity
- **Challenge:** Implementing Merkle proof verification in circuit is complex
- **Solution:** Use existing examples from axiom-crypto/halo2-lib
- **Estimated Time:** 1 week

### 2. Public Input Encoding
- **Challenge:** Ensuring public inputs match between Rust and Solidity
- **Solution:** Carefully document encoding format, add tests
- **Estimated Time:** 1-2 days

### 3. Gas Optimization
- **Challenge:** Verifier gas costs might be high
- **Solution:** Use halo2-solidity-verifier's optimizations, consider proof aggregation
- **Estimated Time:** Ongoing

### 4. Circuit Parameters
- **Challenge:** Choosing optimal k (circuit size parameter)
- **Solution:** Start with k=17, benchmark and adjust
- **Estimated Time:** 2-3 days

## Limitations of halo2-solidity-verifier

From the README:

1. **Instance Column Limit:** Only ≤1 instance column allowed, no rotated queries
   - **Impact:** We need to pack all public inputs into a single column
   - **Solution:** Use a single instance column with 4 values

2. **Selector Compression:** Must disable selector compression for consistent VK
   - **Impact:** Need to use `keygen_vk_custom` with `compress_selectors: false`
   - **Solution:** Easy to implement

3. **Rust Version:** Currently requires Rust 1.77.0
   - **Impact:** Need to manage Rust toolchain version
   - **Solution:** Use rust-toolchain.toml

## Alternative: snark-verifier

**Pros:**
- More mature, audited by multiple teams
- Supports proof aggregation
- More flexible

**Cons:**
- More complex API
- Larger contract size
- Higher gas costs

**Recommendation:** Start with halo2-solidity-verifier for simplicity, migrate to snark-verifier if needed for production.

## Estimated Timeline

| Phase | Task | Time | Complexity |
|-------|------|------|------------|
| 1.1 | Study halo2-base | 2-3 days | Medium |
| 1.2 | Implement Poseidon chip | 1-2 days | Medium |
| 1.3 | Implement Merkle verification | 3-4 days | High |
| 1.4 | Implement commitment verification | 1-2 days | Low |
| 1.5 | Set up public inputs | 1 day | Low |
| 1.6 | Test circuit | 2-3 days | Medium |
| **Phase 1 Total** | | **10-15 days** | |
| 2.1 | Add dependency | 1 hour | Low |
| 2.2 | Create generation script | 1 day | Low |
| 2.3 | Create wrapper contract | 1 day | Low |
| 2.4 | Test verifier | 1 day | Medium |
| **Phase 2 Total** | | **3 days** | |
| 3.1 | Update bridge contract | 1 day | Low |
| 3.2 | Update integration tests | 1 day | Medium |
| 3.3 | Documentation | 1 day | Low |
| **Phase 3 Total** | | **3 days** | |
| **TOTAL** | | **16-21 days** | |

## Next Steps

To proceed with implementation:

1. **Immediate (Today):**
   - Review halo2-base documentation
   - Study axiom-crypto/halo2-lib examples
   - Set up development environment with Rust 1.77.0

2. **This Week:**
   - Implement basic circuit synthesis
   - Get Poseidon hash working in circuit
   - Create simple test case

3. **Next Week:**
   - Implement Merkle proof verification
   - Complete circuit implementation
   - Generate first real proof

4. **Week 3:**
   - Generate Solidity verifier
   - Test end-to-end flow
   - Optimize and document

## Resources

- **halo2-solidity-verifier:** https://github.com/privacy-scaling-explorations/halo2-solidity-verifier
- **halo2-lib (axiom-crypto):** https://github.com/axiom-crypto/halo2-lib
- **Halo2 Book:** https://zcash.github.io/halo2/
- **PSE Halo2:** https://github.com/privacy-scaling-explorations/halo2
- **Tornado Cash Circuits:** https://github.com/tornadocash/tornado-core (for reference)

## Conclusion

**What prevents us from steps 1 & 2:**

1. **Step 1 (Implement circuit synthesis):** 
   - Requires implementing actual constraint logic using halo2-base
   - Need to integrate Poseidon hash chip
   - Must implement Merkle proof verification in circuit
   - **Estimated effort:** 10-15 days

2. **Step 2 (Generate Solidity verifier):**
   - Relatively straightforward once circuit is complete
   - Use halo2-solidity-verifier tool
   - Create wrapper to implement our interface
   - **Estimated effort:** 3 days

**Total estimated effort:** 16-21 days of focused development

The main blocker is implementing the actual circuit logic, not the tooling. The tools exist and are well-maintained, but we need to properly implement our circuit constraints first.

