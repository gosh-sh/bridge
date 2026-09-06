# Documentation index

Canonical documentation for the Acki Nacki ↔ Ethereum bridge. Historical agent notes, partner Q&A, and superseded plans live in [`../_archive/`](../_archive/README.md).

## Start here

| Document | Audience |
|----------|----------|
| [../README.md](../README.md) | Project overview, build, repo layout |
| [architecture/four_circuit_architecture.md](architecture/four_circuit_architecture.md) | Four-circuit ZK architecture (AN→ETH state + withdrawal) |
| [architecture/integration_analysis.md](architecture/integration_analysis.md) | Two-sided codebase map |
| [operations/bridge_verification.md](operations/bridge_verification.md) | Invariant labels (DEP-#, LH-#, CC-#, …) |
| [integration/an_partner_integration_plan.md](integration/an_partner_integration_plan.md) | Phase roadmap + Decision Log |

## By topic

### Architecture

- [architecture/four_circuit_architecture.md](architecture/four_circuit_architecture.md) — Circuits 1A/1B/2/3/4, cross-circuit binding
- [architecture/audit_trail_v2.md](architecture/audit_trail_v2.md) — v1→v2 trust delta
- [architecture/integration_analysis.md](architecture/integration_analysis.md) — Ethereum side + partner circuits
- [architecture/diagrams/keccak_coprocessor_flowchart.mmd](architecture/diagrams/keccak_coprocessor_flowchart.mmd) — deposit keccak coprocessor

### Operations & verification

- [operations/verifying_an_proof.md](operations/verifying_an_proof.md) — AN→ETH proof verification flow
- [operations/verifying_eth_proof_on_an.md](operations/verifying_eth_proof_on_an.md) — ETH→AN deposit proof on AN
- [operations/manual_verification_runbook.md](operations/manual_verification_runbook.md) — Hands-on audit protocol
- [operations/evm_an_deposit_e2e_runbook.md](operations/evm_an_deposit_e2e_runbook.md) — Cross-repo deposit E2E operator guide
- [operations/an_bridge_prover_runbook.md](operations/an_bridge_prover_runbook.md) — Live AN→ETH prover daemons
- [operations/an_bridge_prover_fallback_path.md](operations/an_bridge_prover_fallback_path.md) — Primary vs Fallback attestation
- [operations/production_plan.md](operations/production_plan.md) — Production deployment gates
- [operations/testnet_security_status.md](operations/testnet_security_status.md) — Testnet security posture
- [operations/aave/aave_integration.md](operations/aave/aave_integration.md) — AAVE V3 yield (USDC)

### Shellnet / E2E

- [operations/shellnet/shellnet_e2e_acceptance_runbook.md](operations/shellnet/shellnet_e2e_acceptance_runbook.md)
- [operations/shellnet/shellnet_an_eth_relayer_wiring.md](operations/shellnet/shellnet_an_eth_relayer_wiring.md)
- [operations/shellnet/shellnet_usdcbridge_deposit_vk_redeploy.md](operations/shellnet/shellnet_usdcbridge_deposit_vk_redeploy.md)

### ZK / cryptography

**AN side (native Halo2):**

- [zk/an-side/zkhalo2verifywithvk_reference.md](zk/an-side/zkhalo2verifywithvk_reference.md) — Frozen `ZKHALO2VERIFYWITHVK` ABI
- [zk/an-side/deposit_vk_witness_independence.md](zk/an-side/deposit_vk_witness_independence.md) — Production deposit VkBlob
- [zk/an-side/zk_halo2_an_side_design.md](zk/an-side/zk_halo2_an_side_design.md) — Design memo (historical context)

**EVM side (R15 SHPLONK aggregators):**

- [zk/evm-side/r15_snark_verifier_roadmap.md](zk/evm-side/r15_snark_verifier_roadmap.md)
- [zk/evm-side/r15_verifier_sizing_report.md](zk/evm-side/r15_verifier_sizing_report.md)
- [zk/evm-side/halo2_on_chain_verification_paths.md](zk/evm-side/halo2_on_chain_verification_paths.md)
- [zk/evm-side/BLAKE2B_HALO2_VERIFIER.md](zk/evm-side/BLAKE2B_HALO2_VERIFIER.md) — Blake2b transcript reference
- [../contracts/ethereum/verifiers/README.md](../contracts/ethereum/verifiers/README.md) — On-chain `.bin` verifier generation

### Audit

- [../audit/README.md](../audit/README.md) — Pruvendo audit overlay (findings, spec tests)
- [../audit/PROJECT_FACTS.md](../audit/PROJECT_FACTS.md) — Domain facts for agents
- [../audit/reports/eth-audit-plan.md](../audit/reports/eth-audit-plan.md) — **ETH contracts audit plan** (branch `audit`)
- [audit/layer_hashes_circuit_audit.md](audit/layer_hashes_circuit_audit.md) — Partner layer-hashes circuit (Phase 0)
- [audit/qc-off-01-hol-policy.md](audit/qc-off-01-hol-policy.md) — Keep strict depositId HOL; reject out-of-order finalize
- [audit/deposit-relayer-operator-runbook.md](audit/deposit-relayer-operator-runbook.md) — Deposit relayer ops (park / `finalize-one`)

### End users

- [user/USER_GUIDE.md](user/USER_GUIDE.md) — Bridge user guide (USDC)

## Crate-level READMEs

| Crate | README |
|-------|--------|
| `deposit-prover/` | [../deposit-prover/README.md](../deposit-prover/README.md) |
| `crates/an-bridge-prover/` | [../crates/an-bridge-prover/README.md](../crates/an-bridge-prover/README.md) |
| `crates/bridge-evm-aggregator/` | [../crates/bridge-evm-aggregator/README.md](../crates/bridge-evm-aggregator/README.md) |
| `contracts/ethereum/verifiers/` | [../contracts/ethereum/verifiers/README.md](../contracts/ethereum/verifiers/README.md) |

## Archive

Superseded docs, partner Q&A, dated handoffs, and agent-generated notes: [`../_archive/README.md`](../_archive/README.md).
