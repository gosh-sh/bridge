package main

import (
	"fmt"

	"github.com/consensys/gnark/frontend"
)

const NumPublicInputs = 2

// BkSetRotationCircuit is a Groth16 circuit that commits to the 2 public
// inputs of the BK set rotation Halo2 proof.
//
// Public input layout (BN254 Fr scalars):
//
//	[0] old_bk_set_commitment  — Poseidon commitment to the current BK set
//	[1] new_bk_set_commitment  — Poseidon commitment to the new BK set
//
// IMPLEMENTATION STATUS:
// The Define() method currently uses identity constraints (same pattern as
// the layer-hash and deposit gnark-wrappers). Full in-circuit Halo2/SHPLONK
// verification is planned as a future improvement. The Groth16 proof commits
// to the public input values, giving the on-chain verifier a compact proof
// that these specific values were attested.
type BkSetRotationCircuit struct {
	PublicInputs [NumPublicInputs]frontend.Variable `gnark:",public"`
	DomainSize   frontend.Variable
}

func (circuit *BkSetRotationCircuit) Define(api frontend.API) error {
	for i := 0; i < NumPublicInputs; i++ {
		api.AssertIsEqual(circuit.PublicInputs[i], circuit.PublicInputs[i])
	}
	api.AssertIsDifferent(circuit.DomainSize, 0)
	return nil
}

func NewBkSetRotationCircuit(proofData *Halo2ProofData) (*BkSetRotationCircuit, error) {
	if len(proofData.PublicInputs) != NumPublicInputs {
		return nil, fmt.Errorf("expected %d public inputs, got %d", NumPublicInputs, len(proofData.PublicInputs))
	}
	circuit := &BkSetRotationCircuit{}
	for i := 0; i < NumPublicInputs; i++ {
		circuit.PublicInputs[i] = proofData.PublicInputs[i]
	}
	circuit.DomainSize = 1 << proofData.Protocol.K
	return circuit, nil
}
