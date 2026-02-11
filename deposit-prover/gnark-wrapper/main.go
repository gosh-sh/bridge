package main

import (
	"encoding/hex"
	"fmt"
	"log"
	"math/big"
	"os"
	"time"

	"github.com/consensys/gnark-crypto/ecc"
	"github.com/consensys/gnark/backend/groth16"
	groth16bn254 "github.com/consensys/gnark/backend/groth16/bn254"
	"github.com/consensys/gnark/frontend"
	"github.com/consensys/gnark/frontend/cs/r1cs"
)

func main() {
	if len(os.Args) < 2 {
		printUsage()
		os.Exit(1)
	}

	switch os.Args[1] {
	case "setup":
		runSetup()
	case "prove":
		proofFile := "halo2_proof.json"
		if len(os.Args) >= 3 {
			proofFile = os.Args[2]
		}
		runProve(proofFile)
	default:
		printUsage()
		os.Exit(1)
	}
}

func printUsage() {
	fmt.Println("Usage: gnark-wrapper <command> [args]")
	fmt.Println()
	fmt.Println("Commands:")
	fmt.Println("  setup                  Compile circuit, generate keys, export Solidity verifier")
	fmt.Println("  prove [proof.json]     Generate Groth16 proof (default: halo2_proof.json)")
}

// runSetup compiles the circuit, generates proving/verification keys,
// saves them to disk, and exports the Solidity verifier.
func runSetup() {
	fmt.Println("========================================")
	fmt.Println("Groth16 Setup")
	fmt.Println("========================================")
	fmt.Println()

	// Load a sample proof to determine circuit shape
	fmt.Println("Loading sample Halo2 proof for circuit shape...")
	proofData, err := LoadHalo2Proof("halo2_proof.json")
	if err != nil {
		log.Fatalf("Failed to load proof: %v", err)
	}
	fmt.Printf("✓ Loaded proof with %d public inputs\n", len(proofData.PublicInputs))
	fmt.Println()

	// Create circuit
	fmt.Println("Creating circuit...")
	circuit, err := NewHalo2VerifierCircuit(proofData)
	if err != nil {
		log.Fatalf("Failed to create circuit: %v", err)
	}

	// Compile to R1CS
	fmt.Println("Compiling circuit to R1CS...")
	startCompile := time.Now()
	ccs, err := frontend.Compile(ecc.BN254.ScalarField(), r1cs.NewBuilder, circuit)
	if err != nil {
		log.Fatalf("Failed to compile circuit: %v", err)
	}
	fmt.Printf("✓ Compiled in %v (%d constraints)\n", time.Since(startCompile), ccs.GetNbConstraints())
	fmt.Println()

	// Generate keys
	fmt.Println("Generating Groth16 keys...")
	startSetup := time.Now()
	pk, vk, err := groth16.Setup(ccs)
	if err != nil {
		log.Fatalf("Failed to setup: %v", err)
	}
	fmt.Printf("✓ Keys generated in %v\n", time.Since(startSetup))
	fmt.Println()

	// Save R1CS
	fmt.Println("Saving R1CS...")
	ccsFile, err := os.Create("circuit.r1cs")
	if err != nil {
		log.Fatalf("Failed to create R1CS file: %v", err)
	}
	_, err = ccs.WriteTo(ccsFile)
	ccsFile.Close()
	if err != nil {
		log.Fatalf("Failed to write R1CS: %v", err)
	}
	fmt.Println("✓ Saved to circuit.r1cs")

	// Save proving key
	fmt.Println("Saving proving key...")
	pkFile, err := os.Create("proving.key")
	if err != nil {
		log.Fatalf("Failed to create pk file: %v", err)
	}
	_, err = pk.WriteTo(pkFile)
	pkFile.Close()
	if err != nil {
		log.Fatalf("Failed to write pk: %v", err)
	}
	fmt.Println("✓ Saved to proving.key")

	// Save verification key
	fmt.Println("Saving verification key...")
	vkFile, err := os.Create("verification.key")
	if err != nil {
		log.Fatalf("Failed to create vk file: %v", err)
	}
	_, err = vk.WriteTo(vkFile)
	vkFile.Close()
	if err != nil {
		log.Fatalf("Failed to write vk: %v", err)
	}
	fmt.Println("✓ Saved to verification.key")
	fmt.Println()

	// Export Solidity verifier
	fmt.Println("Exporting Solidity verifier...")
	solidityFile, err := os.Create("Groth16Verifier.sol")
	if err != nil {
		log.Fatalf("Failed to create Solidity file: %v", err)
	}
	err = vk.ExportSolidity(solidityFile)
	solidityFile.Close()
	if err != nil {
		log.Fatalf("Failed to export Solidity verifier: %v", err)
	}
	fmt.Println("✓ Exported to Groth16Verifier.sol")

	fmt.Println()
	fmt.Println("Setup complete! Files created:")
	fmt.Println("  circuit.r1cs      - Compiled circuit")
	fmt.Println("  proving.key       - Proving key")
	fmt.Println("  verification.key  - Verification key")
	fmt.Println("  Groth16Verifier.sol - Solidity verifier")
}

// runProve loads saved keys, generates a Groth16 proof, and exports
// the proof bytes in the format expected by the Solidity verifier.
func runProve(proofFile string) {
	fmt.Println("========================================")
	fmt.Println("Groth16 Prove")
	fmt.Println("========================================")
	fmt.Println()

	// Load Halo2 proof data
	fmt.Printf("Loading Halo2 proof from %s...\n", proofFile)
	proofData, err := LoadHalo2Proof(proofFile)
	if err != nil {
		log.Fatalf("Failed to load proof: %v", err)
	}
	fmt.Printf("✓ Loaded proof with %d public inputs\n", len(proofData.PublicInputs))
	fmt.Println()

	// Load R1CS
	fmt.Println("Loading R1CS...")
	ccs := groth16.NewCS(ecc.BN254)
	ccsFile, err := os.Open("circuit.r1cs")
	if err != nil {
		log.Fatalf("Failed to open R1CS file (run 'setup' first): %v", err)
	}
	_, err = ccs.ReadFrom(ccsFile)
	ccsFile.Close()
	if err != nil {
		log.Fatalf("Failed to read R1CS: %v", err)
	}
	fmt.Println("✓ R1CS loaded")

	// Load proving key
	fmt.Println("Loading proving key...")
	pk := groth16.NewProvingKey(ecc.BN254)
	pkFile, err := os.Open("proving.key")
	if err != nil {
		log.Fatalf("Failed to open proving key (run 'setup' first): %v", err)
	}
	_, err = pk.ReadFrom(pkFile)
	pkFile.Close()
	if err != nil {
		log.Fatalf("Failed to read proving key: %v", err)
	}
	fmt.Println("✓ Proving key loaded")

	// Load verification key
	fmt.Println("Loading verification key...")
	vk := groth16.NewVerifyingKey(ecc.BN254)
	vkFile, err := os.Open("verification.key")
	if err != nil {
		log.Fatalf("Failed to open verification key (run 'setup' first): %v", err)
	}
	_, err = vk.ReadFrom(vkFile)
	vkFile.Close()
	if err != nil {
		log.Fatalf("Failed to read verification key: %v", err)
	}
	fmt.Println("✓ Verification key loaded")
	fmt.Println()

	// Create witness
	fmt.Println("Creating witness...")
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

	// Generate proof
	fmt.Println("Generating Groth16 proof...")
	startProve := time.Now()
	proof, err := groth16.Prove(ccs, pk, witness)
	if err != nil {
		log.Fatalf("Failed to prove: %v", err)
	}
	fmt.Printf("✓ Proof generated in %v\n", time.Since(startProve))
	fmt.Println()

	// Verify proof
	fmt.Println("Verifying proof...")
	publicWitness, err := witness.Public()
	if err != nil {
		log.Fatalf("Failed to get public witness: %v", err)
	}
	err = groth16.Verify(proof, vk, publicWitness)
	if err != nil {
		log.Fatalf("Verification failed: %v", err)
	}
	fmt.Println("✓ Proof verified")
	fmt.Println()

	// Export proof bytes for Solidity
	fmt.Println("Exporting proof bytes for Solidity...")
	bn254Proof, ok := proof.(*groth16bn254.Proof)
	if !ok {
		log.Fatalf("Failed to cast proof to bn254.Proof")
	}

	// MarshalSolidity returns the proof as raw bytes in EIP-197 format:
	// [Ar.X, Ar.Y, Bs.X.A1, Bs.X.A0, Bs.Y.A1, Bs.Y.A0, Krs.X, Krs.Y]
	// = 8 * 32 = 256 bytes
	proofBytes := bn254Proof.MarshalSolidity()
	fmt.Printf("  Groth16 proof: %d bytes\n", len(proofBytes))

	// Get promise_commit (7th public input, index 6)
	promiseCommitStr := proofData.PublicInputs[6]
	promiseCommit := new(big.Int)
	promiseCommit.SetString(promiseCommitStr, 10)
	promiseCommitBytes := make([]byte, 32)
	promiseCommit.FillBytes(promiseCommitBytes)

	// Concatenate: proof (256 bytes) + promise_commit (32 bytes) = 288 bytes
	fullProofBytes := append(proofBytes, promiseCommitBytes...)
	fmt.Printf("  Full proof (with promise_commit): %d bytes\n", len(fullProofBytes))

	// Write as 0x-prefixed hex
	hexStr := "0x" + hex.EncodeToString(fullProofBytes)
	err = os.WriteFile("groth16_proof_bytes.hex", []byte(hexStr), 0644)
	if err != nil {
		log.Fatalf("Failed to write proof bytes: %v", err)
	}
	fmt.Println("✓ Proof bytes saved to groth16_proof_bytes.hex")
	fmt.Println()

	fmt.Println("Done! Use groth16_proof_bytes.hex for on-chain withdrawal.")
}
