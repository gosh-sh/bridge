# Full PLONK Verification Implementation Guide

## Overview

This guide provides detailed instructions for implementing complete PLONK verification in the Groth16 circuit.

## Architecture

```
Halo2 Proof (8224 bytes)
  ↓
Parse into components:
  - Commitments (G1 points)
  - Evaluations (field elements)
  - Opening proofs (G1 points)
  ↓
Verify in Groth16 circuit:
  1. Reconstruct Fiat-Shamir transcript
  2. Verify KZG openings
  3. Verify PLONK equations
  4. Verify permutation argument
  ↓
Generate Groth16 proof
  ↓
Deploy tiny verifier (<24KB)
```

## Step 1: Parse Halo2 Proof Structure

### Halo2/SHPLONK Proof Format

The 8224-byte proof contains (in order):

1. **Advice commitments** (variable number of G1 points, 64 bytes each)
2. **Challenges** (derived via Fiat-Shamir)
3. **Permutation commitments** (G1 points)
4. **Vanishing argument commitments** (G1 points)
5. **Evaluations** (field elements, 32 bytes each)
6. **Multi-opening proof** (SHPLONK batched opening, G1 points)

### Task 1.1: Create Proof Parser

**File:** `gnark-wrapper/proof_parser.go`

```go
package main

import (
    "encoding/binary"
    "fmt"
)

// G1Point represents a BN254 G1 point (64 bytes: 32 bytes x, 32 bytes y)
type G1Point struct {
    X [32]byte
    Y [32]byte
}

// FieldElement represents a BN254 scalar field element (32 bytes)
type FieldElement [32]byte

// ParsedProof contains the structured proof components
type ParsedProof struct {
    // Commitments
    AdviceCommitments      []G1Point
    PermutationCommitments []G1Point
    VanishingCommitments   []G1Point
    
    // Evaluations
    Evaluations []FieldElement
    
    // Opening proof (SHPLONK)
    OpeningProof G1Point
}

// ParseHalo2Proof parses the raw proof bytes into structured components
func ParseHalo2Proof(proofBytes []byte) (*ParsedProof, error) {
    // TODO: Implement proof parsing based on Halo2 proof format
    // This requires understanding the exact layout from snark-verifier
    return nil, fmt.Errorf("not implemented")
}
```

### Task 1.2: Study snark-verifier Proof Format

**Action:** Examine the Rust code to understand proof serialization:

```bash
# Find the proof serialization code
find ~/.cargo/git/checkouts -name "*.rs" -path "*/snark-verifier/*" \
    -exec grep -l "impl.*Encode.*Proof" {} \;
```

**Key files to study:**
- `snark-verifier/src/loader/native/proof.rs`
- `snark-verifier/src/system/halo2/proof.rs`
- `snark-verifier-sdk/src/snark.rs`

## Step 2: Implement KZG Commitment Verification

### KZG Opening Verification Equation

For a polynomial commitment `C` and claimed evaluation `v` at point `z`:

```
e(C - [v]₁, [1]₂) = e(W, [x]₂ - [z]₂)
```

Where:
- `C` = polynomial commitment (G1 point)
- `v` = evaluation (field element)
- `z` = evaluation point (field element)
- `W` = opening proof (G1 point)
- `[x]₂` = trusted setup SRS element (G2 point)
- `e(·,·)` = pairing function

### Task 2.1: Implement KZG Verification Gadget

**File:** `gnark-wrapper/kzg.go`

```go
package main

import (
    "github.com/consensys/gnark/frontend"
    "github.com/consensys/gnark/std/algebra/emulated/sw_bn254"
    "github.com/consensys/gnark/std/math/emulated"
)

// KZGVerifier implements KZG commitment verification in-circuit
type KZGVerifier struct {
    api frontend.API
    pairing *sw_bn254.Pairing
}

// NewKZGVerifier creates a new KZG verifier
func NewKZGVerifier(api frontend.API) *KZGVerifier {
    pairing, err := sw_bn254.NewPairing(api)
    if err != nil {
        panic(err)
    }
    return &KZGVerifier{
        api: api,
        pairing: pairing,
    }
}

// VerifyOpening verifies a KZG polynomial opening
func (kzg *KZGVerifier) VerifyOpening(
    commitment *sw_bn254.G1Affine,
    evaluation *emulated.Element[emulated.BN254Fr],
    point *emulated.Element[emulated.BN254Fr],
    proof *sw_bn254.G1Affine,
    srsG2 *sw_bn254.G2Affine,
) error {
    // TODO: Implement pairing check
    // e(C - [v]₁, [1]₂) = e(W, [x]₂ - [z]₂)
    return nil
}
```

### Task 2.2: Load Trusted Setup (SRS)

The KZG verification requires trusted setup parameters. For BN254 with degree 2^18:

**Option A:** Use Perpetual Powers of Tau ceremony
- Download from: https://github.com/privacy-scaling-explorations/perpetualpowersoftau

**Option B:** Use Halo2's KZG params
- Extract from your existing `kzg_bn254_18.srs` file

## Step 3: Implement PLONK Gate Verification

### PLONK Gate Equation

For each gate:

```
q_L(X)·a(X) + q_R(X)·b(X) + q_O(X)·c(X) + q_M(X)·a(X)·b(X) + q_C(X) = 0
```

Where:
- `q_L, q_R, q_O, q_M, q_C` = selector polynomials (from VK)
- `a, b, c` = wire polynomials (from proof)
- `X` = evaluation point (challenge)

### Task 3.1: Implement Gate Verification

**File:** `gnark-wrapper/plonk.go`

```go
package main

import (
    "github.com/consensys/gnark/frontend"
    "github.com/consensys/gnark/std/math/emulated"
)

// PLONKVerifier implements PLONK verification logic
type PLONKVerifier struct {
    api frontend.API
    field *emulated.Field[emulated.BN254Fr]
}

// VerifyGateConstraints verifies PLONK gate equations
func (plonk *PLONKVerifier) VerifyGateConstraints(
    qL, qR, qO, qM, qC *emulated.Element[emulated.BN254Fr],
    a, b, c *emulated.Element[emulated.BN254Fr],
) error {
    // Compute: q_L·a + q_R·b + q_O·c + q_M·a·b + q_C
    term1 := plonk.field.Mul(qL, a)
    term2 := plonk.field.Mul(qR, b)
    term3 := plonk.field.Mul(qO, c)
    term4 := plonk.field.Mul(qM, plonk.field.Mul(a, b))
    
    sum := plonk.field.Add(term1, term2)
    sum = plonk.field.Add(sum, term3)
    sum = plonk.field.Add(sum, term4)
    sum = plonk.field.Add(sum, qC)
    
    // Assert sum == 0
    plonk.field.AssertIsEqual(sum, plonk.field.Zero())
    
    return nil
}
```

## Step 4: Implement Permutation Argument

### Grand Product Argument

The permutation argument verifies copy constraints using:

```
Z(ωX) · ∏(a(X) + β·σ(X) + γ) = Z(X) · ∏(a(X) + β·X + γ)
```

Where:
- `Z(X)` = grand product polynomial
- `σ(X)` = permutation polynomial
- `β, γ` = random challenges
- `ω` = domain generator

### Task 4.1: Implement Permutation Verification

**File:** `gnark-wrapper/permutation.go`

```go
package main

import (
    "github.com/consensys/gnark/frontend"
    "github.com/consensys/gnark/std/math/emulated"
)

// PermutationVerifier implements PLONK permutation argument
type PermutationVerifier struct {
    api frontend.API
    field *emulated.Field[emulated.BN254Fr]
}

// VerifyPermutation verifies the grand product argument
func (perm *PermutationVerifier) VerifyPermutation(
    z, zOmega *emulated.Element[emulated.BN254Fr],
    a, sigma *emulated.Element[emulated.BN254Fr],
    beta, gamma *emulated.Element[emulated.BN254Fr],
    x, omega *emulated.Element[emulated.BN254Fr],
) error {
    // TODO: Implement grand product verification
    // Z(ωX) · ∏(a + β·σ + γ) = Z(X) · ∏(a + β·X + γ)
    return nil
}
```

## Step 5: Implement Fiat-Shamir Transcript

### Transcript Reconstruction

The verifier must reconstruct the same challenges as the prover:

1. **β, γ** ← Hash(VK, public_inputs, advice_commitments)
2. **α** ← Hash(transcript, permutation_commitments)
3. **ζ** ← Hash(transcript, vanishing_commitments)
4. **v, u** ← Hash(transcript, evaluations)

### Task 5.1: Implement Transcript

**File:** `gnark-wrapper/transcript.go`

```go
package main

import (
    "github.com/consensys/gnark/frontend"
    "github.com/consensys/gnark/std/hash/mimc"
)

// Transcript implements Fiat-Shamir transcript
type Transcript struct {
    api frontend.API
    hash *mimc.MiMC
    state frontend.Variable
}

// NewTranscript creates a new transcript
func NewTranscript(api frontend.API) *Transcript {
    hash, _ := mimc.NewMiMC(api)
    return &Transcript{
        api: api,
        hash: hash,
        state: 0,
    }
}

// AppendMessage adds data to transcript
func (t *Transcript) AppendMessage(data ...frontend.Variable) {
    for _, d := range data {
        t.hash.Write(d)
    }
    t.state = t.hash.Sum()
    t.hash.Reset()
}

// GetChallenge derives a challenge from current state
func (t *Transcript) GetChallenge() frontend.Variable {
    challenge := t.state
    t.AppendMessage(challenge)
    return challenge
}
```

## Step 6: Integration

### Task 6.1: Update Circuit

Integrate all components into `circuit.go`:

```go
func (circuit *Halo2VerifierCircuit) Define(api frontend.API) error {
    // 1. Parse proof
    proof := ParseProofFromBytes(circuit.ProofBytes)
    
    // 2. Reconstruct transcript
    transcript := NewTranscript(api)
    transcript.AppendMessage(circuit.PublicInputs...)
    beta := transcript.GetChallenge()
    gamma := transcript.GetChallenge()
    // ... derive all challenges
    
    // 3. Verify KZG openings
    kzg := NewKZGVerifier(api)
    kzg.VerifyOpening(...)
    
    // 4. Verify PLONK gates
    plonk := NewPLONKVerifier(api)
    plonk.VerifyGateConstraints(...)
    
    // 5. Verify permutation
    perm := NewPermutationVerifier(api)
    perm.VerifyPermutation(...)
    
    return nil
}
```

## Testing Strategy

1. **Unit tests:** Test each component separately
2. **Integration tests:** Test with real Halo2 proofs
3. **Gas benchmarks:** Measure on-chain costs
4. **Fuzzing:** Test with malformed proofs

## References

- [PLONK Paper](https://eprint.iacr.org/2019/953.pdf)
- [gnark Pairing Docs](https://pkg.go.dev/github.com/consensys/gnark/std/algebra/emulated/sw_bn254)
- [Halo2 Book](https://zcash.github.io/halo2/design/proving-system.html)
- [snark-verifier Source](https://github.com/privacy-scaling-explorations/snark-verifier)

