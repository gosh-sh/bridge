# Testnet Security Status (Sepolia / Shellnet)

**Last updated:** 2026-06-22  
**Classification:** Production = real R15 verifiers (hybrid layout). Identity-stub Groth16 / mocks = **CI and Foundry only**, never deploy targets.

## Summary

**Production policy:** **No identity-stub Groth16 for Primary or Circuit 2 on chain.** Live `AckiNackiBridge` uses:

| Circuit | On-chain verifier | Proof format |
|---------|-------------------|--------------|
| 1A Primary | `PrimaryAggregatorVerifier` (SHPLONK `.bin`) | Aggregator calldata (~3.8 KB) |
| 1B Fallback | `FallbackVerifier` + `FallbackGroth16VerifierGenerated` | 256-byte Groth16 (EIP-170: no SHPLONK `.bin` for 1B) |
| 2 Layer hashes | `LayerHashesAggregatorVerifier` (SHPLONK `.bin`) | Aggregator calldata (~3 KB) |
| 4 Withdrawal | `BridgeWithdrawalAggregatorVerifier` (SHPLONK `.bin`) | Aggregator calldata — **blocked until M4 `.snark`** |

`MockBridgeWithdrawalVerifier` and legacy identity-stub Groth16 for 1A/2 are **CI / Foundry / local dev artefacts** only.

Until **two** SHPLONK `.bin` files (Primary + LayerHashes) exist **and** Circuit 4 lands, AN→ETH entry points (`verifyBlock`, `withdrawByProof`, `applyBkSetUpdate`) stay **paused** or limited to hybrid smoke (1A+2 SHPLONK, 1B Groth16).

Legacy Sepolia deployments that wire stub Groth16 for 1A/2 verify `PublicInput[i] == PublicInput[i]` and **do not check the Halo2 SHPLONK proof**. Treat them as **invalid for AN→ETH** — redeploy with hybrid R15 verifiers or keep paused.

ETH→AN deposits are verified natively on Acki Nacki via `ZKHALO2VERIFYWITHVK` once the relayer submits real Halo2 proofs — that path is sound **when** the correct VK is deployed on the AN `TokenBridge`.

## What is safe vs unsafe today

| Path | Production requirement | Legacy stub deploy (if any) |
|------|------------------------|-------------------------------|
| ETH `deposit()` | N/A (custody only) | Safe for test amounts when unpaused |
| AN `finalizeDeposit` (Halo2) | Real VK on `TokenBridge` | Sound when VK matches circuit |
| ETH `verifyBlock` (1A + 2) | Real SHPLONK aggregators | **Invalid** — stub Groth16 must not ship |
| ETH `verifyBlock` (1B fallback) | Real Groth16 wrapper (off-chain Halo2 checked at wrap time) | Identity stub OK only if operator re-wraps bound proofs |
| ETH `withdrawByProof` (C4) | Real C4 SHPLONK aggregator | **Invalid** — mock/stub must not ship |
| ETH `applyBkSetUpdate` | Real attestation SHPLONK path | **Invalid** until R15 + wiring |

## Operational controls (Phase 0)

1. **Hybrid deploy only** — `DeployRealBridge.s.sol` / `DeployShellnetE2EBridge.s.sol` wire `ShplonkDeployLib.deployVerifyBlockHybridFromEnv()` (Primary + LayerHashes `.bin`; Fallback Groth16). Stub Groth16 for 1A/2 and mocks are test-only.
2. **`pause()` by default** until Primary + LayerHashes `.bin` exist and C4 is ready; owner unpauses only after forgery tests pass on deployed bytecode.
3. **Proving key hygiene** — gnark `proving.key` files under `crates/bridge-prover-orchestrator/gnark-wrappers/*/proving.key` are gitignored; restrict filesystem access on orchestrator hosts and CI artefact stores.
4. **No real funds** — keep Sepolia/shellnet treasury capped; do not bridge mainnet value until SHPLONK `.bin` files pass EIP-170 ≤ 24 576 B and forgery tests are green.

## Exit criteria (R15 + shellnet E2E)

- No identity-stub Groth16 for Primary or Circuit 2 in the production path.
- Two SHPLONK aggregator `.bin` contracts (1A, 2) + Groth16 fallback (1B) + C4 SHPLONK when M4 ships; each SHPLONK runtime bytecode ≤ 24 576 B.
- Foundry forgery suite: tampered inner proofs revert; random stub proofs revert.
- Documented shellnet acceptance run: `docs/shellnet_e2e_acceptance_runbook.md`.

See also: `docs/production_plan.md`, `docs/r15_snark_verifier_roadmap.md`, `docs/r15_verifier_sizing_report.md`.
