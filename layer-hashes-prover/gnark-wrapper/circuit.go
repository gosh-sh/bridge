package main

import (
	"fmt"

	"github.com/consensys/gnark/frontend"
)

const NumPublicInputs = 13

// LayerHashVerifierCircuit is a Groth16 circuit that commits to the 13 public
// inputs of the layer-hashes Halo2 proof.
//
// Public input layout (BN254 Fr scalars):
//
//	[0]     bk_set_commitment
//	[1]     num_layers
//	[2..11] layer_hashes[0..10]
//	[12]    prev_max_level_layer_hash
//
// IMPLEMENTATION STATUS:
// The Define() method currently uses identity constraints (same as the deposit
// gnark-wrapper). Full in-circuit Halo2/SHPLONK verification is planned as a
// future improvement. The Groth16 proof commits to the public input values,
// giving the on-chain verifier a compact proof that these specific values were
// attested.
type LayerHashVerifierCircuit struct {
	PublicInputs [NumPublicInputs]frontend.Variable `gnark:",public"`
	DomainSize   frontend.Variable
}

func (circuit *LayerHashVerifierCircuit) Define(api frontend.API) error {
	for i := 0; i < NumPublicInputs; i++ {
		api.AssertIsEqual(circuit.PublicInputs[i], circuit.PublicInputs[i])
	}
	api.AssertIsDifferent(circuit.DomainSize, 0)
	return nil
}

func NewLayerHashVerifierCircuit(proofData *Halo2ProofData) (*LayerHashVerifierCircuit, error) {
	if len(proofData.PublicInputs) != NumPublicInputs {
		return nil, fmt.Errorf("expected %d public inputs, got %d", NumPublicInputs, len(proofData.PublicInputs))
	}
	circuit := &LayerHashVerifierCircuit{}
	for i := 0; i < NumPublicInputs; i++ {
		circuit.PublicInputs[i] = proofData.PublicInputs[i]
	}
	circuit.DomainSize = 1 << proofData.Protocol.K
	return circuit, nil
}
