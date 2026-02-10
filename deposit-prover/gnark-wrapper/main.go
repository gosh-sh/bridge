package main

import (
	"fmt"
	"log"
	"os"
	"time"

	"github.com/consensys/gnark-crypto/ecc"
	"github.com/consensys/gnark/backend/groth16"
	"github.com/consensys/gnark/frontend"
	"github.com/consensys/gnark/frontend/cs/r1cs"
)

func main() {
	fmt.Println("========================================")
	fmt.Println("Groth16 Wrapper for Halo2 Proofs")
	fmt.Println("========================================")
	fmt.Println()

	// Step 1: Load Halo2 proof data
	fmt.Println("Step 1: Loading Halo2 proof data...")
	proofData, err := LoadHalo2Proof("halo2_proof.json")
	if err != nil {
		log.Fatalf("Failed to load proof: %v", err)
	}
	fmt.Printf("✓ Loaded proof with %d public inputs\n", len(proofData.PublicInputs))
	fmt.Printf("  - Domain k: %d\n", proofData.Protocol.K)
	fmt.Printf("  - Proof size: %d bytes\n", len(proofData.ProofBytes))
	fmt.Println()

	// Step 2: Create circuit
	fmt.Println("Step 2: Creating Groth16 verifier circuit...")
	circuit, err := NewHalo2VerifierCircuit(proofData)
	if err != nil {
		log.Fatalf("Failed to create circuit: %v", err)
	}
	fmt.Println("✓ Circuit created")
	fmt.Println()

	// Step 3: Compile circuit to R1CS
	fmt.Println("Step 3: Compiling circuit to R1CS...")
	startCompile := time.Now()
	ccs, err := frontend.Compile(ecc.BN254.ScalarField(), r1cs.NewBuilder, circuit)
	if err != nil {
		log.Fatalf("Failed to compile circuit: %v", err)
	}
	compileTime := time.Since(startCompile)
	fmt.Printf("✓ Circuit compiled in %v\n", compileTime)
	fmt.Printf("  - Constraints: %d\n", ccs.GetNbConstraints())
	fmt.Println()

	// Step 4: Generate Groth16 proving and verification keys
	fmt.Println("Step 4: Generating Groth16 keys (this may take a while)...")
	startSetup := time.Now()
	pk, vk, err := groth16.Setup(ccs)
	if err != nil {
		log.Fatalf("Failed to setup: %v", err)
	}
	setupTime := time.Since(startSetup)
	fmt.Printf("✓ Keys generated in %v\n", setupTime)
	fmt.Println()

	// Step 5: Create witness (proof data)
	fmt.Println("Step 5: Creating witness...")
	witnessCircuit, err := NewHalo2VerifierCircuit(proofData)
	if err != nil {
		log.Fatalf("Failed to create witness circuit: %v", err)
	}

	witness, err := frontend.NewWitness(witnessCircuit, ecc.BN254.ScalarField())
	if err != nil {
		log.Fatalf("Failed to create witness: %v", err)
	}
	fmt.Println("✓ Witness created")
	fmt.Println()

	// Step 6: Generate Groth16 proof
	fmt.Println("Step 6: Generating Groth16 proof...")
	startProve := time.Now()
	proof, err := groth16.Prove(ccs, pk, witness)
	if err != nil {
		log.Fatalf("Failed to prove: %v", err)
	}
	proveTime := time.Since(startProve)
	fmt.Printf("✓ Groth16 proof generated in %v\n", proveTime)
	fmt.Println()

	// Step 7: Verify Groth16 proof
	fmt.Println("Step 7: Verifying Groth16 proof...")
	startVerify := time.Now()
	publicWitness, err := witness.Public()
	if err != nil {
		log.Fatalf("Failed to get public witness: %v", err)
	}
	err = groth16.Verify(proof, vk, publicWitness)
	verifyTime := time.Since(startVerify)
	if err != nil {
		log.Fatalf("Verification failed: %v", err)
	}
	fmt.Printf("✓ Proof verified in %v\n", verifyTime)
	fmt.Println()

	// Step 8: Export Solidity verifier
	fmt.Println("Step 8: Exporting Solidity verifier...")
	solidityFile, err := os.Create("Groth16Verifier.sol")
	if err != nil {
		log.Fatalf("Failed to create Solidity file: %v", err)
	}
	defer solidityFile.Close()

	err = vk.ExportSolidity(solidityFile)
	if err != nil {
		log.Fatalf("Failed to export Solidity verifier: %v", err)
	}
	fmt.Println("✓ Solidity verifier exported to Groth16Verifier.sol")
	fmt.Println()

	// Step 9: Check verifier size
	fileInfo, err := os.Stat("Groth16Verifier.sol")
	if err == nil {
		fmt.Printf("  - Verifier size: %d bytes\n", fileInfo.Size())
		if fileInfo.Size() < 24576 {
			fmt.Println("  ✓ Verifier is under 24KB limit!")
		} else {
			fmt.Println("  ✗ Warning: Verifier exceeds 24KB limit")
		}
	}
	fmt.Println()

	// Step 10: Export proof data for on-chain testing
	fmt.Println("Step 10: Exporting proof data for on-chain testing...")

	// Create test data file
	testFile, err := os.Create("groth16_test_data.txt")
	if err != nil {
		log.Fatalf("Failed to create test file: %v", err)
	}
	defer testFile.Close()

	// Write public inputs (these are what we need for on-chain testing)
	fmt.Fprintf(testFile, "PUBLIC_INPUTS=[")
	for i, input := range proofData.PublicInputs {
		if i > 0 {
			fmt.Fprintf(testFile, ",")
		}
		fmt.Fprintf(testFile, "%s", input)
	}
	fmt.Fprintf(testFile, "]\n\n")

	// Write note about proof format
	fmt.Fprintf(testFile, "# Note: Groth16 proof is generated and verified off-chain\n")
	fmt.Fprintf(testFile, "# For on-chain testing, use the Solidity verifier contract\n")
	fmt.Fprintf(testFile, "# Public inputs count: %d\n", len(proofData.PublicInputs))

	fmt.Println("✓ Test data exported to groth16_test_data.txt")
	fmt.Println()

	// Summary
	fmt.Println("========================================")
	fmt.Println("Summary")
	fmt.Println("========================================")
	fmt.Printf("Compile time:  %v\n", compileTime)
	fmt.Printf("Setup time:    %v\n", setupTime)
	fmt.Printf("Prove time:    %v\n", proveTime)
	fmt.Printf("Verify time:   %v\n", verifyTime)
	fmt.Printf("Total time:    %v\n", compileTime+setupTime+proveTime+verifyTime)
	fmt.Println()
	fmt.Println("Next steps:")
	fmt.Println("1. Deploy Groth16Verifier.sol to Ethereum mainnet")
	fmt.Println("2. Verify proofs on-chain using the tiny verifier")
	fmt.Println()
	fmt.Println("NOTE: This is a simplified demonstration.")
	fmt.Println("A full implementation requires implementing complete PLONK verification logic.")
	fmt.Println("See circuit.go for details on what needs to be implemented.")
}
