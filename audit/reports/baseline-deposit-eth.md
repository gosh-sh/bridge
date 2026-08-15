# Baseline — ETH deposit (open questions only)

**Ветка:** `audit-new`  
**Обновлено:** 2026-08-14 (Phase 2 evidence cross-ref; author ack pending)  
**Scope:** L1 `deposit()` + off-chain ETH→AN deposit pipeline.

Закрытые пункты **не перечисляем** — см. `baseline-deposit-eth-locked.md`.  
Phase 2 partial PoC + CI gates: `phase-2-deposit-eth-milestone.md` — **не** заменяют author sign-off; колонка **Author default** = recommended ack для handoff.

---

## L1 on-chain (`AckiNackiBridge.deposit`)

| ID | PoC | Evidence (partial) | Вопрос к автору | Author default (recommended) |
|----|-----|-------------------|-----------------|------------------------------|
| QC-A1-2 | `FuzzDepositToken.t.sol`, `DepositEdgeCases.t.sol` | TD-23 FoT invariant; TD-24 blacklist mock; TD-56 mainnet fork notes | Trust USDC: proxy upgrade, blacklist/pause Circle — принимаем? | **Ack** — standard ERC-20 USDC for target deployment; document Circle ops risk (`td-56-usdc-mainnet-fork-notes.md`) |
| QC-A1-3 | `EmergencyYield.t.sol`, `EthAuditQcHardening.t.sol` | TD-35 donation path; TD-66 AAVE interleaving | Yield после `emergencyWithdrawAll` — через `skimExcessUsdc`, не `harvestYield` — ок? | **Ack** — `skimExcessUsdc` is intended recovery path |
| QC-A1-4 | `bridge_verification.md` §4 | DEP overlay accounting tests | AC-6 wording + DEP-2/3 USDC sync — ок? | **Ack** — docs wording update (CEI narrative), code unchanged |

---

## Cross-chain (ETH-сторона депозита)

| ID | PoC | Evidence (partial) | Вопрос к автору | Author default (recommended) |
|----|-----|-------------------|-----------------|------------------------------|
| QC-AN-J1 | `DepositWhaleCap.t.sol` | TD-36 cross cap; TD-33 dust spam | ETH cap = `uint64.max`; AN `uint64` mint — совместная min/max policy? | **Ack** — ETH per-tx cap at `uint64.max`; AN per-chain cap via `setMintCap` (TD-04 ops seed) |
| QC-AN-J3 | `DepositNegative.t.sol` | TD-31 anWorkchain; TD-44 event≠PI | ETH `InvalidAnAccount` vs AN без pre-ZK guard — гармонизация? | **Ack** — document asymmetry; AN relies on proof-bound recipient |
| QC-AN-J5 | `DepositEdgeCases.t.sol` | TD-23/24 USDC trust PoCs | USDC custody vs ECC mint — документируем модель? | **Ack** — document L1 custody + AN ECC mint model in ops docs |
| QC-AN-J2 | `DepositPauseAsymmetry.t.sol` | TD-58 pause asymmetry notes | Pause asymmetry; AN finalize permissionless — ок? | **Ack** — no bridge `pause` on `deposit` (#20); token pause external |
| BC-AN-02 | `test_bc_an_02_*`, `integration/test_bc_an_01_*` | TD-04 handoff v2; `eccUSDCBridge` + AN integration **74 passed** (2026-08-14) | Allowlist L1 bridge на AN — блокирует финализацию | **Ack** — allowlist by design; owner seeds **`setTrustedL1Bridge`** post-deploy |

---

## Off-chain (prover + relayer)

| ID | PoC | Evidence (partial) | Вопрос к автору | Author default (recommended) |
|----|-----|-------------------|-----------------|------------------------------|
| QC-OFF-01 | `f10_head_of_line.rs`, `td_26_*` | TD-26 HOL/skip; TD-62 depositId gap | Strict sequential `depositId` + `record_skip` — policy ок? | **Ack** — strict HOL + explicit skip/park policy documented |
| QC-OFF-02 | ops | TD-53 runbook; `deposit-relayer-operator-runbook.md` | Backup relayer / runbook SLA? | **Ops** — define SLA in runbook (no code change) |
| QC-OFF-03 | design | permissionless `finalizeDeposit` in overlay | Permissionless finalize — incentives? | **Ack** — permissionless by design; relayer incentive = ops |
| QC-OFF-04 | ops | — | TLS pinning, key custody? | **Ops** — standard key custody checklist |
| QC-OFF-05 | `f10_interface_reverted.rs`, `td_65_*` | TD-65 stub `is_finalized` always false; mock CI smoke | `is_finalized` read API timeline? | **Defer** — stub documented; nullifier read API when AN exposes it |
| QC-OFF-06 | `f10_competing_submit.rs`, `td_65_g3_revert_loop.rs` | TD-65 **8 tests** + `check_td65_g3_smoke.sh`; TD-37 competing; TD-26 HOL | Live Revert → `Rejected` loop (G3) | **Partial ack** — exit 51 → `AlreadyFinalized` OK; generic Revert → HOL is liveness QC (not BC) |
| QC-OFF-08 | `state.rs`, `td_18_*` | TD-18 state.json durability | `--force-state` policy? | **Ack** — binding enforced; `--force-state` ops-only with checklist |
| QC-OFF-09 | `types.rs`, `td_16_*` | TD-16 prod allowlist; TD-05 dappId | Default `AN_DAPP_ID=0` preflight на CLI? | **Ack** — reject `dapp_id=0`; CLI preflight recommended |
| QC-OFF-10 | `source.rs`, `td_39_*` | TD-39 incremental scan cost | Incremental scan vs O(head) rescan | **Ack** — incremental scan implemented; tail-block QC documented |
| QC-OFF-11 | `td_40_*` | TD-40 CLI backoff/fsync tests | `backoff-multiplier=0`, fsync, Pending timeout | **Ack** — defaults documented; ops tune timeouts |
| QC-OFF-13 | live | TD-63 fetcher parse edge | tvm-sdk receipt parsing coverage | **Defer** — live tvm-sdk matrix; mock edges covered |
| QC-PROV-01 | `f10a_binding.rs`, `circuit_v2.rs` | TD-68 `check_pi_count_docs.sh`; TD-42 VkBlob pin; TD-43 opcode triple | 12 PI pinned; legacy verify paths? | **Partial ack** — 12 PI + opcode path gated; MockProver ≠ production wire |
| QC-PROV-04 | `padding_mutation_poc.rs`, `td_20_*` | TD-20 padding malleability | MPT padding witness malleability — accept? | **Ack** — witness malleability only; no false-deposit path in PoC |
| QC-PROV-05 | `td_43_*`, `check_mock_vs_shplonk_smoke.sh` | TD-43 CI smoke; `PROJECT_FACTS.md` layers | MockProver ≠ SHPLONK | **Ack** — use `verify_deposit_opcode_triple` / opcode example for acceptance |
| QC-PROV-06 | `td_49_*`, `check_mutation_kill_smoke.sh` | TD-49 **KILLED 13/13**; 3 QC survivors documented | Mutation kill score | **Ack** — score pinned; survivors (dappId/promiseCommit/anWorkchain) intentional |

---

## Phase 2 CI gates (2026-08-14)

| Gate | TD |
|------|-----|
| `check_pi_count_docs.sh` | TD-68 |
| `check_vk_srs_pin.sh` | TD-42 |
| `check_mock_vs_shplonk_smoke.sh` | TD-43 |
| `check_mutation_kill_smoke.sh` | TD-49 |
| `td_53_dry_run_recovery_smoke.sh` | TD-53 |
| `check_td65_g3_smoke.sh` | TD-65 |
| `check_an_overlay_patch_matrix.sh` | TD-04 |

Owner deploy readiness (ops, not cluster deploy):

    bash scripts/check_td04_deploy_readiness.sh

Digest: `phase-2-deposit-eth-milestone.md`.

---

## Gates (regression)

    make pre-push-audit
    cd audit/spec/ethereum && FOUNDRY_PROFILE=audit forge test --match-contract Deposit
    cargo test -p deposit-relayer-daemon
    cargo test -p deposit-prover
    bash scripts/check_deposit_audit_gates.sh
    bash scripts/check_td04_deploy_readiness.sh   # optional — owner deploy prep

`check_deposit_audit_gates.sh` runs: TD-68 → TD-42 → TD-43 → TD-49 → TD-53 → TD-65 → TD-04 overlay matrix.

---

## Delta

Новые находки → `delta-deposit-eth.md`. Phase 2 rollup → `phase-2-deposit-eth-milestone.md`.
