# Testnet Security Status (Sepolia / Shellnet)

**Last updated:** 2026-06-08  
**Classification:** Integration-only — **not cryptographically secure for production**

## Summary

The Sepolia `AckiNackiBridge` deployment and the local shellnet stack use **identity-stub Groth16 wrappers** for AN→ETH circuits 1A, 1B, 2, and 4. Those wrappers verify `PublicInput[i] == PublicInput[i]` and **do not check the Halo2 SHPLONK proof**. Anyone who can produce a valid-looking 256-byte Groth16 proof with arbitrary public inputs can pass on-chain verification until R15 aggregator verifiers land.

ETH→AN deposits are verified natively on Acki Nacki via `ZKHALO2VERIFYWITHVK` once the relayer submits real Halo2 proofs — that path is sound **when** the correct VK is deployed on the AN `TokenBridge`.

## What is safe vs unsafe today

| Path | On-chain crypto | Status |
|------|-----------------|--------|
| ETH `deposit()` | N/A (custody only) | Safe for test amounts |
| AN `finalizeDeposit` (Halo2) | Real SHPLONK on AN | Sound when VK matches circuit |
| ETH `verifyBlock` (1A/1B + 2) | Stub Groth16 | **Unsafe** — PI-only tautology |
| ETH `withdrawByProof` (C4) | Mock or stub Groth16 | **Unsafe** until R15 C4 aggregator |
| ETH `applyBkSetUpdate` | Attestation via same stub | **Unsafe** until R15 + on-chain wiring |

## Operational controls (Phase 0)

1. **Label deployments** — runbooks and operator docs must state “integration-only, not mainnet-ready.”
2. **`pause()`** — bridge owner can halt `deposit`, `verifyBlock`, `withdrawByProof`, and `applyBkSetUpdate` on suspicion of PK compromise or active exploitation.
3. **Proving key hygiene** — gnark `proving.key` files under `crates/bridge-prover-orchestrator/gnark-wrappers/*/proving.key` are gitignored; restrict filesystem access on orchestrator hosts and CI artefact stores.
4. **No real funds** — keep Sepolia/shellnet treasury capped; do not bridge mainnet value until all four R15 verifier `.bin` files pass the EIP-170 ≤ 24 576 B gate and forgery tests are green.

## Exit criteria (R15 + shellnet E2E)

- Zero identity-stub Groth16 in the production path.
- Four separate SHPLONK aggregator verifier contracts (1A, 1B, 2, 4), each runtime bytecode ≤ 24 576 B.
- Foundry forgery suite: tampered inner proofs revert; random Groth16 stub proofs revert.
- Documented shellnet acceptance run: `docs/shellnet_e2e_acceptance_runbook.md`.

See also: `docs/r15_snark_verifier_roadmap.md`, `docs/r15_verifier_sizing_report.md`.
