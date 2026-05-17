package main

import (
	"fmt"

	"github.com/consensys/gnark/frontend"
)

// NumPublicInputs matches Circuit 4 (Bridge Event Prove) public-input layout
// emitted by `bridge-event-prove-circuit` (sibling repo
// `gosh-sh/acki-nacki-to-eth-bridge-halo2-circuits`):
//
//	[0]            tokenId        (uint32 BE-packed from event body[54..58))
//	[1]            dappFr         (Fr-encoded AN-side bridge dApp id)
//	[2]            accFr          (Fr-encoded AN-side bridge account id)
//	[3..=102]      layerHashes    100 candidate latest-layer hashes
//
// = 103 BN254 Fr field elements (3 + NumLayerHashes).
//
// IMPORTANT — Phase A status (see ../../README.md and
// `docs/circuit_4_open_questions.md` in the bridge repo). The partner's
// Circuit 4 keeps `dstChainId`, `amount`, `recipient`, `sender` as private
// witnesses. The bridge therefore cannot deploy a real `withdraw()` yet,
// and this wrapper is a Phase A scaffold: `setup` / `prove` will run against
// an actual halo2 proof once it lands, but until then the only consumer is
// the contract-side `verifyEvent` adapter (mock-tested).
const NumLayerHashes = 100
const NumPublicInputs = 3 + NumLayerHashes

// BridgeEventVerifierCircuit is a Groth16 circuit that commits to the 103
// public inputs of the Circuit 4 Halo2 SHPLONK proof. Same identity-stub
// trust model as the sibling wrappers in this repo: the gnark circuit only
// enforces that the public inputs flow through unchanged; the upstream
// Halo2 proof is the actual cryptographic guarantee.
type BridgeEventVerifierCircuit struct {
	PublicInputs [NumPublicInputs]frontend.Variable `gnark:",public"`
	DomainSize   frontend.Variable
}

func (circuit *BridgeEventVerifierCircuit) Define(api frontend.API) error {
	for i := 0; i < NumPublicInputs; i++ {
		api.AssertIsEqual(circuit.PublicInputs[i], circuit.PublicInputs[i])
	}
	api.AssertIsDifferent(circuit.DomainSize, 0)
	return nil
}

func NewBridgeEventVerifierCircuit(proofData *Halo2ProofData) (*BridgeEventVerifierCircuit, error) {
	if len(proofData.PublicInputs) != NumPublicInputs {
		return nil, fmt.Errorf("expected %d public inputs, got %d", NumPublicInputs, len(proofData.PublicInputs))
	}
	circuit := &BridgeEventVerifierCircuit{}
	for i := 0; i < NumPublicInputs; i++ {
		circuit.PublicInputs[i] = proofData.PublicInputs[i]
	}
	circuit.DomainSize = 1 << proofData.Protocol.K
	return circuit, nil
}
