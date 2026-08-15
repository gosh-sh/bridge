# Phase G — продолжение аудита на `audit-new`

**База:** `audit-new` (merge `origin/main` + audit overlay, commit `4a29137+`).  
**Канон кода:** `origin/main` (`c9d5412`). **`main` не меняем** — работа только в `audit-new`.

---

## G1.5 — ETH deposit baseline + edge tests (2026-08-13)

| Артефакт | Содержание |
|----------|------------|
| `baseline-deposit-eth.md` | **только open** вопросы (не перечислять закрытые каждый раз) |
| `baseline-deposit-eth-locked.md` | карта тест→инвариант для закрытых пунктов |
| `delta-deposit-eth.md` | новые находки vs июльский аудит |
| `DepositEdgeCases.t.sol`, `DepositWorkchain.t.sol` | +12 edge-case tests |
| `f10a_binding.rs` | 12 PI (QC-PROV-01 partial close) |

---

## G1 — Closeout sync (этот коммит)

Обновлены: `closeout-eth.md`, `closeout-an.md`, `questions-cross-chain.md`, HANDOFF-файлы.

Пункты **resolved in main** помечены; overlay-тесты адаптированы (56 ETH + 74 AN + 68 relayer).

---

## G2 — BC-AN-02 (следующий фокус)

| ID | Тема | Действие |
|----|------|----------|
| BC-AN-02 | нет allowlist L1 `contractAddr` на AN | ждём ack / `immutable EXPECTED_L1_BRIDGE` или документировать trust model |

PoC: `audit/spec/an/integration/test_bc_an_02_no_l1_bridge_allowlist_pre_zk.py`

---

## G3 — QC-OFF-06 live path

| Статус | Деталь |
|--------|--------|
| **partial QC** | `td_65_g3_revert_loop.rs` (8): exit 51 → `AlreadyFinalized`; generic `Reverted` → HOL; `is_finalized` stub; notes `td-65-g3-live-notes.md` |
| mock | `f10_competing_submit.rs` — competing relayers, no double-mint |
| mock | `f10_interface_reverted.rs` — generic Revert → `Rejected` |
| open | shellnet E2E двух relayer'ов (E-AN-01); nullifier read API (QC-OFF-05) |

Нужен: e2e с двумя relayer'ами на shellnet или nullifier read API.

---

## G4 — Author ack (открыто)

**ETH:** QC-A1-2, A1-3, A1-4, A2-1, A2-2, A2-4, WD-Q1, WD-Q3, WD-Q4 (~9).  
**AN:** QC-AN-02, 04, 09; partial 01, 05, 06, 10; joint J1, J3, J5.  
**Off-chain:** QC-OFF-01, 02–06 (частично), QC-PROV-01, QC-PROV-04.

---

## G5 — E-AN-01 shellnet E2E

**deferred** — после G2/G3 или по запросу ops.  
Mock recovery path: TD-53 (`td_53_e2e_runbook_recovery.rs`, `td-53-shellnet-e2e-runbook-notes.md`).

---

## Gates (`audit-new`, 2026-08-13)

| Gate | Результат |
|------|-----------|
| `contracts/ethereum && forge test` | 126 passed |
| `audit/spec/ethereum` profile audit | 69 passed |
| `make pre-push-an` | 42 passed |
| `make audit-an-test` | 74 passed |
| `cargo test` deposit-relayer | 68 passed |

Команда:

    make pre-push-audit

---

## G6 — Phase 2 deposit ETH milestone (2026-08-14)

| Артефакт | Содержание |
|----------|------------|
| `phase-2-deposit-eth-milestone.md` | TD-36–68 rollup, META CI gates, ops blockers, Phase 3 outline |
| `check_deposit_audit_gates.sh` | TD-68 → TD-42 → TD-43 → TD-49 → TD-04 overlay matrix |

**Verdict:** Phase 2 **closed (mock/CI)**; P0 ops carry-over TD-04 live deploy.

---

## Ссылки

| Документ | Назначение |
|----------|------------|
| `closeout-eth.md` | ETH QC register |
| `closeout-an.md` | AN BC/QC + F10 |
| `HANDOFF-an-cross-chain-ru.txt` | RU summary для авторов |
| `HANDOFF-f10-prover-relayer-ru.txt` | off-chain HANDOFF |
| `phase-2-deposit-eth-milestone.md` | Phase 2 TD-36–68 rollup + Phase 3 outline |
