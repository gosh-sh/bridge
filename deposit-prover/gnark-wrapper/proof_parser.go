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

// ParsedProof contains the structured proof components for SHPLONK verification
type ParsedProof struct {
	// Witness commitments grouped by phase
	// For deposit circuit: [12, 14, 6, 18] = 50 total
	WitnessCommitments [][]G1Point

	// Quotient commitments (H polynomial chunks)
	// Typically 3 chunks for degree 18
	QuotientCommitments []G1Point

	// Polynomial evaluations at challenge point ζ
	Evaluations []FieldElement

	// SHPLONK multi-opening proof (W and W')
	W      G1Point
	WPrime G1Point
}

// ParseHalo2Proof parses the raw proof bytes into structured components
// based on the protocol data (verification key)
func ParseHalo2Proof(proofBytes []byte, protocol *ProtocolData) (*ParsedProof, error) {
	offset := 0
	parsed := &ParsedProof{}

	// Parse witness commitments (grouped by phase)
	parsed.WitnessCommitments = make([][]G1Point, len(protocol.NumWitness))
	for phase, numWitness := range protocol.NumWitness {
		parsed.WitnessCommitments[phase] = make([]G1Point, numWitness)
		for i := uint(0); i < numWitness; i++ {
			if offset+64 > len(proofBytes) {
				return nil, fmt.Errorf("insufficient bytes for witness commitment phase=%d index=%d", phase, i)
			}
			copy(parsed.WitnessCommitments[phase][i].X[:], proofBytes[offset:offset+32])
			copy(parsed.WitnessCommitments[phase][i].Y[:], proofBytes[offset+32:offset+64])
			offset += 64
		}
	}

	// Parse quotient commitments
	// For degree k=18, we have 3 chunks
	numQuotientChunks := 3
	parsed.QuotientCommitments = make([]G1Point, numQuotientChunks)
	for i := 0; i < numQuotientChunks; i++ {
		if offset+64 > len(proofBytes) {
			return nil, fmt.Errorf("insufficient bytes for quotient commitment %d", i)
		}
		copy(parsed.QuotientCommitments[i].X[:], proofBytes[offset:offset+32])
		copy(parsed.QuotientCommitments[i].Y[:], proofBytes[offset+32:offset+64])
		offset += 64
	}

	// Parse evaluations
	// Reserve 128 bytes for W and W' at the end
	remainingBytes := len(proofBytes) - offset - 128
	if remainingBytes < 0 || remainingBytes%32 != 0 {
		return nil, fmt.Errorf("invalid proof size: cannot parse evaluations")
	}
	numEvaluations := remainingBytes / 32
	parsed.Evaluations = make([]FieldElement, numEvaluations)
	for i := 0; i < numEvaluations; i++ {
		copy(parsed.Evaluations[i][:], proofBytes[offset:offset+32])
		offset += 32
	}

	// Parse SHPLONK opening proof (W, W')
	if offset+64 > len(proofBytes) {
		return nil, fmt.Errorf("insufficient bytes for W")
	}
	copy(parsed.W.X[:], proofBytes[offset:offset+32])
	copy(parsed.W.Y[:], proofBytes[offset+32:offset+64])
	offset += 64

	if offset+64 > len(proofBytes) {
		return nil, fmt.Errorf("insufficient bytes for W'")
	}
	copy(parsed.WPrime.X[:], proofBytes[offset:offset+32])
	copy(parsed.WPrime.Y[:], proofBytes[offset+32:offset+64])
	offset += 64

	if offset != len(proofBytes) {
		return nil, fmt.Errorf("proof size mismatch: parsed %d bytes, expected %d", offset, len(proofBytes))
	}

	return parsed, nil
}

// Helper function to convert bytes to uint64 (little-endian)
func bytesToUint64(b []byte) uint64 {
	return binary.LittleEndian.Uint64(b)
}

// Helper function to convert uint64 to bytes (little-endian)
func uint64ToBytes(v uint64) []byte {
	b := make([]byte, 8)
	binary.LittleEndian.PutUint64(b, v)
	return b
}

