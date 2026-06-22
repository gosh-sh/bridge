# Circuit 4 gnark wrapper — RETIRED (2026-06-08)

The identity-stub Groth16 wrapper for Circuit 4 has been **removed** as part of R15.

Use instead:

- Inner proofs: `bridge-prover-orchestrator/src/circuit4_prover.rs` (Poseidon transcript for ETH aggregator)
- On-chain verifier: `contracts/ethereum/src/BridgeWithdrawalAggregatorVerifier.sol`
- Aggregator pipeline: `crates/bridge-evm-aggregator/`

See `docs/r15_snark_verifier_roadmap.md` and `gnark-wrappers/SECURITY.md`.
