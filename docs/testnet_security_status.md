# Testnet Security Status (Sepolia / Shellnet)

**Last updated:** 2026-06-19  
**Classification:** Production = real Shplonk verifiers only. Stub Groth16 / mocks = **CI and Foundry only**, never deploy targets.

## Summary

**Production policy:** **No stubs on chain.** Every live `AckiNackiBridge` must use real R15 SHPLONK aggregator verifiers only. Identity-stub Groth16 wrappers and `MockBridgeWithdrawalVerifier` are **CI / Foundry / local dev artefacts** — they must not appear in deploy scripts or operator runbooks. Until four real `.bin` verifiers exist, AN→ETH entry points (`verifyBlock`, `withdrawByProof`, `applyBkSetUpdate`) stay **paused**.

Legacy Sepolia deployments that still wire stub Groth16 verify `PublicInput[i] == PublicInput[i]` and **do not check the Halo2 SHPLONK proof**. Treat them as **invalid for AN→ETH** — redeploy with Shplonk verifiers or keep paused.

ETH→AN deposits are verified natively on Acki Nacki via `ZKHALO2VERIFYWITHVK` once the relayer submits real Halo2 proofs — that path is sound **when** the correct VK is deployed on the AN `TokenBridge`.

## What is safe vs unsafe today

| Path | Production requirement | Legacy stub deploy (if any) |
|------|------------------------|-------------------------------|
| ETH `deposit()` | N/A (custody only) | Safe for test amounts when unpaused |
| AN `finalizeDeposit` (Halo2) | Real VK on `USDCBridge` | Sound when VK matches circuit |
| ETH `verifyBlock` (1A/1B + 2) | Real Shplonk aggregators × 3 | **Invalid** — stub Groth16 must not ship |
| ETH `withdrawByProof` (C4) | Real C4 Shplonk aggregator | **Invalid** — mock/stub must not ship |
| ETH `applyBkSetUpdate` | Real attestation Shplonk path | **Invalid** until R15 + wiring |

## Operational controls (Phase 0)

1. **No stub deploys** — `DeployRealBridge.s.sol` / `DeployShellnetE2EBridge.s.sol` wire Shplonk aggregator adapters via `ShplonkDeployLib` (`.bin` under `verifiers/`). Stub Groth16 and mocks are test-only.
2. **`pause()` by default** until four real `.bin` verifiers are on chain; owner unpauses only after forgery tests pass on the deployed bytecode.
3. **Proving key hygiene** — gnark `proving.key` files under `crates/bridge-prover-orchestrator/gnark-wrappers/*/proving.key` are gitignored; restrict filesystem access on orchestrator hosts and CI artefact stores.
4. **No real funds** — keep Sepolia/shellnet treasury capped; do not bridge mainnet value until all four R15 verifier `.bin` files pass the EIP-170 ≤ 24 576 B gate and forgery tests are green.

## Exit criteria (R15 + shellnet E2E)

- Zero identity-stub Groth16 in the production path.
- Four separate SHPLONK aggregator verifier contracts (1A, 1B, 2, 4), each runtime bytecode ≤ 24 576 B.
- Foundry forgery suite: tampered inner proofs revert; random Groth16 stub proofs revert.
- Documented shellnet acceptance run: `docs/shellnet_e2e_acceptance_runbook.md`.

See also: `docs/r15_snark_verifier_roadmap.md`, `docs/r15_verifier_sizing_report.md`.
