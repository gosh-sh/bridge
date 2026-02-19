package main

import (
	"encoding/json"
	"os"
)

// Halo2ProofData represents the parsed Halo2 proof data from Rust.
// This struct matches the Rust Halo2ProofData struct.
type Halo2ProofData struct {
	PublicInputs []string      `json:"public_inputs"`
	ProofBytes   []byte        `json:"proof_bytes"`
	Protocol     ProtocolData  `json:"protocol"`
}

// ProtocolData represents the protocol information (verification key data).
type ProtocolData struct {
	K                        uint32   `json:"k"`
	NumInstance              []uint   `json:"num_instance"`
	NumWitness               []uint   `json:"num_witness"`
	NumChallenge             []uint   `json:"num_challenge"`
	PreprocessedCommitments  []string `json:"preprocessed_commitments"`
}

// LoadHalo2Proof loads a Halo2 proof from a JSON file.
func LoadHalo2Proof(filename string) (*Halo2ProofData, error) {
	data, err := os.ReadFile(filename)
	if err != nil {
		return nil, err
	}

	var proof Halo2ProofData
	err = json.Unmarshal(data, &proof)
	if err != nil {
		return nil, err
	}

	return &proof, nil
}

