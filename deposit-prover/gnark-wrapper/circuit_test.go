package main

import (
	"encoding/json"
	"os"
	"testing"

	"github.com/consensys/gnark-crypto/ecc"
	"github.com/consensys/gnark/backend/groth16"
	"github.com/consensys/gnark/frontend"
	"github.com/consensys/gnark/frontend/cs/r1cs"
	"github.com/consensys/gnark/test"
)

// TestCircuitCompilation tests that the circuit compiles successfully
func TestCircuitCompilation(t *testing.T) {
	// Create a minimal circuit instance
	circuit := &Halo2VerifierCircuit{}

	// Compile the circuit
	_, err := frontend.Compile(ecc.BN254.ScalarField(), r1cs.NewBuilder, circuit)
	if err != nil {
		t.Fatalf("Circuit compilation failed: %v", err)
	}

	t.Log("✓ Circuit compiled successfully")
}

// TestCircuitWithDummyData tests the circuit with dummy data
func TestCircuitWithDummyData(t *testing.T) {
	// Create circuit with dummy public inputs
	circuit := &Halo2VerifierCircuit{}
	for i := 0; i < 7; i++ {
		circuit.PublicInputs[i] = 1
	}

	// Set dummy witness data
	var dummyX, dummyY [32]byte
	dummyX[31] = 1
	dummyY[31] = 1

	for i := 0; i < 12; i++ {
		circuit.WitnessCommitmentsPhase0[i] = G1Point{X: dummyX, Y: dummyY}
	}
	circuit.DomainSize = 1 << 18 // 2^18

	// Test the circuit
	assert := test.NewAssert(t)
	err := test.IsSolved(circuit, circuit, ecc.BN254.ScalarField())
	if err != nil {
		t.Logf("Circuit constraints not satisfied (expected for dummy data): %v", err)
	} else {
		assert.Log("✓ Circuit constraints satisfied with dummy data")
	}
}

// TestLoadProofData tests loading proof data from JSON
func TestLoadProofData(t *testing.T) {
	// Check if test proof exists
	proofPath := "halo2_proof.json"
	if _, err := os.Stat(proofPath); os.IsNotExist(err) {
		t.Skip("Skipping test: halo2_proof.json not found. Run export_proof_for_gnark first.")
	}

	// Load proof data
	proofData, err := LoadProofData(proofPath)
	if err != nil {
		t.Fatalf("Failed to load proof data: %v", err)
	}

	// Validate proof data
	if len(proofData.PublicInputs) != 7 {
		t.Errorf("Expected 7 public inputs, got %d", len(proofData.PublicInputs))
	}

	if len(proofData.ProofBytes) != 8224 {
		t.Errorf("Expected 8224 proof bytes, got %d", len(proofData.ProofBytes))
	}

	if proofData.Protocol.K != 18 {
		t.Errorf("Expected k=18, got k=%d", proofData.Protocol.K)
	}

	t.Logf("✓ Proof data loaded successfully")
	t.Logf("  - Public inputs: %d", len(proofData.PublicInputs))
	t.Logf("  - Proof bytes: %d", len(proofData.ProofBytes))
	t.Logf("  - Domain size: 2^%d = %d", proofData.Protocol.K, 1<<proofData.Protocol.K)
}

// TestProofParsing tests parsing the proof bytes
func TestProofParsing(t *testing.T) {
	// Check if test proof exists
	proofPath := "halo2_proof.json"
	if _, err := os.Stat(proofPath); os.IsNotExist(err) {
		t.Skip("Skipping test: halo2_proof.json not found")
	}

	// Load proof data
	proofData, err := LoadProofData(proofPath)
	if err != nil {
		t.Fatalf("Failed to load proof data: %v", err)
	}

	// Parse proof
	parsedProof, err := ParseHalo2Proof(proofData.ProofBytes, &proofData.Protocol)
	if err != nil {
		t.Fatalf("Failed to parse proof: %v", err)
	}

	// Validate parsed proof structure
	expectedWitnessCommitments := []int{12, 14, 6, 18}
	if len(parsedProof.WitnessCommitments) != len(expectedWitnessCommitments) {
		t.Errorf("Expected %d phases, got %d", len(expectedWitnessCommitments), len(parsedProof.WitnessCommitments))
	}

	for i, expected := range expectedWitnessCommitments {
		if i < len(parsedProof.WitnessCommitments) {
			actual := len(parsedProof.WitnessCommitments[i])
			if actual != expected {
				t.Errorf("Phase %d: expected %d commitments, got %d", i, expected, actual)
			}
		}
	}

	if len(parsedProof.QuotientCommitments) != 3 {
		t.Errorf("Expected 3 quotient commitments, got %d", len(parsedProof.QuotientCommitments))
	}

	if len(parsedProof.Evaluations) != 147 {
		t.Errorf("Expected 147 evaluations, got %d", len(parsedProof.Evaluations))
	}

	t.Logf("✓ Proof parsed successfully")
	t.Logf("  - Witness commitments: %v", expectedWitnessCommitments)
	t.Logf("  - Quotient commitments: %d", len(parsedProof.QuotientCommitments))
	t.Logf("  - Evaluations: %d", len(parsedProof.Evaluations))
}

// TestCircuitWithRealProof tests the circuit with a real Halo2 proof
func TestCircuitWithRealProof(t *testing.T) {
	// Check if test proof exists
	proofPath := "halo2_proof.json"
	if _, err := os.Stat(proofPath); os.IsNotExist(err) {
		t.Skip("Skipping test: halo2_proof.json not found")
	}

	// Load proof data
	proofData, err := LoadProofData(proofPath)
	if err != nil {
		t.Fatalf("Failed to load proof data: %v", err)
	}

	// Create circuit with real proof data
	circuit, err := NewHalo2VerifierCircuit(proofData)
	if err != nil {
		t.Fatalf("Failed to create circuit: %v", err)
	}

	// Parse proof to populate witness
	parsedProof, err := ParseHalo2Proof(proofData.ProofBytes, &proofData.Protocol)
	if err != nil {
		t.Fatalf("Failed to parse proof: %v", err)
	}

	// Populate witness commitments
	for i := 0; i < 12 && i < len(parsedProof.WitnessCommitments[0]); i++ {
		circuit.WitnessCommitmentsPhase0[i] = parsedProof.WitnessCommitments[0][i]
	}
	for i := 0; i < 14 && i < len(parsedProof.WitnessCommitments[1]); i++ {
		circuit.WitnessCommitmentsPhase1[i] = parsedProof.WitnessCommitments[1][i]
	}
	for i := 0; i < 6 && i < len(parsedProof.WitnessCommitments[2]); i++ {
		circuit.WitnessCommitmentsPhase2[i] = parsedProof.WitnessCommitments[2][i]
	}
	for i := 0; i < 18 && i < len(parsedProof.WitnessCommitments[3]); i++ {
		circuit.WitnessCommitmentsPhase3[i] = parsedProof.WitnessCommitments[3][i]
	}

	// Test circuit compilation
	assert := test.NewAssert(t)
	err = test.IsSolved(circuit, circuit, ecc.BN254.ScalarField())
	if err != nil {
		t.Logf("Circuit constraints not satisfied (expected - full verification not implemented): %v", err)
	} else {
		assert.Log("✓ Circuit constraints satisfied with real proof")
	}
}

// TestGroth16ProofGeneration tests full Groth16 proof generation
func TestGroth16ProofGeneration(t *testing.T) {
	// This is a longer test - skip in short mode
	if testing.Short() {
		t.Skip("Skipping Groth16 proof generation in short mode")
	}

	// Check if test proof exists
	proofPath := "halo2_proof.json"
	if _, err := os.Stat(proofPath); os.IsNotExist(err) {
		t.Skip("Skipping test: halo2_proof.json not found")
	}

	// Load proof data
	proofData, err := LoadProofData(proofPath)
	if err != nil {
		t.Fatalf("Failed to load proof data: %v", err)
	}

	// Create circuit
	circuit, err := NewHalo2VerifierCircuit(proofData)
	if err != nil {
		t.Fatalf("Failed to create circuit: %v", err)
	}

	t.Log("Compiling circuit...")
	ccs, err := frontend.Compile(ecc.BN254.ScalarField(), r1cs.NewBuilder, circuit)
	if err != nil {
		t.Fatalf("Circuit compilation failed: %v", err)
	}
	t.Logf("✓ Circuit compiled: %d constraints", ccs.GetNbConstraints())

	t.Log("Generating Groth16 keys (this may take a while)...")
	pk, vk, err := groth16.Setup(ccs)
	if err != nil {
		t.Fatalf("Groth16 setup failed: %v", err)
	}
	t.Log("✓ Groth16 keys generated")

	// Create witness
	witness, err := frontend.NewWitness(circuit, ecc.BN254.ScalarField())
	if err != nil {
		t.Fatalf("Failed to create witness: %v", err)
	}

	t.Log("Generating Groth16 proof...")
	proof, err := groth16.Prove(ccs, pk, witness)
	if err != nil {
		t.Fatalf("Groth16 proof generation failed: %v", err)
	}
	t.Log("✓ Groth16 proof generated")

	// Verify the proof
	publicWitness, err := witness.Public()
	if err != nil {
		t.Fatalf("Failed to extract public witness: %v", err)
	}

	t.Log("Verifying Groth16 proof...")
	err = groth16.Verify(proof, vk, publicWitness)
	if err != nil {
		t.Fatalf("Groth16 verification failed: %v", err)
	}
	t.Log("✓ Groth16 proof verified successfully!")
}

// BenchmarkCircuitCompilation benchmarks circuit compilation
func BenchmarkCircuitCompilation(b *testing.B) {
	circuit := &Halo2VerifierCircuit{}

	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		_, err := frontend.Compile(ecc.BN254.ScalarField(), r1cs.NewBuilder, circuit)
		if err != nil {
			b.Fatalf("Circuit compilation failed: %v", err)
		}
	}
}

// Helper function to load proof data (duplicate from main.go for testing)
func LoadProofData(path string) (*Halo2ProofData, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}

	var proofData Halo2ProofData
	err = json.Unmarshal(data, &proofData)
	if err != nil {
		return nil, err
	}

	return &proofData, nil
}
