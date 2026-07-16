# Bridge — project facts (audit reference)

Keep this file aligned with code. Full architecture: `docs/architecture/four_circuit_architecture.md`.

## Scope

Cross-chain bridge ETH ↔ Acki Nacki via ZK proofs. This repo: **Ethereum contracts**, relayers, deposit-prover, integration docs. AN contracts: **`../acki-nacki`**.

## Entry points

| Flow | ETH | AN | PI count |
|------|-----|-----|----------|
| Deposit | `AckiNackiBridge.deposit()` (USDC) | `USDCBridge.finalizeDeposit` | **11** |
| State sync | `AckiNackiBridge.verifyBlock()` | — | 4 + 14 (1A/1B + 2) |
| Withdraw | `AckiNackiBridge.withdrawByProof()` | withdrawal event | **10** |

Retired: ETH-side deposit Groth16, `withdraw()`, `verifyEvent`, 103-PI withdraw.

## ZK boundaries

- Deposit: Halo2 SHPLONK on AN via `ZKHALO2VERIFYWITHVK` (3 operands: vk, pis, proof)
- AN→ETH on-chain: R15 SHPLONK aggregators (Primary / Fallback / LayerHashes)
- Circuit 4 withdraw: mock verifier in Foundry; production aggregator pending R15 M3–M7
- KZG: **Acki Nacki chain ceremony** — not Hermez

## Cross-circuit binding (CC-#)

`verifyBlock` requires matching `block_id` and `bk_set_poseidon` across attestation (1A or 1B) and layer-hashes (2) proofs, plus monotonic `block_seq_no` and Poseidon chain anchor.

## Known audit assumptions (partner circuits)

- BLS: no G2 subgroup check (BLS-1)
- Layer hashes: no semantic BTreeMap validation in circuit (D-1)
- Bincode layout constants fragile across node releases (L-1)

## Test baselines (2026-07)

- Foundry: ~174 tests / 21 suites under `contracts/ethereum/test/`
- `bridge-relayer-daemon`: unit tests + `#[ignore]` live BK-set
- `deposit-relayer-daemon`: 32 lib + fixture + live discovery tests

## Sibling repos

`acki-nacki`, `tvm-sdk`, `acki-nacki-to-eth-bridge-halo2-prover`, `acki-nacki-to-eth-bridge-halo2-circuits`, `deposit-prover` (in-tree), partner halo2 forks.

## Lessons not to repeat

1. **Constant drift** — prover vs node literals (e.g. `HISTORY_PROOF_WINDOW_SIZE`); prefer compile-time import from one crate
2. **Embedded child code** — parent redeploy must re-embed recompiled child (DepositVoucher)
3. **VkBlob / PI mismatch** — smoke tests with wrong VK (4-PI fallback) ≠ deposit E2E
4. **SRS mismatch** — proofs keyed on wrong ceremony fail opcode verify silently at integration
5. **LLM greenwash** — tightening fuzz bounds hides real bridge bugs (see `.cursor/rules/bridge-core-audit.mdc`)
