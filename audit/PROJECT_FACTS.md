# Bridge — project facts (audit reference)

Keep this file aligned with code. Full architecture: `docs/architecture/four_circuit_architecture.md`.

## Scope

Cross-chain bridge ETH ↔ Acki Nacki via ZK proofs. This repo: **Ethereum contracts**, relayers, deposit-prover, integration docs. AN contracts: **`../acki-nacki`**.

## Entry points

| Flow | ETH | AN | PI count |
|------|-----|-----|----------|
| Deposit | `AckiNackiBridge.deposit()` (USDC) | `USDCBridge.finalizeDeposit` | **12** (`depositId`, `sender`, `amount`, `contractAddress`, `chainId`, `dappIdHigh`, `dappIdLow`, `anAccountHigh`, `anAccountLow`, `blockHashHigh`, `blockHashLow`, `promiseCommit` — see `deposit-prover/src/circuit_v2.rs` `DEPOSIT_PUBLIC_INPUT_LAYOUT`) |
| State sync | `AckiNackiBridge.verifyBlock()` | — | 4 + 14 (1A/1B + 2) |
| Withdraw | `AckiNackiBridge.withdrawByProof()` | withdrawal event | **10** |

Retired: ETH-side deposit Groth16, `withdraw()`, `verifyEvent`, 103-PI withdraw.

## ZK boundaries

- Deposit: Halo2 SHPLONK on AN via `ZKHALO2VERIFYWITHVK` (3 operands: vk, pis, proof)
- AN→ETH on-chain: R15 SHPLONK aggregators (Primary / Fallback / LayerHashes)
- Circuit 4 withdraw: mock verifier in Foundry; production aggregator pending R15 M3–M7
- KZG: **Acki Nacki chain ceremony** — not Hermez

## Deposit verification layers (TD-43 / DEP-MOCK-VS-REAL)

Do **not** treat MockProver or struct `verify_proof` green as proxy for AN acceptance.

| Layer | What it checks | Proxy for AN opcode? |
|-------|----------------|----------------------|
| MockProver (`test_circuit_mock`) | Constraint satisfaction on witness | **No** — no SHPLONK bytes |
| `prover::verify_proof` | bincode `Snark` deserialize + PI field match | **No** — no crypto / wrong wire (Poseidon export) |
| `verify_deposit_opcode_triple` | VkBlob + Blake2b SHPLONK + 12×32 B PI | **Yes** — mirrors opcode handler |
| AN VM `ZKHALO2VERIFYWITHVK` | On-chain triple | Production gate |

Deposit VkBlob is **RLC** shape (12 PI, `9dacd998…` pin — TD-42). Use `verify_deposit_opcode_triple` / `examples/verify_opcode_triple.rs`, **not** `Halo2TvmOperands::verify` (Base VkBlob only).

CI smoke: `scripts/check_mock_vs_shplonk_smoke.sh` (after `check_vk_srs_pin.sh`). PoC: `deposit-prover/tests/td_43_mock_vs_shplonk.rs`, notes `audit/reports/td-43-mock-vs-shplonk-notes.md`.

## Mutation kill score (TD-49 / DEP-MUTATION-TESTING / D-25)

**Pinned score:** `KILLED 13/13 (100%)` on the documented kill set (7 relayer `check_binds_to` bound fields + 4 L1 overlay mutants + 2 circuit PI flips).  
**QC survivors (3, intentional, not gaps):** `dappId` hi/lo, `promiseCommit`, `anWorkchain` — not event-bound at relayer (TD-05 / TD-21 / TD-31).

| Layer | PoC |
|-------|-----|
| Relayer | `crates/deposit-relayer-daemon/tests/td_49_mutation_kill_matrix.rs` |
| L1 overlay | `audit/spec/ethereum/DepositMutationKill.t.sol` |
| Circuit | `deposit-prover/tests/td_49_mutation_kill.rs` |

Re-run when changing `check_binds_to`, `deposit()` CEI/accounting, `nonReentrant`, or PI bind layout: `scripts/check_mutation_kill_smoke.sh` (in `check_deposit_audit_gates.sh` after TD-43). Notes: `audit/reports/td-49-mutation-score-notes.md`.

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

## Trust assumptions (ETH custody)

- **USDC (Sepolia / mainnet):** bridge assumes standard ERC-20 semantics — exact `transferFrom` credit, no fee-on-transfer. Mainnet USDC is an upgradeable proxy; Circle blacklist/pause on the bridge address would freeze flows (operational risk, not a Solidity bug). **Author confirm:** acceptable for target deployment?
- **Bridge pause (#20 / Stage II ETH-4):** `AckiNackiBridge` has **no** `pause()` / `whenNotPaused` on `deposit`, `verifyBlock`, or `withdrawByProof` — only `nonReentrant`. Token pause/blacklist is external (TD-24/58). Relayer BK sentry may pause **off-chain** submission. PDF ETH-4 guardian pause is **not** restored; ETH-1/ETH-2 are field-range gated instead (`audit/findings/BRIDGE-ETH-04/`).
- **ETH-5 constructor:** Circuit 4 withdraw requires the full verifyBlock triple (`WithdrawRequiresVerifyBlock`). Partial 1A/1B/C2 wiring reverts `PartialVerifyBlockWiring`. Deposit-only and verifyBlock-only remain legal. Mainnet `DeployRealBridge` requires `USE_AXIOM_ORACLE` + `WIRE_VERIFY_BLOCK`. See `audit/findings/BRIDGE-ETH-05/`.
- **ETH-6 artefacts:** pin `contracts/ethereum/verifiers/SHA256SUMS` and CREATE `extcodehash` in `ShplonkDeployLib` (`PRIMARY_YUL_CODEHASH` / …). Default pairing `ShplonkArtefactPairing.t.sol` — 1A/1B/C2/C4 all PASS (n14 regen 2026-09-08). See `audit/findings/BRIDGE-ETH-06/`.
- **solc:** Foundry `solc_version = "0.8.21"` + `via_ir` (ETH-10 / `BRIDGE-ETH-10`).
- **ETH-8 ownership:** `transferOwnership` nominates `pendingOwner`; `acceptOwnership` completes (ETH-8). See `audit/findings/BRIDGE-ETH-08/`.
- **ETH-7 emergency leftover aToken:** `emergencyWithdrawAll` reverts `EmergencyLeftoverAToken` if aUSDC remains after `withdraw(max)` — leftover shares must not become `harvestYield`. Liquid excess remains `skimExcessUsdc` (QC-A1-3). See `audit/findings/BRIDGE-ETH-07/`.
- **ETH-9:** mainnet `DeployRealBridge` requires `altTokenId == 0` (not a constructor `chainid` check — Foundry binds Circuit 4 with `vm.chainId(1)`). `supplyToAave` checks `approve` bool (`ApproveFailed`). FoT / rebase fail closed (`TransferAmountMismatch`, ETH-11). See `audit/findings/BRIDGE-ETH-09/`, `audit/findings/BRIDGE-ETH-11/`.
- **QC-AN-10 / WD-AN-07:** ETH keeps `InvalidAnAccount` / `InvalidRecipient`. AN `finalizeDeposit` / `confirmDeposit` / `initiateWithdrawal` require non-zero recipient (`ERR_ZERO_RECIPIENT`). Snapshot code **223**; production sibling `acki-nacki` code **230** (`223` is already `ERR_WRONG_DAPP` there). See `audit/findings/BRIDGE-AN-10/`.
- **QC-OFF-01:** official daemon stays strictly sequential (`next_target = last_processed + 1`); out-of-order finalize is rejected ([issue #34](https://github.com/gosh-sh/bridge/issues/34)). Production systemd uses `--skip-after-attempts` / `SKIP_AFTER_ATTEMPTS=64`; CLI default remains 0. Policy: `docs/audit/qc-off-01-hol-policy.md`.
- **Per-tx deposit cap:** `MAX_DEPOSIT_AMOUNT = type(uint64).max` on main (#20); overlay documents aggregate TVL uncapped (`DepositWhaleCap.t.sol`). Live docs no longer claim 100 USDC as the contract cap (frontend form still has a 100 USDC convenience cap).

## Audit classification (ETH pass 2026-07)

- **QC:** PoC exists; need intent to classify bug vs feature (`audit/reports/closeout-eth.md`).
- **BC:** PoC exists; behaviour likely wrong under bridge assumptions — bug **candidate**, not confirmed.
- **OK:** No action.

This pass: BC=0, QC=13 (all PoC-linked), audit overlay 47/47 green.

## AN audit tooling (Phase F0)

| Item | Path |
|------|------|
| Toolchain symlinks | `.tools/{sold,tvm-debugger,tvm-cli}` (gitignored) |
| Setup script | `scripts/setup_an_audit_tools.sh` |
| Contract sync | `scripts/sync_an_contracts.sh` → `audit/spec/an-contracts/` (`acki-nacki@contracts/bridge`) |
| Pytest harness | `audit/spec/an/` (`test_base.py` from dex) |
| Agent briefing | `audit/spec/an/AGENT_CONTEXT.md` |
| TVM knowledge | `audit/knowledge/01-05` → `../dex/knowledge/` |
| Plan | `audit/reports/an-audit-plan.md` |

Gate: `make pre-push-an` (toolchain smoke). Full spec after contract sync.
