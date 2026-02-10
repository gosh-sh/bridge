package main

import (
	"github.com/consensys/gnark/frontend"
	"github.com/consensys/gnark/std/algebra/emulated/sw_bn254"
	"github.com/consensys/gnark/std/algebra/emulated/sw_emulated"
	"github.com/consensys/gnark/std/math/emulated"
)

// KZGVerifier implements KZG polynomial commitment verification in-circuit.
//
// KZG (Kate-Zaverucha-Goldberg) commitments allow committing to a polynomial
// and later proving its evaluation at a specific point.
//
// Verification equation:
//
//	e(C - [v]₁, [1]₂) = e(π, [x]₂ - [z]₂)
//
// Where:
//   - C = polynomial commitment (G1 point)
//   - v = claimed evaluation at point z
//   - π = opening proof (G1 point)
//   - z = evaluation point
//   - [x]₂ = G2 generator from SRS (trusted setup)
//
// This can be rewritten as a single pairing check:
//
//	e(C - [v]₁, [1]₂) · e(-π, [x]₂ - [z]₂) = 1
//
// References:
// - KZG paper: https://www.iacr.org/archive/asiacrypt2010/6477178/6477178.pdf
// - gnark pairing gadgets: https://pkg.go.dev/github.com/consensys/gnark/std/algebra/emulated/sw_bn254
type KZGVerifier struct {
	api     frontend.API
	pairing *sw_bn254.Pairing
	curve   *sw_emulated.Curve[sw_bn254.BaseField, sw_bn254.ScalarField]
}

// NewKZGVerifier creates a new KZG verifier.
func NewKZGVerifier(api frontend.API) (*KZGVerifier, error) {
	pairing, err := sw_bn254.NewPairing(api)
	if err != nil {
		return nil, err
	}

	curve, err := sw_emulated.New[sw_bn254.BaseField, sw_bn254.ScalarField](api, sw_emulated.GetBN254Params())
	if err != nil {
		return nil, err
	}

	return &KZGVerifier{
		api:     api,
		pairing: pairing,
		curve:   curve,
	}, nil
}

// VerifyOpening verifies a KZG commitment opening.
//
// Parameters:
//   - commitment: The polynomial commitment C (G1 point)
//   - proof: The opening proof π (G1 point)
//   - point: The evaluation point z (scalar)
//   - evaluation: The claimed evaluation v = p(z) (scalar)
//   - g2: The G2 generator [1]₂
//   - g2Tau: The G2 SRS element [x]₂
//
// Verifies: e(C - [v]₁, [1]₂) = e(π, [x]₂ - [z]₂)
func (kzg *KZGVerifier) VerifyOpening(
	commitment *sw_bn254.G1Affine,
	proof *sw_bn254.G1Affine,
	point *emulated.Element[sw_bn254.ScalarField],
	evaluation *emulated.Element[sw_bn254.ScalarField],
	g2 *sw_bn254.G2Affine,
	g2Tau *sw_bn254.G2Affine,
) error {
	// Compute C - [v]₁ = C - v·G₁
	// where G₁ is the G1 generator
	g1 := kzg.curve.Generator()
	vG1 := kzg.curve.ScalarMul(g1, evaluation)
	negVG1 := kzg.curve.Neg(vG1)
	cMinusV := kzg.curve.Add(commitment, negVG1)

	// For G2 operations, we need to use the Ext2 field arithmetic
	// Compute [x]₂ - [z]₂ = [x]₂ - z·[1]₂
	// This is complex in gnark, so we'll use a different approach:
	// We can rewrite the pairing check to avoid G2 scalar multiplication

	// Instead of: e(C - [v]₁, [1]₂) = e(π, [x]₂ - [z]₂)
	// We use: e(C - [v]₁, [1]₂) · e(-π, [x]₂) · e(z·π, [1]₂) = 1

	// Compute z·π
	zProof := kzg.curve.ScalarMul(proof, point)

	// Negate proof
	negProof := kzg.curve.Neg(proof)

	// Perform pairing check
	// e(C - [v]₁, [1]₂) · e(-π, [x]₂) · e(z·π, [1]₂) = 1
	kzg.pairing.PairingCheck(
		[]*sw_bn254.G1Affine{cMinusV, negProof, zProof},
		[]*sw_bn254.G2Affine{g2, g2Tau, g2},
	)

	return nil
}

// VerifyBatchOpening verifies multiple KZG openings at the same point.
//
// This is more efficient than verifying each opening individually.
//
// Parameters:
//   - commitments: List of polynomial commitments [C₁, C₂, ..., Cₙ]
//   - proof: Single opening proof π
//   - point: The evaluation point z (same for all)
//   - evaluations: List of claimed evaluations [v₁, v₂, ..., vₙ]
//   - powers: Random powers [1, r, r², ..., rⁿ⁻¹] for batching
//   - g2: The G2 generator [1]₂
//   - g2Tau: The G2 SRS element [x]₂
//
// Verifies: e(∑ᵢ rⁱ⁻¹·(Cᵢ - [vᵢ]₁), [1]₂) = e(π, [x]₂ - [z]₂)
func (kzg *KZGVerifier) VerifyBatchOpening(
	commitments []*sw_bn254.G1Affine,
	proof *sw_bn254.G1Affine,
	point *emulated.Element[sw_bn254.ScalarField],
	evaluations []*emulated.Element[sw_bn254.ScalarField],
	powers []*emulated.Element[sw_bn254.ScalarField],
	g2 *sw_bn254.G2Affine,
	g2Tau *sw_bn254.G2Affine,
) error {
	if len(commitments) != len(evaluations) || len(commitments) != len(powers) {
		panic("Mismatched lengths in batch opening")
	}

	// Compute batched commitment: ∑ᵢ rⁱ⁻¹·Cᵢ
	batchedCommitment := kzg.curve.ScalarMul(commitments[0], powers[0])
	for i := 1; i < len(commitments); i++ {
		scaled := kzg.curve.ScalarMul(commitments[i], powers[i])
		batchedCommitment = kzg.curve.Add(batchedCommitment, scaled)
	}

	// Compute batched evaluation: ∑ᵢ rⁱ⁻¹·vᵢ
	// We need to use emulated field arithmetic
	field, err := emulated.NewField[sw_bn254.ScalarField](kzg.api)
	if err != nil {
		panic("Failed to create emulated field")
	}

	batchedEval := field.Mul(powers[0], evaluations[0])
	for i := 1; i < len(evaluations); i++ {
		term := field.Mul(powers[i], evaluations[i])
		batchedEval = field.Add(batchedEval, term)
	}

	// Verify the batched opening
	return kzg.VerifyOpening(batchedCommitment, proof, point, batchedEval, g2, g2Tau)
}

// SHPLONKVerifier implements SHPLONK (batched polynomial opening) verification.
//
// SHPLONK is a batched polynomial commitment scheme that allows verifying
// multiple polynomial openings at different points with a single pairing check.
//
// Verification equation:
//
//	e(F, [1]₂) = e(W', [x]₂ - [z']₂)
//
// Where F is computed as:
//
//	F = ∑ᵢ γⁱ · (∑ⱼ μʲ · Cᵢⱼ - vᵢⱼ · [1]₁) / (z' - zᵢ) - W
//
// References:
// - SHPLONK paper: https://eprint.iacr.org/2020/081
// - snark-verifier implementation: bdfg21.rs
type SHPLONKVerifier struct {
	kzg *KZGVerifier
	api frontend.API
}

// NewSHPLONKVerifier creates a new SHPLONK verifier.
func NewSHPLONKVerifier(api frontend.API) (*SHPLONKVerifier, error) {
	kzg, err := NewKZGVerifier(api)
	if err != nil {
		return nil, err
	}

	return &SHPLONKVerifier{
		kzg: kzg,
		api: api,
	}, nil
}

// QuerySet represents a set of polynomial queries at the same point.
type QuerySet struct {
	Point       *emulated.Element[sw_bn254.ScalarField]   // Evaluation point z
	Commitments []*sw_bn254.G1Affine                      // Polynomial commitments
	Evaluations []*emulated.Element[sw_bn254.ScalarField] // Claimed evaluations
}

// VerifySHPLONK verifies a SHPLONK multi-opening proof.
//
// Parameters:
//   - querySets: List of query sets (polynomials evaluated at different points)
//   - w: First opening proof W (G1 point)
//   - wPrime: Second opening proof W' (G1 point)
//   - mu: Batching challenge μ
//   - gamma: Batching challenge γ
//   - zPrime: Evaluation point z'
//   - g2: The G2 generator [1]₂
//   - g2Tau: The G2 SRS element [x]₂
//
// Verifies: e(F, [1]₂) = e(W', [x]₂ - [z']₂)
// Where: F = ∑ᵢ γⁱ · (∑ⱼ μʲ · Cᵢⱼ - vᵢⱼ · [1]₁) / (z' - zᵢ) - W
func (shplonk *SHPLONKVerifier) VerifySHPLONK(
	querySets []*QuerySet,
	w *sw_bn254.G1Affine,
	wPrime *sw_bn254.G1Affine,
	mu *emulated.Element[sw_bn254.ScalarField],
	gamma *emulated.Element[sw_bn254.ScalarField],
	zPrime *emulated.Element[sw_bn254.ScalarField],
	g2 *sw_bn254.G2Affine,
	g2Tau *sw_bn254.G2Affine,
) error {
	field, err := emulated.NewField[sw_bn254.ScalarField](shplonk.api)
	if err != nil {
		return err
	}

	curve := shplonk.kzg.curve
	g1 := curve.Generator()

	// Compute F = ∑ᵢ γⁱ · (∑ⱼ μʲ · Cᵢⱼ - vᵢⱼ · [1]₁) / (z' - zᵢ) - W

	// Start with F = -W
	f := curve.Neg(w)

	// Compute powers of γ: [1, γ, γ², ...]
	gammaPowers := make([]*emulated.Element[sw_bn254.ScalarField], len(querySets))
	gammaPowers[0] = field.One()
	for i := 1; i < len(querySets); i++ {
		gammaPowers[i] = field.Mul(gammaPowers[i-1], gamma)
	}

	// For each query set
	for i, qs := range querySets {
		// Compute powers of μ: [1, μ, μ², ...]
		muPowers := make([]*emulated.Element[sw_bn254.ScalarField], len(qs.Commitments))
		muPowers[0] = field.One()
		for j := 1; j < len(qs.Commitments); j++ {
			muPowers[j] = field.Mul(muPowers[j-1], mu)
		}

		// Compute ∑ⱼ μʲ · Cᵢⱼ
		batchedCommitment := curve.ScalarMul(qs.Commitments[0], muPowers[0])
		for j := 1; j < len(qs.Commitments); j++ {
			scaled := curve.ScalarMul(qs.Commitments[j], muPowers[j])
			batchedCommitment = curve.Add(batchedCommitment, scaled)
		}

		// Compute ∑ⱼ μʲ · vᵢⱼ
		batchedEval := field.Mul(muPowers[0], qs.Evaluations[0])
		for j := 1; j < len(qs.Evaluations); j++ {
			term := field.Mul(muPowers[j], qs.Evaluations[j])
			batchedEval = field.Add(batchedEval, term)
		}

		// Compute vᵢⱼ · [1]₁
		evalG1 := curve.ScalarMul(g1, batchedEval)

		// Compute Cᵢⱼ - vᵢⱼ · [1]₁
		negEvalG1 := curve.Neg(evalG1)
		diff := curve.Add(batchedCommitment, negEvalG1)

		// Compute 1 / (z' - zᵢ)
		denominator := field.Sub(zPrime, qs.Point)
		invDenominator := field.Inverse(denominator)

		// Compute γⁱ / (z' - zᵢ)
		coefficient := field.Mul(gammaPowers[i], invDenominator)

		// Compute γⁱ · (Cᵢⱼ - vᵢⱼ · [1]₁) / (z' - zᵢ)
		term := curve.ScalarMul(diff, coefficient)

		// Add to F
		f = curve.Add(f, term)
	}

	// Verify: e(F, [1]₂) = e(W', [x]₂ - [z']₂)
	// Rewrite as: e(F, [1]₂) · e(-W', [x]₂) · e(z'·W', [1]₂) = 1
	// This avoids G2 scalar multiplication

	// Compute z'·W'
	zPrimeWPrime := curve.ScalarMul(wPrime, zPrime)

	// Negate W'
	negWPrime := curve.Neg(wPrime)

	// Perform pairing check
	// e(F, [1]₂) · e(-W', [x]₂) · e(z'·W', [1]₂) = 1
	shplonk.kzg.pairing.PairingCheck(
		[]*sw_bn254.G1Affine{f, negWPrime, zPrimeWPrime},
		[]*sw_bn254.G2Affine{g2, g2Tau, g2},
	)

	return nil
}
