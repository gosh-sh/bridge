package main

import (
	"fmt"

	"github.com/consensys/gnark/frontend"
)

// NumPublicInputs matches Circuit 4 (`bridge-event-prove-circuit`,
// single-final-root layout — partner branch
// `circuit4-single-final-root`) public-input layout emitted by
// `bridge-prover-orchestrator`:
//
//	[0]  tokenId       (uint32 packed BE from event body[54..58))
//	[1]  amount        (uint128)
//	[2]  recipientHi   (Fr; top 10 bytes of 20-byte EVM address, BE)
//	[3]  recipientLo   (Fr; bottom 10 bytes of 20-byte EVM address, BE)
//	[4]  dstChainId    (uint256)
//	[5]  senderAccFr   (Fr; AN-side sender 256-bit account id)
//	[6]  dappFr        (Fr; bridge dApp identifier on AN side)
//	[7]  accFr         (Fr; bridge account identifier on AN side)
//	[8]  nullifier     (Fr; Poseidon(block_id_fr, tokenId, amount,
//	                    recipientHi, recipientLo, senderAccFr))
//	[9]  finalRoot     (Fr; off-circuit anchor checked against
//	                    `_knownAnchors` in `AckiNackiBridge.sol`)
//
// = 10 BN254 Fr field elements (matches `PUB_*` slot indices in
// `bridge_event_prove_circuit::PUB_*`; `TOTAL_PUBLIC_INPUTS = 10`).
//
// IMPORTANT — R15 status. This wrapper enforces only identity-stub
// assertions over the public inputs. The Halo2 SHPLONK proof itself
// is **NOT verified** inside Groth16. Replacing the wrapper with a
// real Halo2-in-gnark verifier is tracked as Phase 8 of
// `docs/an_partner_integration_plan.md` and is the single biggest
// open mainnet blocker on the AN→ETH side.
const NumPublicInputs = 10

// BridgeWithdrawalVerifierCircuit is a Groth16 circuit that commits to
// the 10 public inputs of the Circuit 4 (single-final-root) Halo2
// SHPLONK proof. Same identity-stub trust model as the sibling wrappers
// in this repo: the gnark circuit only enforces that the public inputs
// flow through unchanged; the upstream Halo2 proof is (today) the only
// cryptographic guarantee.
type BridgeWithdrawalVerifierCircuit struct {
	PublicInputs [NumPublicInputs]frontend.Variable `gnark:",public"`
	DomainSize   frontend.Variable
}

func (circuit *BridgeWithdrawalVerifierCircuit) Define(api frontend.API) error {
	for i := 0; i < NumPublicInputs; i++ {
		api.AssertIsEqual(circuit.PublicInputs[i], circuit.PublicInputs[i])
	}
	api.AssertIsDifferent(circuit.DomainSize, 0)
	return nil
}

func NewBridgeWithdrawalVerifierCircuit(proofData *Halo2ProofData) (*BridgeWithdrawalVerifierCircuit, error) {
	if len(proofData.PublicInputs) != NumPublicInputs {
		return nil, fmt.Errorf("expected %d public inputs, got %d", NumPublicInputs, len(proofData.PublicInputs))
	}
	circuit := &BridgeWithdrawalVerifierCircuit{}
	for i := 0; i < NumPublicInputs; i++ {
		circuit.PublicInputs[i] = proofData.PublicInputs[i]
	}
	circuit.DomainSize = 1 << proofData.Protocol.K
	return circuit, nil
}
