package main

import (
	"github.com/consensys/gnark/frontend"
	"github.com/consensys/gnark/std/hash/sha3"
	"github.com/consensys/gnark/std/math/uints"
)

// Transcript implements the Fiat-Shamir transcript for PLONK verification.
//
// This matches snark-verifier's EvmTranscript which uses Keccak256 as the hash function.
// The transcript maintains a buffer of absorbed data and squeezes challenges from it.
//
// Key operations:
// - CommonScalar: Absorb a scalar field element (32 bytes, big-endian)
// - CommonEcPoint: Absorb an elliptic curve point (64 bytes: x || y, big-endian)
// - SqueezeChallenge: Hash the buffer and extract a challenge
//
// The implementation must exactly match snark-verifier's transcript to produce
// the same challenges.
type Transcript struct {
	api    frontend.API
	buffer []frontend.Variable // Buffer of absorbed data (as bytes)
}

// NewTranscript creates a new Fiat-Shamir transcript.
func NewTranscript(api frontend.API) *Transcript {
	return &Transcript{
		api:    api,
		buffer: make([]frontend.Variable, 0, 1024), // Pre-allocate for efficiency
	}
}

// CommonScalar absorbs a scalar field element into the transcript.
//
// The scalar is encoded as 32 bytes in big-endian format (matching EVM encoding).
// This matches snark-verifier's common_scalar implementation.
func (t *Transcript) CommonScalar(scalar frontend.Variable) {
	// Convert scalar to 32 bytes (big-endian)
	// In gnark, we work with Variables which represent field elements
	// We need to convert to bytes for hashing
	bytes := t.scalarToBytes(scalar)
	t.buffer = append(t.buffer, bytes...)
}

// CommonEcPoint absorbs an elliptic curve point into the transcript.
//
// The point is encoded as 64 bytes: x (32 bytes) || y (32 bytes), big-endian.
// This matches snark-verifier's common_ec_point implementation.
func (t *Transcript) CommonEcPoint(x, y frontend.Variable) {
	// Convert x and y coordinates to bytes (big-endian)
	xBytes := t.scalarToBytes(x)
	yBytes := t.scalarToBytes(y)
	t.buffer = append(t.buffer, xBytes...)
	t.buffer = append(t.buffer, yBytes...)
}

// SqueezeChallenge derives a challenge from the current transcript state.
//
// This matches snark-verifier's squeeze_challenge implementation:
// 1. If buffer is exactly 32 bytes, append 0x01
// 2. Hash the buffer with Keccak256
// 3. Reduce the hash modulo the field order to get the challenge
// 4. Replace buffer with the hash for next squeeze
//
// Returns the challenge as a field element.
func (t *Transcript) SqueezeChallenge() frontend.Variable {
	// If buffer is exactly 32 bytes, append 0x01
	// This prevents collision between empty input and [0x01]
	if len(t.buffer) == 32 {
		t.buffer = append(t.buffer, 1)
	}

	// Hash the buffer with Keccak256
	hash := t.keccak256(t.buffer)

	// The hash is 32 bytes (256 bits)
	// We need to reduce it modulo the BN254 scalar field order
	challenge := t.bytesToScalar(hash)

	// Replace buffer with hash for next squeeze
	t.buffer = hash

	return challenge
}

// Reset resets the transcript to initial state.
func (t *Transcript) Reset() {
	t.buffer = t.buffer[:0]
}

// scalarToBytes converts a scalar field element to 32 bytes (big-endian).
//
// In gnark circuits, Variables represent field elements.
// We need to convert them to byte representation for hashing.
func (t *Transcript) scalarToBytes(scalar frontend.Variable) []frontend.Variable {
	// Use gnark's ToBinary to convert to bits
	bits := t.api.ToBinary(scalar, 256)

	// Convert bits to bytes (big-endian)
	bytes := make([]frontend.Variable, 32)
	for i := 0; i < 32; i++ {
		// Each byte is 8 bits
		// Big-endian: most significant byte first
		byteVal := frontend.Variable(0)
		for j := 0; j < 8; j++ {
			bitIdx := (31-i)*8 + (7 - j) // Big-endian bit ordering
			bit := bits[bitIdx]
			byteVal = t.api.Add(byteVal, t.api.Mul(bit, 1<<j))
		}
		bytes[i] = byteVal
	}

	return bytes
}

// bytesToScalar converts 32 bytes (big-endian) to a scalar field element.
//
// The bytes represent a 256-bit integer which we reduce modulo the field order.
func (t *Transcript) bytesToScalar(bytes []frontend.Variable) frontend.Variable {
	// Convert bytes to bits (big-endian)
	bits := make([]frontend.Variable, 256)
	for i := 0; i < 32; i++ {
		byteBits := t.api.ToBinary(bytes[i], 8)
		for j := 0; j < 8; j++ {
			bitIdx := i*8 + (7 - j) // Big-endian bit ordering
			bits[bitIdx] = byteBits[j]
		}
	}

	// Convert bits to scalar
	// This automatically reduces modulo the field order
	scalar := t.api.FromBinary(bits...)

	return scalar
}

// keccak256 computes the Keccak256 hash of the input bytes.
//
// This uses gnark's Keccak256 gadget which implements the hash function
// in-circuit using constraints.
//
// Returns 32 bytes (256 bits) as Variables.
func (t *Transcript) keccak256(input []frontend.Variable) []frontend.Variable {
	// Create Bytes helper for converting Variables to U8
	bytesAPI, err := uints.NewBytes(t.api)
	if err != nil {
		panic("Failed to create Bytes API")
	}

	// Convert Variables (bytes) to uints.U8 for Keccak256 gadget
	u8Input := make([]uints.U8, len(input))
	for i, v := range input {
		u8Input[i] = bytesAPI.ValueOf(v)
	}

	// Compute Keccak256 hash using gnark's SHA3 package
	// Keccak256 is the same as SHA3-256 before the final padding change
	hasher, err := sha3.NewLegacyKeccak256(t.api)
	if err != nil {
		// In circuit, we can't return errors, so this should never happen
		// if the circuit is properly constructed
		panic("Failed to create Keccak256 hasher")
	}

	hasher.Write(u8Input)
	hashU8 := hasher.Sum()

	// Convert back to Variables
	hashBytes := make([]frontend.Variable, 32)
	for i := 0; i < 32; i++ {
		hashBytes[i] = bytesAPI.ValueUnchecked(hashU8[i])
	}

	return hashBytes
}

// Helper function to create a transcript and absorb initial state
func NewTranscriptWithInitialState(api frontend.API, initialState frontend.Variable) *Transcript {
	t := NewTranscript(api)
	if initialState != nil {
		t.CommonScalar(initialState)
	}
	return t
}

// TranscriptChallenge represents a challenge derived from the transcript.
// This is used to make the code more readable.
type TranscriptChallenge struct {
	Value frontend.Variable
}

// DeriveChallenges derives all PLONK challenges from the transcript.
//
// This function reconstructs the exact sequence of transcript operations
// that snark-verifier performs during proof verification.
//
// The sequence is:
// 1. Absorb public inputs
// 2. Absorb witness commitments (phase 0) → squeeze β, γ
// 3. Absorb witness commitments (phase 1) → squeeze α
// 4. Absorb witness commitments (phase 2) → squeeze challenges for phase 2
// 5. Absorb witness commitments (phase 3) → squeeze challenges for phase 3
// 6. Absorb quotient commitments → squeeze ζ (evaluation point)
// 7. Absorb evaluations → squeeze μ, γ_kzg (SHPLONK challenges)
// 8. Squeeze z' (SHPLONK evaluation point)
//
// Returns all challenges needed for PLONK verification.
type PLONKChallenges struct {
	Beta     frontend.Variable // Permutation challenge
	Gamma    frontend.Variable // Permutation challenge
	Alpha    frontend.Variable // Gate constraint challenge
	Zeta     frontend.Variable // Evaluation point
	Mu       frontend.Variable // SHPLONK batching challenge
	GammaKzg frontend.Variable // SHPLONK batching challenge
	ZPrime   frontend.Variable // SHPLONK evaluation point
}

// DerivePLONKChallenges derives all PLONK challenges from proof components.
//
// This must exactly match the sequence in snark-verifier's PlonkProof::read().
func DerivePLONKChallenges(
	api frontend.API,
	publicInputs []frontend.Variable,
	witnessCommitments [][]G1Point, // Grouped by phase
	quotientCommitments []G1Point,
	evaluations []FieldElement,
) *PLONKChallenges {
	t := NewTranscript(api)

	// 1. Absorb public inputs
	for _, input := range publicInputs {
		t.CommonScalar(input)
	}

	// 2. Absorb phase 0 witness commitments
	for _, point := range witnessCommitments[0] {
		t.CommonEcPoint(point.X, point.Y)
	}
	beta := t.SqueezeChallenge()
	gamma := t.SqueezeChallenge()

	// 3. Absorb phase 1 witness commitments
	if len(witnessCommitments) > 1 {
		for _, point := range witnessCommitments[1] {
			t.CommonEcPoint(point.X, point.Y)
		}
	}
	alpha := t.SqueezeChallenge()

	// 4. Absorb phase 2 witness commitments (if any)
	if len(witnessCommitments) > 2 {
		for _, point := range witnessCommitments[2] {
			t.CommonEcPoint(point.X, point.Y)
		}
		// Squeeze challenges for phase 2 (we don't use them directly)
		t.SqueezeChallenge()
		t.SqueezeChallenge()
	}

	// 5. Absorb phase 3 witness commitments (if any)
	if len(witnessCommitments) > 3 {
		for _, point := range witnessCommitments[3] {
			t.CommonEcPoint(point.X, point.Y)
		}
		// Squeeze challenge for phase 3
		t.SqueezeChallenge()
	}

	// 6. Absorb quotient commitments
	for _, point := range quotientCommitments {
		t.CommonEcPoint(point.X, point.Y)
	}
	zeta := t.SqueezeChallenge()

	// 7. Absorb evaluations
	for _, eval := range evaluations {
		// Convert FieldElement to Variable
		evalVar := bytesToVariable(api, eval[:])
		t.CommonScalar(evalVar)
	}

	// 8. Squeeze SHPLONK challenges
	mu := t.SqueezeChallenge()
	gammaKzg := t.SqueezeChallenge()

	// Note: W is absorbed here in the actual proof reading
	// but we don't have it yet in this function

	zPrime := t.SqueezeChallenge()

	return &PLONKChallenges{
		Beta:     beta,
		Gamma:    gamma,
		Alpha:    alpha,
		Zeta:     zeta,
		Mu:       mu,
		GammaKzg: gammaKzg,
		ZPrime:   zPrime,
	}
}

// Helper function to convert bytes to Variable
func bytesToVariable(api frontend.API, bytes []byte) frontend.Variable {
	// Convert bytes to bits
	bits := make([]frontend.Variable, len(bytes)*8)
	for i, b := range bytes {
		for j := 0; j < 8; j++ {
			bits[i*8+j] = (b >> (7 - j)) & 1
		}
	}
	return api.FromBinary(bits...)
}
