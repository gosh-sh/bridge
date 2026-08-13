# Phase G — продолжение аудита на `audit-new`

**База:** `audit-new` (merge `origin/main` + audit overlay, commit `4a29137+`).  
**Канон кода:** `origin/main` (`c9d5412`). **`main` не меняем** — работа только в `audit-new`.

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
| partial | main #15: `AlreadyFinalized` для exit `0x1000`; mock path зелёный (`f10_competing_submit.rs`) |
| open | live `AnInterfaceSubmitter`: Revert → `Rejected` (`f10_interface_reverted.rs`) |

Нужен: e2e с двумя relayer'ами на shellnet или nullifier read API.

---

## G4 — Author ack (открыто)

**ETH:** QC-A1-2, A1-3, A1-4, A2-1, A2-2, A2-4, WD-Q1, WD-Q3, WD-Q4 (~9).  
**AN:** QC-AN-02, 04, 09; partial 01, 05, 06, 10; joint J1, J3, J5.  
**Off-chain:** QC-OFF-01, 02–06 (частично), QC-PROV-01, QC-PROV-04.

---

## G5 — E-AN-01 shellnet E2E

**deferred** — после G2/G3 или по запросу ops.

---

## Gates (`audit-new`, 2026-08-13)

| Gate | Результат |
|------|-----------|
| `contracts/ethereum && forge test` | 126 passed |
| `audit/spec/ethereum` profile audit | 56 passed |
| `make pre-push-an` | 42 passed |
| `make audit-an-test` | 74 passed |
| `cargo test` deposit-relayer | 68 passed |

Команда:

    make pre-push-audit

---

## Ссылки

| Документ | Назначение |
|----------|------------|
| `closeout-eth.md` | ETH QC register |
| `closeout-an.md` | AN BC/QC + F10 |
| `HANDOFF-an-cross-chain-ru.txt` | RU summary для авторов |
| `HANDOFF-f10-prover-relayer-ru.txt` | off-chain HANDOFF |
