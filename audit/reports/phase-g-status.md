# Phase G / Wave 2 — статус на `audit-new`

**Обновлено:** 2026-08-15  
**HEAD:** `7bd7366+` (Phase 2 deposit-ETH + eccUSDCBridge migration)  
**Канон dev code:** `github/main` @ `a7a1130` — **уже внутри** `audit-new` (merge-base = github tip).  
**Следующий sprint:** `git fetch github` — если появились новые коммиты, merge → `audit-new`.

---

## Deposit-ETH handoff — CLOSED (2026-08-15)

Baseline walkthrough завершён; вопросы переданы авторам. Артефакты:

| Документ | Назначение |
|----------|------------|
| `baseline-deposit-eth.md` / `locked` | open QC vs зелёные тесты |
| `phase-2-deposit-eth-milestone.md` | Phase 2 rollup |
| `deposit-verify-three-tier-backlog.md` | tier 1–3 test expansion |
| `wave-2-security-plan.md` | Wave 2 security scope |

---

## Wave 2 — текущий фокус

| ID | Задача | Статус |
|----|--------|--------|
| T2-1 | Все `deposit_10proofs` → opcode triple | **done** (`td_43_all_fixture_triples_pass_opcode_triple`) |
| T3-2/3 | tvm-debugger opcode + fixtures | backlog |
| W2-1 | ETH `withdrawByProof` security | next baseline |
| Main sync | `fetch github` each sprint | ongoing |

---

## Закрыто в Phase 2 / walkthrough

| ID | Disposition |
|----|-------------|
| BC-AN-02 | Ack — `setTrustedL1Bridge` on `eccUSDCBridge` |
| QC-AN-J1..J5, BC-AN-02 | Ack / ops / defer |
| QC-OFF-01..13, QC-PROV-* | Ack / ops / defer / вопросы devs |
| QC-A1-2..4 | передано авторам |

---

## Открыто (не блокирует Wave 2 старт)

| ID | Тема |
|----|------|
| QC-OFF-05 | `is_finalized` read API — вопрос devs |
| QC-OFF-06 | exit code classifier — вопрос devs |
| QC-OFF-13 | live tvm-sdk matrix — defer |
| TD-04 / E-AN-01 | live deploy / shellnet E2E — ops |
| Author ack | ответы на переданные L1/cross/off-chain вопросы |

---

## Gates

    make pre-push-audit
    bash scripts/check_deposit_audit_gates.sh
    AN_AUDIT_INTEGRATION=1 bash scripts/ci_an_audit.sh

| Gate block | TD |
|------------|-----|
| PI count | TD-68 |
| VkBlob pin | TD-42 |
| Mock≠SHPLONK | TD-43 |
| Mutation kill | TD-49 |
| TD-53 / TD-65 / TD-04 | smoke |

---

## Ссылки

| Документ | Назначение |
|----------|------------|
| `wave-2-security-plan.md` | Wave 2 plan + main sync policy |
| `closeout-eth.md` / `closeout-an.md` | QC registers |
| `HANDOFF-an-cross-chain-ru.txt` | RU handoff для авторов |
