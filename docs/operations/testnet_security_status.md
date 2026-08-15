# Testnet Security Status (Sepolia / Shellnet)

**Last updated:** 2026-06-22  
**Classification:** Production = real R15 SHPLONK aggregator verifiers (1A/1B/2). Identity-stub Groth16 / mocks = **CI and Foundry only**, never deploy targets.

## Summary

**Production policy:** **No identity-stub Groth16 for Primary or Circuit 2 on chain.** Live `AckiNackiBridge` uses:

| Circuit | On-chain verifier | Proof format |
|---------|-------------------|--------------|
| 1A Primary | `PrimaryAggregatorVerifier` (SHPLONK `.bin`) | Aggregator calldata (~3.8 KB) |
| 1B Fallback | `FallbackAggregatorVerifier` (SHPLONK `.bin`, K=21 inner) | Aggregator calldata (~3.8 KB) |
| 2 Layer hashes | `LayerHashesAggregatorVerifier` (SHPLONK `.bin`) | Aggregator calldata (~3 KB) |
| 4 Withdrawal | `BridgeWithdrawalAggregatorVerifier` (SHPLONK `.bin`) | Aggregator calldata — **blocked until M4 `.snark`** |

`MockBridgeWithdrawalVerifier` and legacy identity-stub Groth16 for 1A/2 are **CI / Foundry / local dev artefacts** only.

Until the **three** attestation/layer SHPLONK `.bin` files (Primary + Fallback + LayerHashes) exist **and** Circuit 4 lands, AN→ETH entry points (`verifyBlock`, `withdrawByProof`, `applyBkSetUpdate`) stay **paused** or limited to attestation smoke (1A/1B/2 SHPLONK).

Legacy Sepolia deployments that wire stub Groth16 for 1A/2 verify `PublicInput[i] == PublicInput[i]` and **do not check the Halo2 SHPLONK proof**. Treat them as **invalid for AN→ETH** — redeploy with R15 SHPLONK verifiers or keep paused.

ETH→AN deposits are verified natively on Acki Nacki via `ZKHALO2VERIFYWITHVK` once the relayer submits real Halo2 proofs — that path is sound **when** the correct VK is deployed on the AN `TokenBridge`.

## What is safe vs unsafe today

| Path | Production requirement | Legacy stub deploy (if any) |
|------|------------------------|-------------------------------|
| ETH `deposit()` | N/A (custody only) | Always callable; no bridge `pause` (#20). USDC token pause = external risk |
| AN `finalizeDeposit` (Halo2) | Real VK on `TokenBridge` | Sound when VK matches circuit |
| ETH `verifyBlock` (1A + 1B + 2) | Real SHPLONK aggregators | **Invalid** — stub Groth16 must not ship |
| ETH `withdrawByProof` (C4) | Real C4 SHPLONK aggregator | **Invalid** — mock/stub must not ship |
| ETH `applyBkSetUpdate` | Real attestation SHPLONK path | **Invalid** until R15 + wiring |

## Operational controls (Phase 0)

1. **SHPLONK deploy only** — `DeployRealBridge.s.sol` / `DeployShellnetE2EBridge.s.sol` wire `ShplonkDeployLib.deployVerifyBlockProductionFromEnv()` (Primary + Fallback + LayerHashes `.bin`). Stub Groth16 for 1A/2 and mocks are test-only.
2. **No bridge `pause()` (#20)** — operational halt via relayer BK sentry / USDC token pause; do not assume `AckiNackiBridge.pause()` exists.
3. **Proving key hygiene** — gnark `proving.key` files under `crates/bridge-snark-utils/gnark-wrappers/*/proving.key` are gitignored; restrict filesystem access on orchestrator hosts and CI artefact stores.
4. **No real funds** — keep Sepolia/shellnet treasury capped; do not bridge mainnet value until SHPLONK `.bin` files pass EIP-170 ≤ 24 576 B and forgery tests are green.

## Exit criteria (R15 + shellnet E2E)

- No identity-stub Groth16 for Primary or Circuit 2 in the production path.
- Three SHPLONK aggregator `.bin` contracts (1A, 1B, 2) + C4 SHPLONK when M4 ships; each SHPLONK runtime bytecode ≤ 24 576 B.
- Foundry forgery suite: tampered inner proofs revert; random stub proofs revert.
- Documented shellnet acceptance run: `docs/shellnet_e2e_acceptance_runbook.md`.

See also: docs/operations/production_plan.md, `docs/zk/evm-side/r15_snark_verifier_roadmap.md`, `docs/zk/evm-side/r15_verifier_sizing_report.md`.
