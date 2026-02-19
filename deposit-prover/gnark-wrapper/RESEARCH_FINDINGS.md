# Research Findings: gnark PLONK Verifier Analysis

## Summary

After studying gnark's PLONK recursion verifier and SP1's gnark-ffi implementation, I've identified the key components needed for implementing full PLONK verification in our Groth16 circuit.

## Key Findings

### 1. gnark Already Has PLONK In-Circuit Verifier! ✅

**Location:** `/tmp/gnark/std/recursion/plonk/verifier.go`

**This is HUGE:** gnark provides a complete, production-ready PLONK verifier that can be used inside a Groth16 circuit!

**Proof Structure (from gnark):**
```go
type Proof[FR emulated.FieldParams, G1El algebra.G1ElementT, G2El algebra.G2ElementT] struct {
    // Commitments to the solution vectors (L, R, O)
    LRO [3]kzg.Commitment[G1El]
    
    // Commitment to Z, the permutation polynomial
    Z kzg.Commitment[G1El]
    
    // Commitments to h1, h2, h3 (quotient polynomial)
    H [3]kzg.Commitment[G1El]
    
    // BSB22 commitments (for custom gates)
    Bsb22Commitments []kzg.Commitment[G1El]
    
    // Batch opening proof
    BatchedProof kzg.BatchOpeningProof[FR, G1El]
    
    // Opening proof of Z at zeta*mu
    ZShiftedOpening kzg.OpeningProof[FR, G1El]
}
```

### 2. Halo2 vs gnark PLONK Proof Format

**Problem:** Halo2 uses SHPLONK (different from standard PLONK)

**Halo2/SHPLONK proof contains:**
- Advice commitments (variable number)
- Permutation commitments
- Vanishing argument commitments
- Evaluations
- Multi-opening proof (SHPLONK batched opening)

**gnark PLONK proof expects:**
- LRO commitments (3 fixed)
- Z commitment (1)
- H commitments (3)
- Batch opening proof
- Z shifted opening

**Key Insight:** The formats are DIFFERENT. We cannot directly use gnark's PLONK verifier for Halo2 proofs!

### 3. SP1's Approach

SP1 does NOT verify PLONK proofs directly. Instead:

1. SP1 generates its own STARK proofs
2. Converts STARK verification into arithmetic constraints
3. Compiles those constraints into a gnark circuit
4. Wraps in Groth16

**Key Difference:** SP1 doesn't verify PLONK - it verifies STARKs!

### 4. What This Means for Us

We have **3 options:**

#### Option A: Adapt gnark's PLONK Verifier (Complex)

**Challenge:** Convert Halo2/SHPLONK proof format to gnark PLONK format

**Steps:**
1. Parse Halo2 proof bytes
2. Map Halo2 commitments to gnark PLONK structure
3. Handle SHPLONK → standard PLONK conversion
4. Use gnark's verifier

**Difficulty:** HIGH (proof format mismatch)
**Time:** 3-4 weeks

#### Option B: Implement SHPLONK Verifier from Scratch (Very Complex)

**Steps:**
1. Implement SHPLONK verification equations
2. Use gnark's KZG gadgets
3. Implement Halo2-specific logic

**Difficulty:** VERY HIGH (cryptographic complexity)
**Time:** 4-6 weeks
**Risk:** HIGH (easy to make mistakes)

#### Option C: Use Halo2's Native Aggregation (Simpler)

**Idea:** Instead of Groth16 wrapper, use Halo2's built-in aggregation

**Steps:**
1. Use snark-verifier-sdk's AggregationCircuit
2. Aggregate multiple proofs
3. Deploy aggregated verifier

**Difficulty:** MEDIUM
**Time:** 1-2 weeks
**Limitation:** May still exceed 24KB

## Recommended Approach

### NEW Option D: Hybrid Approach (RECOMMENDED)

**Insight:** We don't need to verify the FULL Halo2 proof in gnark!

**Strategy:**
1. **In Halo2:** Generate proof of deposit event
2. **In Halo2 Aggregation:** Verify the deposit proof + extract public inputs
3. **In gnark:** Verify ONLY the aggregated proof's public inputs match expected values
4. **Wrap in Groth16:** Generate tiny verifier

**Why This Works:**
- Halo2 aggregation already verifies the deposit proof
- gnark only needs to verify the aggregation proof's correctness
- Much simpler than full PLONK verification
- Leverages existing, audited code

**Implementation:**
```
Deposit Proof (Halo2)
  ↓
Aggregation Circuit (Halo2) - verifies deposit proof
  ↓ outputs: commitment to public inputs
Commitment Verifier (gnark) - verifies commitment
  ↓
Groth16 Proof
  ↓
Tiny Verifier (<24KB)
```

## Detailed Analysis: gnark PLONK Verifier

### Verifier Interface

```go
func Verify[FR emulated.FieldParams, G1El algebra.G1ElementT, G2El algebra.G2ElementT](
    api frontend.API,
    vk VerifyingKey[FR, G1El, G2El],
    proof Proof[FR, G1El, G2El],
    publicWitness Witness[FR],
    opts ...Option,
) error
```

### Key Components

1. **KZG Verification** (`std/commitments/kzg/verifier.go`)
   - Batch opening verification
   - Single opening verification
   - Uses pairing gadgets

2. **Fiat-Shamir Transcript** (`std/fiat-shamir`)
   - Challenge derivation
   - Transcript management

3. **Field Arithmetic** (`std/math/emulated`)
   - Emulated field operations
   - For non-native curves

4. **Pairing Operations** (`std/algebra/emulated/sw_bn254`)
   - Pairing gadgets
   - G1/G2 operations

### Verification Steps (from verifier.go)

1. Derive challenges (β, γ, α, ζ) via Fiat-Shamir
2. Compute linearization polynomial
3. Compute opening point
4. Verify batch opening proof
5. Verify Z shifted opening
6. Check public input binding

## Proof Format Analysis

### Halo2 Proof Bytes (8224 bytes)

Based on snark-verifier source, the proof contains (in order):

```
Offset | Size | Content
-------|------|--------
0      | 64×N | Advice commitments (N depends on circuit)
...    | 64×M | Permutation commitments
...    | 64×K | Vanishing commitments
...    | 32×L | Evaluations
...    | 64   | Multi-opening proof (W)
...    | 64   | Multi-opening proof (W')
```

**Challenge:** N, M, K, L are circuit-specific and not explicitly encoded in proof!

**Solution:** Extract from protocol data (verification key)

### Mapping to gnark PLONK

This is the HARD part. Halo2's proof structure doesn't directly map to gnark's expected format:

| Halo2 | gnark PLONK |
|-------|-------------|
| Advice commitments | LRO[3] ??? |
| Permutation commitments | Z ??? |
| Vanishing commitments | H[3] ??? |
| Multi-opening proof | BatchedProof ??? |

**Problem:** The structures don't align!

## Conclusion

**Key Realization:** Directly using gnark's PLONK verifier for Halo2 proofs is NOT straightforward due to proof format differences.

**Recommended Path Forward:**

1. **Short-term (1 week):** Implement Option D (Hybrid Approach)
   - Use Halo2 aggregation
   - Simple commitment verification in gnark
   - Wrap in Groth16

2. **Medium-term (2-3 weeks):** If Option D doesn't work:
   - Implement custom SHPLONK verifier
   - Use gnark's KZG gadgets as building blocks
   - Reference snark-verifier's Rust implementation

3. **Long-term (fallback):** Deploy on L2
   - Arbitrum/Optimism (48KB limit)
   - Plan Groth16 wrapper as v2

## Next Steps

1. **Test Option D:** Implement hybrid approach
2. **Measure verifier size:** Check if it's under 24KB
3. **If successful:** Deploy to mainnet
4. **If not:** Proceed with custom SHPLONK implementation

## Resources

- gnark PLONK verifier: `/tmp/gnark/std/recursion/plonk/verifier.go`
- gnark KZG gadgets: `/tmp/gnark/std/commitments/kzg/verifier.go`
- SP1 gnark-ffi: `/tmp/sp1/crates/recursion/gnark-ffi/go/sp1/`
- snark-verifier: `~/.cargo/git/checkouts/snark-verifier-*/`

