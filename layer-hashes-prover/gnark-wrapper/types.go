package main

import (
	"encoding/json"
	"os"
)

type Halo2ProofData struct {
	PublicInputs []string     `json:"public_inputs"`
	ProofBytes   []byte       `json:"proof_bytes"`
	Protocol     ProtocolData `json:"protocol"`
}

type ProtocolData struct {
	K                       uint32   `json:"k"`
	NumInstance             []uint   `json:"num_instance"`
	NumWitness              []uint   `json:"num_witness"`
	NumChallenge            []uint   `json:"num_challenge"`
	PreprocessedCommitments []string `json:"preprocessed_commitments"`
}

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
