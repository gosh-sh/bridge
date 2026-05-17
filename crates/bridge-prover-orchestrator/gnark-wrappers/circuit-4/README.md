# gnark wrapper — Circuit 4 (Bridge Event Prove) **— Phase A scaffold**

This wrapper mirrors `circuit-2/` but with **103 public inputs**:

```
[0]            tokenId      (uint32 packed BE from event body[54..58))
[1]            dappFr       (Fr-encoded AN-side bridge dApp id)
[2]            accFr        (Fr-encoded AN-side bridge account id)
[3..=102]      layerHashes  100 candidate latest-layer hashes
```

corresponding to partner's `bridge-event-prove-circuit` in
[`gosh-sh/acki-nacki-to-eth-bridge-halo2-circuits`](
https://github.com/gosh-sh/acki-nacki-to-eth-bridge-halo2-circuits/tree/main/bridge-event-prove-circuit/src
).

## Status: Phase A — wrapper only, NOT wired into the production bridge

The bridge's Solidity side (`AckiNackiBridge.verifyEvent`,
`BridgeEventVerifier.sol`, `IBridgeEventGroth16Verifier.sol`) is ready and
mock-tested (`AckiNackiBridgeVerifyEvent.t.sol`, 16 tests). What's missing
to flip on Circuit 4 end-to-end:

1. **A real halo2 proof export for Circuit 4.** Today's wrapper has the
   right shape (103 inputs, identity stub on the circuit side, matching the
   trust model of the existing Circuit 1A/1B/2 wrappers in this repo) but
   the partner's `bridge-event-prove-circuit` doesn't yet have a stable
   `halo2_proof.json` exporter the way Circuits 1A/2 do (those go through
   `bridge-prover-orchestrator/src/{primary,layer_hashes}_prover.rs`).
2. **Five open Phase B design questions** — see
   [`docs/circuit_4_open_questions.md`](../../../../docs/circuit_4_open_questions.md)
   in the bridge repo. Chief blockers:
   - **Q-CIRC4-1**: `amount` / `recipient` are currently *private* — a real
     `withdraw()` needs them public.
   - **Q-CIRC4-2**: no nullifier — `verifyEvent` is replayable today.
   - **Q-CIRC4-3**: `recipient` is a 20-byte Ethereum address (good!) but
     `sender` is a TVM `std_addr`; we need to align the `dstChainId`
     semantics before the verifier can route by chain.

Once those are resolved, `setup` / `prove` on this wrapper become
production paths (same as `circuit-2/` is today) and
`Groth16Verifier.sol` ends up at
`contracts/ethereum/src/BridgeEventGroth16VerifierGenerated.sol`.

## Usage (future, once the partner exports a real halo2 proof)

```bash
cd crates/bridge-prover-orchestrator/gnark-wrappers/circuit-4
go run . setup ../../proofs/bridge-event/halo2_proof.json
go run . prove ../../proofs/bridge-event/halo2_proof.json
cp Groth16Verifier.sol \
   ../../../../contracts/ethereum/src/BridgeEventGroth16VerifierGenerated.sol
```

`setup` writes `circuit.r1cs`, `proving.key`, `verification.key`, and the
Solidity verifier — this is the **one-off ceremony** for Circuit 4 (an
audit observation: the trusted-setup story for *all* gnark wrappers in this
repo is Phase 8 of the integration plan and remains open).

`prove` produces a 256-byte `groth16_proof.hex` plus a 103 × 32-byte
`groth16_public_inputs.hex`. The combined JSON is consumed by the bridge's
existing `Groth16OutputJson` shape (see
`crates/bridge-relayer-daemon/src/source.rs`).
