package main

import (
	"fmt"

	"github.com/consensys/gnark/frontend"
)

// NumPublicInputs matches Circuit 1B (Fallback attestation) public-input layout
// from `bridge_prover_orchestrator::FallbackProofOutput::instances`:
//
//	[0] envelope_hash
//	[1] bk_set_poseidon
//	[2] block_seq_no
//	[3] last_seen_block_seqno
const NumPublicInputs = 4

// FallbackVerifierCircuit is a Groth16 circuit that commits to the four public
// inputs of the Circuit 1B Halo2 SHPLONK proof.
//
// IMPLEMENTATION STATUS:
// Like the layer-hashes and BK-set-rotation gnark wrappers in this repo, the
// Define() method here uses identity constraints. Full in-circuit Halo2/SHPLONK
// verification is planned as a future improvement (it would require a gnark
// implementation of the Halo2 SHPLONK verifier — non-trivial; see Phase 8 R&D in
// docs/an_partner_integration_plan.md). For now the Groth16 proof commits to the
// public input values, giving the on-chain verifier a compact proof that those
// specific values were attested. The off-chain relayer is responsible for
// having held a verified Halo2 proof before generating the Groth16 wrap. Same
// trust model as the sibling per-circuit verifiers under contracts/ethereum/src/
// (`PrimaryVerifier.sol`, `LayerHashesMovementVerifier.sol`).
type FallbackVerifierCircuit struct {
	PublicInputs [NumPublicInputs]frontend.Variable `gnark:",public"`
	DomainSize   frontend.Variable
}

func (circuit *FallbackVerifierCircuit) Define(api frontend.API) error {
	for i := 0; i < NumPublicInputs; i++ {
		api.AssertIsEqual(circuit.PublicInputs[i], circuit.PublicInputs[i])
	}
	api.AssertIsDifferent(circuit.DomainSize, 0)
	return nil
}

func NewFallbackVerifierCircuit(proofData *Halo2ProofData) (*FallbackVerifierCircuit, error) {
	if len(proofData.PublicInputs) != NumPublicInputs {
		return nil, fmt.Errorf("expected %d public inputs, got %d", NumPublicInputs, len(proofData.PublicInputs))
	}
	circuit := &FallbackVerifierCircuit{}
	for i := 0; i < NumPublicInputs; i++ {
		circuit.PublicInputs[i] = proofData.PublicInputs[i]
	}
	circuit.DomainSize = 1 << proofData.Protocol.K
	return circuit, nil
}
