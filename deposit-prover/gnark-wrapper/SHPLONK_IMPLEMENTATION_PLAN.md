# SHPLONK Verifier Implementation Plan

## Overview

After studying snark-verifier's BDFG21 (SHPLONK) implementation, I now have a clear understanding of what needs to be implemented in gnark.

## SHPLONK Verification Algorithm

**Paper:** https://eprint.iacr.org/2020/081 (BDFG21)

**Key Insight:** SHPLONK is a batched polynomial commitment opening scheme that allows verifying multiple polynomial evaluations with a single pairing check.

### Verification Equation

Given:
- Commitments: `C_1, C_2, ..., C_n` (G1 points)
- Evaluation points: `z_1, z_2, ..., z_m`
- Claimed evaluations: `v_{i,j}` (polynomial i at point j)
- Proof: `(W, W')` (two G1 points)
- Challenges: `μ, γ, z'` (derived via Fiat-Shamir)

Verify:
```
e(F, [1]_2) = e(W', [x]_2 - [z']_2)
```

Where `F` is computed as:
```
F = Σ_i γ^i · (Σ_j μ^j · C_{i,j} - v_{i,j} · [1]_1) / (z' - z_i) - W
```

### Proof Structure

From snark-verifier's `Bdfg21Proof`:

```rust
struct Bdfg21Proof {
    mu: Scalar,        // Challenge (squeezed from transcript)
    gamma: Scalar,     // Challenge (squeezed from transcript)
    w: G1Point,        // First opening proof point
    z_prime: Scalar,   // Challenge (squeezed from transcript)
    w_prime: G1Point,  // Second opening proof point
}
```

**Important:** The challenges are NOT in the proof bytes! They are derived via Fiat-Shamir transcript.

### Halo2 Proof Structure

Based on snark-verifier's PLONK verifier, a Halo2 proof contains:

1. **Witness commitments** (advice columns)
2. **Permutation commitments** (Z polynomial)
3. **Quotient commitments** (H polynomial chunks)
4. **Evaluations** (polynomial evaluations at challenge point ζ)
5. **Multi-opening proof** (W, W' from SHPLONK)

## Implementation Strategy

### Phase 1: Understand Proof Encoding (Week 1)

**Goal:** Parse the 8224-byte proof into structured components

**Tasks:**
1. Study how snark-verifier encodes proofs (likely using `bincode` or custom encoding)
2. Identify the exact byte layout:
   - How many commitments?
   - How many evaluations?
   - Where is W and W'?
3. Create Go structs matching the structure
4. Implement parser in Go

**Deliverable:** `proof_parser.go` that decodes proof bytes into:
```go
type ParsedHalo2Proof struct {
    WitnessCommitments   []G1Point
    PermutationCommitments []G1Point
    QuotientCommitments  []G1Point
    Evaluations          []FieldElement
    W                    G1Point
    WPrime               G1Point
}
```

### Phase 2: Implement Fiat-Shamir Transcript (Week 2)

**Goal:** Reconstruct challenges (β, γ, α, ζ, μ, γ_kzg, z') from proof data

**Tasks:**
1. Study snark-verifier's transcript implementation
2. Understand the exact sequence of absorb/squeeze operations
3. Implement matching transcript in gnark circuit
4. Verify challenges match expected values

**Deliverable:** `transcript.go` that computes:
```go
type Challenges struct {
    Beta      emulated.Element[sw_bn254.ScalarField]
    Gamma     emulated.Element[sw_bn254.ScalarField]
    Alpha     emulated.Element[sw_bn254.ScalarField]
    Zeta      emulated.Element[sw_bn254.ScalarField]
    Mu        emulated.Element[sw_bn254.ScalarField]
    GammaKzg  emulated.Element[sw_bn254.ScalarField]
    ZPrime    emulated.Element[sw_bn254.ScalarField]
}
```

### Phase 3: Implement PLONK Gate Verification (Week 3)

**Goal:** Verify PLONK constraints are satisfied

**Tasks:**
1. Compute linearization polynomial evaluation
2. Verify gate constraints: `q_L·a + q_R·b + q_O·c + q_M·a·b + q_C = 0`
3. Verify permutation argument
4. Verify public input binding

**Deliverable:** `plonk_gates.go` that verifies gate equations

### Phase 4: Implement SHPLONK Verification (Week 4)

**Goal:** Verify polynomial commitment openings using SHPLONK

**Tasks:**
1. Implement query set grouping (group queries by evaluation point)
2. Compute coefficients for batching
3. Compute F = batched commitment
4. Verify pairing equation: `e(F, [1]_2) = e(W', [x]_2 - [z']_2)`

**Deliverable:** `shplonk_verifier.go` using gnark's KZG gadgets

### Phase 5: Integration & Testing (Week 5)

**Goal:** Put it all together and test

**Tasks:**
1. Integrate all components into main circuit
2. Test with real Halo2 proofs
3. Optimize circuit size
4. Measure Groth16 verifier size

**Deliverable:** Working end-to-end verification

### Phase 6: Optimization (Week 6)

**Goal:** Get verifier under 24KB

**Tasks:**
1. Identify bottlenecks in circuit
2. Optimize field operations
3. Reduce number of constraints
4. Use lookup tables where possible

**Deliverable:** Groth16 verifier <24KB

## Key Technical Challenges

### Challenge 1: Proof Encoding

**Problem:** We don't know the exact byte layout of the 8224-byte proof

**Solution:**
1. Study snark-verifier's `PlonkProof::read()` function
2. Trace through the transcript operations
3. Identify where each component is read
4. Replicate in Go

**Code to study:**
- `snark-verifier/src/verifier/plonk/proof.rs`
- `snark-verifier/src/util/transcript.rs`

### Challenge 2: Fiat-Shamir Transcript

**Problem:** Must exactly match snark-verifier's transcript to get same challenges

**Solution:**
1. Use same hash function (likely Keccak256 or Poseidon2)
2. Same absorb/squeeze sequence
3. Same encoding of field elements and points

**Critical:** Even a single bit difference will cause verification to fail!

### Challenge 3: Field Arithmetic in Circuit

**Problem:** All operations must be done in-circuit using emulated field arithmetic

**Solution:**
- Use `emulated.Element[sw_bn254.ScalarField]` for Fr operations
- Use gnark's pairing gadgets for pairing checks
- Batch operations where possible to reduce constraints

### Challenge 4: Circuit Size

**Problem:** Full PLONK+SHPLONK verification may create a large circuit

**Solution:**
1. Use gnark's optimized gadgets
2. Avoid redundant computations
3. Use lookup tables for common operations
4. Consider splitting verification into multiple circuits if needed

## Detailed Implementation: SHPLONK Verification

Based on `bdfg21.rs`, here's the exact algorithm:

```rust
fn verify(
    svk: &KzgSuccinctVerifyingKey,
    commitments: &[Msm],
    z: &Scalar,  // evaluation point (ζ in PLONK)
    queries: &[Query],
    proof: &Bdfg21Proof,
) -> KzgAccumulator {
    // 1. Group queries by evaluation point
    let sets = query_sets(queries);
    
    // 2. Compute coefficients for each query set
    let coeffs = query_set_coeffs(&sets, z, &proof.z_prime);
    
    // 3. Compute batched commitment F
    let powers_of_mu = proof.mu.powers(max_polys_per_set);
    let powers_of_gamma = proof.gamma.powers(num_sets);
    
    let f = sets.iter().zip(coeffs.iter())
        .map(|(set, coeff)| {
            // Batch commitments with μ
            let msm = set.polys.iter().zip(powers_of_mu.iter())
                .map(|(poly, mu_power)| commitments[poly] * mu_power)
                .sum();
            // Multiply by γ^i
            msm * powers_of_gamma[i]
        })
        .sum()
        - proof.w * coeffs[0].z_s;
    
    // 4. Return accumulator for pairing check
    // Verifier checks: e(f, [1]_2) = e(w_prime, [x]_2 - [z']_2)
    KzgAccumulator { lhs: f, rhs: proof.w_prime }
}
```

### In gnark Circuit

```go
func (circuit *Halo2VerifierCircuit) VerifyShplonk(
    api frontend.API,
    commitments []sw_bn254.G1Affine,
    z emulated.Element[sw_bn254.ScalarField],
    queries []Query,
    w sw_bn254.G1Affine,
    wPrime sw_bn254.G1Affine,
    mu emulated.Element[sw_bn254.ScalarField],
    gamma emulated.Element[sw_bn254.ScalarField],
    zPrime emulated.Element[sw_bn254.ScalarField],
) error {
    // 1. Group queries
    sets := groupQueries(queries)
    
    // 2. Compute coefficients
    coeffs := computeCoeffs(api, sets, z, zPrime)
    
    // 3. Compute F
    f := computeBatchedCommitment(api, sets, commitments, coeffs, mu, gamma, w)
    
    // 4. Verify pairing
    pairing := sw_bn254.NewPairing(api)
    
    // e(F, [1]_2) = e(W', [x]_2 - [z']_2)
    // Equivalent to: e(F, [1]_2) · e(-W', [x]_2 - [z']_2) = 1
    
    g2Gen := sw_bn254.GetG2Generator()
    g2Tau := getG2Tau() // [x]_2 from SRS
    
    // Compute [x - z']_2 = [x]_2 - z' · [1]_2
    g2Point := pairing.Sub(g2Tau, pairing.ScalarMul(g2Gen, zPrime))
    
    // Negate W'
    wPrimeNeg := pairing.Neg(wPrime)
    
    // Pairing check
    pairing.PairingCheck(
        []sw_bn254.G1Affine{f, wPrimeNeg},
        []sw_bn254.G2Affine{g2Gen, g2Point},
    )
    
    return nil
}
```

## Resources Needed

1. **SRS (Structured Reference String)**
   - Need [1]_1, [x]_1, [x^2]_1, ..., [x^n]_1 in G1
   - Need [1]_2, [x]_2 in G2
   - Can use Perpetual Powers of Tau ceremony results

2. **Verification Key**
   - Preprocessed commitments (54 G1 points)
   - Domain parameters (k=18, omega)
   - Number of instance/witness columns

3. **Test Vectors**
   - Known good Halo2 proofs
   - Expected intermediate values (challenges, evaluations)
   - For debugging and validation

## Success Criteria

1. ✅ Parse 8224-byte proof correctly
2. ✅ Reconstruct challenges matching snark-verifier
3. ✅ Verify PLONK gates
4. ✅ Verify SHPLONK opening
5. ✅ Generate Groth16 proof
6. ✅ Groth16 verifier <24KB
7. ✅ Gas cost <500k for on-chain verification

## Timeline

- **Week 1:** Proof parsing
- **Week 2:** Fiat-Shamir transcript
- **Week 3:** PLONK gates
- **Week 4:** SHPLONK verification
- **Week 5:** Integration & testing
- **Week 6:** Optimization

**Total: 6 weeks**

## Next Immediate Steps

1. Study `snark-verifier/src/verifier/plonk/proof.rs` to understand proof encoding
2. Create test that loads proof and prints byte offsets
3. Implement proof parser in Go
4. Verify parser works by comparing with Rust output

---

**Status:** Ready to begin Week 1 - Proof Parsing

