package main

import (
	"encoding/hex"
	"encoding/json"
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
		proofFile := "halo2_proof.json"
		if len(os.Args) >= 3 {
			proofFile = os.Args[2]
		}
		runSetup(proofFile)
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
	fmt.Println("Usage: gnark-wrapper <command> [proof.json]")
	fmt.Println()
	fmt.Println("Commands:")
	fmt.Println("  setup [proof.json]  Compile circuit, generate keys, export Solidity verifier")
	fmt.Println("  prove [proof.json]  Generate Groth16 proof (default: halo2_proof.json)")
}

func runSetup(proofFile string) {
	fmt.Println("=== Layer Hash Groth16 Setup ===")

	proofData, err := LoadHalo2Proof(proofFile)
	if err != nil {
		log.Fatalf("Failed to load proof: %v", err)
	}
	fmt.Printf("Loaded proof with %d public inputs, k=%d\n", len(proofData.PublicInputs), proofData.Protocol.K)

	circuit, err := NewLayerHashVerifierCircuit(proofData)
	if err != nil {
		log.Fatalf("Failed to create circuit: %v", err)
	}

	fmt.Println("Compiling circuit to R1CS...")
	t := time.Now()
	ccs, err := frontend.Compile(ecc.BN254.ScalarField(), r1cs.NewBuilder, circuit)
	if err != nil {
		log.Fatalf("Failed to compile circuit: %v", err)
	}
	fmt.Printf("Compiled in %v (%d constraints)\n", time.Since(t), ccs.GetNbConstraints())

	fmt.Println("Generating Groth16 keys...")
	t = time.Now()
	pk, vk, err := groth16.Setup(ccs)
	if err != nil {
		log.Fatalf("Failed to setup: %v", err)
	}
	fmt.Printf("Keys generated in %v\n", time.Since(t))

	saveFile("circuit.r1cs", func(f *os.File) error { _, e := ccs.WriteTo(f); return e })
	saveFile("proving.key", func(f *os.File) error { _, e := pk.WriteTo(f); return e })
	saveFile("verification.key", func(f *os.File) error { _, e := vk.WriteTo(f); return e })
	saveFile("Groth16Verifier.sol", func(f *os.File) error { return vk.ExportSolidity(f) })

	fmt.Println("Setup complete!")
}

func runProve(proofFile string) {
	fmt.Println("=== Layer Hash Groth16 Prove ===")

	proofData, err := LoadHalo2Proof(proofFile)
	if err != nil {
		log.Fatalf("Failed to load proof: %v", err)
	}
	fmt.Printf("Loaded proof with %d public inputs\n", len(proofData.PublicInputs))

	ccs := groth16.NewCS(ecc.BN254)
	loadFile("circuit.r1cs", func(f *os.File) error { _, e := ccs.ReadFrom(f); return e })

	pk := groth16.NewProvingKey(ecc.BN254)
	loadFile("proving.key", func(f *os.File) error { _, e := pk.ReadFrom(f); return e })

	vk := groth16.NewVerifyingKey(ecc.BN254)
	loadFile("verification.key", func(f *os.File) error { _, e := vk.ReadFrom(f); return e })

	witnessCircuit, err := NewLayerHashVerifierCircuit(proofData)
	if err != nil {
		log.Fatalf("Failed to create witness: %v", err)
	}
	witness, err := frontend.NewWitness(witnessCircuit, ecc.BN254.ScalarField())
	if err != nil {
		log.Fatalf("Failed to create witness: %v", err)
	}

	fmt.Println("Generating Groth16 proof...")
	t := time.Now()
	proof, err := groth16.Prove(ccs, pk, witness)
	if err != nil {
		log.Fatalf("Failed to prove: %v", err)
	}
	fmt.Printf("Proof generated in %v\n", time.Since(t))

	publicWitness, err := witness.Public()
	if err != nil {
		log.Fatalf("Failed to get public witness: %v", err)
	}
	err = groth16.Verify(proof, vk, publicWitness)
	if err != nil {
		log.Fatalf("Verification failed: %v", err)
	}
	fmt.Println("Proof verified locally!")

	bn254Proof, ok := proof.(*groth16bn254.Proof)
	if !ok {
		log.Fatalf("Failed to cast proof to bn254.Proof")
	}

	groth16ProofBytes := bn254Proof.MarshalSolidity()
	fmt.Printf("Groth16 proof: %d bytes\n", len(groth16ProofBytes))

	// Export public inputs as 32-byte big-endian values for Solidity
	var publicInputBytes []byte
	for _, piStr := range proofData.PublicInputs {
		val := new(big.Int)
		val.SetString(piStr, 10)
		b := make([]byte, 32)
		val.FillBytes(b)
		publicInputBytes = append(publicInputBytes, b...)
	}

	// Write outputs
	proofHex := "0x" + hex.EncodeToString(groth16ProofBytes)
	os.WriteFile("groth16_proof.hex", []byte(proofHex), 0644)

	inputsHex := "0x" + hex.EncodeToString(publicInputBytes)
	os.WriteFile("groth16_public_inputs.hex", []byte(inputsHex), 0644)

	// Also write a combined JSON for convenience
	combined := map[string]interface{}{
		"proof":         proofHex,
		"public_inputs": proofData.PublicInputs,
	}
	combinedJSON, _ := json.MarshalIndent(combined, "", "  ")
	os.WriteFile("groth16_output.json", combinedJSON, 0644)

	fmt.Println("Output files:")
	fmt.Println("  groth16_proof.hex          - 256-byte Groth16 proof (0x-prefixed)")
	fmt.Println("  groth16_public_inputs.hex  - 13 × 32-byte public inputs (0x-prefixed)")
	fmt.Println("  groth16_output.json        - Combined proof + public inputs")
}

func saveFile(name string, write func(*os.File) error) {
	f, err := os.Create(name)
	if err != nil {
		log.Fatalf("Failed to create %s: %v", name, err)
	}
	defer f.Close()
	if err := write(f); err != nil {
		log.Fatalf("Failed to write %s: %v", name, err)
	}
	fmt.Printf("  Saved %s\n", name)
}

func loadFile(name string, read func(*os.File) error) {
	f, err := os.Open(name)
	if err != nil {
		log.Fatalf("Failed to open %s (run 'setup' first): %v", name, err)
	}
	defer f.Close()
	if err := read(f); err != nil {
		log.Fatalf("Failed to read %s: %v", name, err)
	}
	fmt.Printf("  Loaded %s\n", name)
}
