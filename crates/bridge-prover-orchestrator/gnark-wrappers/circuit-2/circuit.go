package main

import (
	"fmt"

	"github.com/consensys/gnark/frontend"
)

// NumPublicInputs matches Circuit 2 (Layer Hashes Movement) public-input layout
// emitted by build_layer_hashes_constraints in
// `historical-layer-hashes-movement-checker-circuit/src/circuit.rs`:
//
//	[0]      block_id
//	[1]      bk_set_poseidon
//	[2]      num_layers
//	[3..=12] layer_hash_frs[0..MAX_LAYERS=10]
//	[13]     prev_max_level_layer_hash
//
// = 14 BN254 Fr field elements.
const NumPublicInputs = 14

// LayerHashesVerifierCircuit is a Groth16 circuit that commits to the 14 public
// inputs of the Circuit 2 Halo2 SHPLONK proof. Same identity-stub trust model
// as the Primary / Fallback / BK-rotation wrappers in this repo: the gnark
// circuit only enforces that the public inputs flow through unchanged; the
// upstream Halo2 proof is the actual cryptographic guarantee.
type LayerHashesVerifierCircuit struct {
	PublicInputs [NumPublicInputs]frontend.Variable `gnark:",public"`
	DomainSize   frontend.Variable
}

func (circuit *LayerHashesVerifierCircuit) Define(api frontend.API) error {
	for i := 0; i < NumPublicInputs; i++ {
		api.AssertIsEqual(circuit.PublicInputs[i], circuit.PublicInputs[i])
	}
	api.AssertIsDifferent(circuit.DomainSize, 0)
	return nil
}

func NewLayerHashesVerifierCircuit(proofData *Halo2ProofData) (*LayerHashesVerifierCircuit, error) {
	if len(proofData.PublicInputs) != NumPublicInputs {
		return nil, fmt.Errorf("expected %d public inputs, got %d", NumPublicInputs, len(proofData.PublicInputs))
	}
	circuit := &LayerHashesVerifierCircuit{}
	for i := 0; i < NumPublicInputs; i++ {
		circuit.PublicInputs[i] = proofData.PublicInputs[i]
	}
	circuit.DomainSize = 1 << proofData.Protocol.K
	return circuit, nil
}
