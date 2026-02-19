package main

import (
	"github.com/consensys/gnark/frontend"
	"github.com/consensys/gnark/std/algebra/emulated/sw_bn254"
	"github.com/consensys/gnark/std/math/emulated"
)

// PLONKVerifier implements PLONK gate constraint and permutation verification.
//
// PLONK (Permutations over Lagrange-bases for Oecumenical Noninteractive arguments of Knowledge)
// is a universal SNARK that uses custom gates and a permutation argument.
//
// The verifier checks:
// 1. Gate constraints: q_L·a + q_R·b + q_O·c + q_M·a·b + q_C = 0
// 2. Permutation argument: ∏(f + β·σ + γ) = ∏(f + β·id + γ)
// 3. Public input binding: L_0(X)·(a(X) - PI) = 0
//
// References:
// - PLONK paper: https://eprint.iacr.org/2019/953
// - snark-verifier implementation: plonk.rs
type PLONKVerifier struct {
	api   frontend.API
	field *emulated.Field[sw_bn254.ScalarField]
}

// NewPLONKVerifier creates a new PLONK verifier.
func NewPLONKVerifier(api frontend.API) (*PLONKVerifier, error) {
	field, err := emulated.NewField[sw_bn254.ScalarField](api)
	if err != nil {
		return nil, err
	}

	return &PLONKVerifier{
		api:   api,
		field: field,
	}, nil
}

// VerificationKey contains the PLONK verification key data.
//
// This includes:
// - Preprocessed commitments (fixed columns, permutation polynomials)
// - Domain parameters (size, generator)
// - Circuit-specific parameters
type VerificationKey struct {
	// Domain parameters
	DomainSize uint64 // 2^k (e.g., 262144 for k=18)

	// Preprocessed commitments (G1 points)
	// These are commitments to fixed columns and permutation polynomials
	PreprocessedCommitments []*sw_bn254.G1Affine

	// Number of public inputs
	NumPublicInputs int
}

// ProofEvaluations contains all polynomial evaluations at the challenge point ζ.
//
// In PLONK, the prover commits to polynomials and then proves their evaluations
// at a random challenge point ζ chosen by the verifier (via Fiat-Shamir).
type ProofEvaluations struct {
	// Witness polynomial evaluations
	// a(ζ), b(ζ), c(ζ), ... for each witness column
	Witnesses []*emulated.Element[sw_bn254.ScalarField]

	// Permutation polynomial evaluation
	// Z(ζ) - the grand product polynomial
	Permutation *emulated.Element[sw_bn254.ScalarField]

	// Permutation polynomial evaluation at ωζ
	// Z(ωζ) - needed for permutation check
	PermutationNext *emulated.Element[sw_bn254.ScalarField]

	// Fixed column evaluations (selectors)
	// q_L(ζ), q_R(ζ), q_O(ζ), q_M(ζ), q_C(ζ), ...
	Fixed []*emulated.Element[sw_bn254.ScalarField]

	// Permutation polynomial evaluations
	// σ_1(ζ), σ_2(ζ), σ_3(ζ), ...
	Permutations []*emulated.Element[sw_bn254.ScalarField]
}

// VerifyGateConstraints verifies the PLONK gate constraints.
//
// For each gate, we verify:
//
//	q_L·a + q_R·b + q_O·c + q_M·a·b + q_C = 0
//
// At the challenge point ζ, this becomes:
//
//	q_L(ζ)·a(ζ) + q_R(ζ)·b(ζ) + q_O(ζ)·c(ζ) + q_M(ζ)·a(ζ)·b(ζ) + q_C(ζ) = 0
//
// Parameters:
//   - evaluations: All polynomial evaluations at ζ
//   - alpha: Random challenge for batching constraints
//
// Returns the gate constraint polynomial evaluation at ζ.
func (plonk *PLONKVerifier) VerifyGateConstraints(
	evaluations *ProofEvaluations,
	alpha *emulated.Element[sw_bn254.ScalarField],
) *emulated.Element[sw_bn254.ScalarField] {
	// For simplicity, we assume standard PLONK gates with 3 wires (a, b, c)
	// and 5 selectors (q_L, q_R, q_O, q_M, q_C)

	// Extract evaluations
	a := evaluations.Witnesses[0]
	b := evaluations.Witnesses[1]
	c := evaluations.Witnesses[2]

	qL := evaluations.Fixed[0]
	qR := evaluations.Fixed[1]
	qO := evaluations.Fixed[2]
	qM := evaluations.Fixed[3]
	qC := evaluations.Fixed[4]

	// Compute gate constraint: q_L·a + q_R·b + q_O·c + q_M·a·b + q_C
	term1 := plonk.field.Mul(qL, a)
	term2 := plonk.field.Mul(qR, b)
	term3 := plonk.field.Mul(qO, c)
	term4 := plonk.field.Mul(qM, plonk.field.Mul(a, b))

	result := plonk.field.Add(term1, term2)
	result = plonk.field.Add(result, term3)
	result = plonk.field.Add(result, term4)
	result = plonk.field.Add(result, qC)

	return result
}

// VerifyPermutationArgument verifies the PLONK permutation (copy constraint) argument.
//
// The permutation argument ensures that wires that should have the same value
// actually do have the same value (copy constraints).
//
// The check is:
//
//	Z(ωζ) · ∏(a + β·σ + γ) = Z(ζ) · ∏(a + β·ζ·ω^i + γ)
//
// Where:
//   - Z is the grand product polynomial
//   - β, γ are random challenges
//   - σ are the permutation polynomials
//   - ω is the domain generator
//
// Parameters:
//   - evaluations: All polynomial evaluations at ζ
//   - beta: Permutation challenge β
//   - gamma: Permutation challenge γ
//   - zeta: Evaluation point ζ
//   - omega: Domain generator ω
//
// Returns the permutation constraint polynomial evaluation at ζ.
func (plonk *PLONKVerifier) VerifyPermutationArgument(
	evaluations *ProofEvaluations,
	beta *emulated.Element[sw_bn254.ScalarField],
	gamma *emulated.Element[sw_bn254.ScalarField],
	zeta *emulated.Element[sw_bn254.ScalarField],
	omega *emulated.Element[sw_bn254.ScalarField],
) *emulated.Element[sw_bn254.ScalarField] {
	// Extract evaluations
	a := evaluations.Witnesses[0]
	b := evaluations.Witnesses[1]
	c := evaluations.Witnesses[2]

	z := evaluations.Permutation
	zNext := evaluations.PermutationNext

	sigma1 := evaluations.Permutations[0]
	sigma2 := evaluations.Permutations[1]
	sigma3 := evaluations.Permutations[2]

	// Compute left side: Z(ωζ) · ∏(a + β·σ + γ)
	// = Z(ωζ) · (a + β·σ_1 + γ) · (b + β·σ_2 + γ) · (c + β·σ_3 + γ)

	term1 := plonk.field.Add(a, plonk.field.Add(plonk.field.Mul(beta, sigma1), gamma))
	term2 := plonk.field.Add(b, plonk.field.Add(plonk.field.Mul(beta, sigma2), gamma))
	term3 := plonk.field.Add(c, plonk.field.Add(plonk.field.Mul(beta, sigma3), gamma))

	leftSide := plonk.field.Mul(zNext, plonk.field.Mul(term1, plonk.field.Mul(term2, term3)))

	// Compute right side: Z(ζ) · ∏(a + β·ζ·ω^i + γ)
	// = Z(ζ) · (a + β·ζ + γ) · (b + β·ζ·ω + γ) · (c + β·ζ·ω² + γ)

	zetaOmega := plonk.field.Mul(zeta, omega)
	zetaOmega2 := plonk.field.Mul(zetaOmega, omega)

	term1Right := plonk.field.Add(a, plonk.field.Add(plonk.field.Mul(beta, zeta), gamma))
	term2Right := plonk.field.Add(b, plonk.field.Add(plonk.field.Mul(beta, zetaOmega), gamma))
	term3Right := plonk.field.Add(c, plonk.field.Add(plonk.field.Mul(beta, zetaOmega2), gamma))

	rightSide := plonk.field.Mul(z, plonk.field.Mul(term1Right, plonk.field.Mul(term2Right, term3Right)))

	// Return the difference (should be zero)
	return plonk.field.Sub(leftSide, rightSide)
}

// VerifyPublicInputBinding verifies that public inputs are correctly bound.
//
// The check is:
//
//	L_0(ζ) · (a(ζ) - PI) = 0
//
// Where:
//   - L_0 is the Lagrange polynomial for the first row
//   - PI is the public input
//   - a(ζ) is the witness evaluation at ζ
//
// For multiple public inputs, we check:
//
//	∑_i L_i(ζ) · (a(ζ) - PI_i) = 0
//
// Parameters:
//   - publicInputs: The public input values
//   - evaluations: All polynomial evaluations at ζ
//   - zeta: Evaluation point ζ
//   - domainSize: Size of the evaluation domain (2^k)
//
// Returns the public input constraint polynomial evaluation at ζ.
func (plonk *PLONKVerifier) VerifyPublicInputBinding(
	publicInputs []*emulated.Element[sw_bn254.ScalarField],
	evaluations *ProofEvaluations,
	zeta *emulated.Element[sw_bn254.ScalarField],
	domainSize uint64,
) *emulated.Element[sw_bn254.ScalarField] {
	// Compute Lagrange polynomials L_i(ζ) for i = 0, 1, ..., n-1
	// L_i(ζ) = ω^i · (ζ^n - 1) / (n · (ζ - ω^i))
	// where ω is the domain generator and n is the domain size

	// For simplicity, we'll use a precomputed approach
	// In a full implementation, we'd compute these dynamically

	// For now, just verify the first public input
	// L_0(ζ) = (ζ^n - 1) / (n · (ζ - 1))

	// Compute ζ^n by repeated squaring
	zetaN := zeta
	for i := uint64(1); i < domainSize; i++ {
		zetaN = plonk.field.Mul(zetaN, zeta)
	}

	// Compute ζ^n - 1
	zetaNMinus1 := plonk.field.Sub(zetaN, plonk.field.One())

	// Compute ζ - 1
	zetaMinus1 := plonk.field.Sub(zeta, plonk.field.One())

	// Compute L_0(ζ) = (ζ^n - 1) / (n · (ζ - 1))
	n := plonk.field.NewElement(domainSize)
	denominator := plonk.field.Mul(n, zetaMinus1)
	l0 := plonk.field.Div(zetaNMinus1, denominator)

	// Verify: L_0(ζ) · (a(ζ) - PI_0) = 0
	a := evaluations.Witnesses[0]
	pi0 := publicInputs[0]

	diff := plonk.field.Sub(a, pi0)
	result := plonk.field.Mul(l0, diff)

	// For multiple public inputs, we'd sum over all of them
	// For now, we just return the first one

	return result
}

// VerifyQuotientPolynomial verifies the quotient polynomial.
//
// The quotient polynomial t(X) is defined as:
//
//	t(X) = (gate_constraints(X) + α·permutation_constraint(X) + α²·public_input_constraint(X)) / Z_H(X)
//
// Where Z_H(X) = X^n - 1 is the vanishing polynomial of the domain.
//
// At the challenge point ζ, we verify:
//
//	t(ζ) · Z_H(ζ) = gate_constraints(ζ) + α·permutation_constraint(ζ) + α²·public_input_constraint(ζ)
//
// Parameters:
//   - quotientEval: The quotient polynomial evaluation t(ζ)
//   - gateConstraint: Gate constraint evaluation at ζ
//   - permutationConstraint: Permutation constraint evaluation at ζ
//   - publicInputConstraint: Public input constraint evaluation at ζ
//   - alpha: Random challenge for batching
//   - zeta: Evaluation point ζ
//   - domainSize: Size of the evaluation domain (2^k)
//
// Returns true if the quotient polynomial is correct.
func (plonk *PLONKVerifier) VerifyQuotientPolynomial(
	quotientEval *emulated.Element[sw_bn254.ScalarField],
	gateConstraint *emulated.Element[sw_bn254.ScalarField],
	permutationConstraint *emulated.Element[sw_bn254.ScalarField],
	publicInputConstraint *emulated.Element[sw_bn254.ScalarField],
	alpha *emulated.Element[sw_bn254.ScalarField],
	zeta *emulated.Element[sw_bn254.ScalarField],
	domainSize uint64,
) {
	// Compute Z_H(ζ) = ζ^n - 1
	zetaN := zeta
	for i := uint64(1); i < domainSize; i++ {
		zetaN = plonk.field.Mul(zetaN, zeta)
	}
	zH := plonk.field.Sub(zetaN, plonk.field.One())

	// Compute left side: t(ζ) · Z_H(ζ)
	leftSide := plonk.field.Mul(quotientEval, zH)

	// Compute right side: gate + α·perm + α²·pi
	alpha2 := plonk.field.Mul(alpha, alpha)

	rightSide := gateConstraint
	rightSide = plonk.field.Add(rightSide, plonk.field.Mul(alpha, permutationConstraint))
	rightSide = plonk.field.Add(rightSide, plonk.field.Mul(alpha2, publicInputConstraint))

	// Verify: left side = right side
	plonk.api.AssertIsEqual(leftSide, rightSide)
}
