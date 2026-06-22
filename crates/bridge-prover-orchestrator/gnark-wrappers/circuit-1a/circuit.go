package main

import (
	"fmt"

	"github.com/consensys/gnark/frontend"
)

// NumPublicInputs matches Circuit 1A (Primary attestation) public-input layout
// from `bridge_prover_lib::prover::ProofOutput`'s `[block_id_fr,
// bk_set_commitment_fr, block_seq_no, last_seen_block_seqno]` ordering. This
// is the same 4-element layout as Circuit 1B (Fallback) — by design, so the
// bridge contract sees a uniform public-input shape regardless of finalization
// path.
const NumPublicInputs = 4

// PrimaryVerifierCircuit is a Groth16 circuit that commits to the four public
// inputs of the Circuit 1A Halo2 SHPLONK proof. Same identity-stub trust model
// as the layer-hashes / BK-rotation / Fallback wrappers in this repo.
type PrimaryVerifierCircuit struct {
	PublicInputs [NumPublicInputs]frontend.Variable `gnark:",public"`
	DomainSize   frontend.Variable
}

func (circuit *PrimaryVerifierCircuit) Define(api frontend.API) error {
	for i := 0; i < NumPublicInputs; i++ {
		api.AssertIsEqual(circuit.PublicInputs[i], circuit.PublicInputs[i])
	}
	api.AssertIsDifferent(circuit.DomainSize, 0)
	return nil
}

func NewPrimaryVerifierCircuit(proofData *Halo2ProofData) (*PrimaryVerifierCircuit, error) {
	if len(proofData.PublicInputs) != NumPublicInputs {
		return nil, fmt.Errorf("expected %d public inputs, got %d", NumPublicInputs, len(proofData.PublicInputs))
	}
	circuit := &PrimaryVerifierCircuit{}
	for i := 0; i < NumPublicInputs; i++ {
		circuit.PublicInputs[i] = proofData.PublicInputs[i]
	}
	circuit.DomainSize = 1 << proofData.Protocol.K
	return circuit, nil
}
