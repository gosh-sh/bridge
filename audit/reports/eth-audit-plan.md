# План аудита — Ethereum-контракты (ветка `audit`)

**Приоритет:** безопасность средств. Любая потеря USDC/treasury — катастрофа.  
**ZK-схемы:** вне scope (уже аудированы партнёром). Контракты трактуют верификаторы как **trust boundary** — проверяем wiring, CEI, accounting, replay, access control, но не soundness Halo2.

**Методология:** ammalgam + dex + [Consensys Smart Contract Best Practices](https://consensys.github.io/smart-contract-best-practices/) + [SWC Registry](https://swcregistry.io/) + bridge-specific (nullifiers, cross-circuit binding, treasury isolation).

**Артефакты:** `audit/findings/BRIDGE-XXX/`, реестры `audit/reports/findings-summary.md`, `audit/reports/questions.md`.  
**Инварианты:** `docs/operations/bridge_verification.md` (DEP-, LH-, CC-, AC-, OR-, ZK-#).

### Статус (2026-07-16)

| Фаза | Статус |
|------|--------|
| A — ручной аудит | ✅ A1–A4, BC=0 |
| B — test matrix | ✅ `audit/reports/test-matrix.md` |
| C — unit | ✅ 29 тестов |
| D — fuzz/invariant | ✅ 11 тестов |
| E — E2E gaps | ✅ 7 новых + covered (47 total @ audit profile) |

---

## 0. Подготовка (1 день)

| Шаг | Действие |
|-----|----------|
| 0.1 | Зафиксировать commit SHA и `forge test` baseline (`cd contracts/ethereum && forge test`) |
| 0.2 | Прочитать `AckiNackiBridge.sol` + `docs/architecture/four_circuit_architecture.md` |
| 0.3 | Составить карту entry points (см. §1) — чеклист для ручного аудита |
| 0.4 | Создать `audit/reports/manual-audit/` — по одному файлу на workstream (шаблон ниже) |

### Карта scope (in-scope)

| Модуль | Файлы | Критичность |
|--------|-------|-------------|
| **Core bridge** | `AckiNackiBridge.sol` | **Critical** — custody, verifyBlock, withdrawByProof, AAVE (no bridge `pause` — #20) |
| **Verifier adapters** | `PrimaryVerifier`, `FallbackAggregatorVerifier`, `LayerHashesMovementVerifier`, `BridgeWithdrawalVerifier` | **High** — PI layout, proof length, try/catch |
| **SHPLONK aggregators** | `*AggregatorVerifier.sol`, `ShplonkAggregatorVerifierBase.sol` | **High** — production path; mock bypass = fund loss |
| **Oracle** | `AxiomBlockHeaderOracle.sol` | Medium — сейчас не на hot path |
| **Low-level ZK libs** | `Blake2b*`, `Halo2Verifier.sol` | Low — smoke/regression only |
| **Deploy scripts** | `script/Deploy*.s.sol` | Medium — misconfiguration |

**Out of scope:** soundness partner circuits, relayer daemons (byzantine relayer не может украсть — только stall).

**Deposit off-chain verification (TD-43):** MockProver / `verify_proof` struct **не** заменяют SHPLONK opcode triple. CI smoke: `check_mock_vs_shplonk_smoke.sh`; opcode path: `verify_deposit_opcode_triple`. Не полагаться на mock-only green при аудите deposit-prover / relayer. См. `audit/PROJECT_FACTS.md` § verification layers, `audit/reports/td-43-mock-vs-shplonk-notes.md`.

---

## 1. Фаза A — Ручной аудит через субагентов

### Политика моделей

| Задача | Модель субагента |
|--------|------------------|
| **Ручной code review** (эта фаза) | **Fable 5** или **GPT 5.6** |
| Все остальные субагенты (тесты, CI, docs, sync) | **Composer 2.5** (не Fast) |

### Принципы ручного аудита

1. **Не «зеленить»** — каждое замечание классифицировать BC / QC / OK до правок кода.
2. **PoC обязателен и для BC, и для QC** — различие не в «проверяли / не проверяли», а в **выводе**:
   - **QC** — воспроизвели, понимаем механизм, но **не можем** решить bug vs feature без подтверждения intent (продукт / протокол / партнёр).
   - **BC (bug candidate)** — PoC показывает поведение, которое **по нашим допущениям не должно** быть; не называем confirmed bug из уважения к контексту разработчиков.
   - Без PoC — не QC и не BC; остаётся гипотеза или «BC pending» до red test.
3. Фокус на **fund loss**, **unauthorized mint/payout**, **replay**, **accounting drift**, **pause/owner abuse**.
4. Для каждого external/public — таблица: caller → state read/write → external calls → reentrancy surface.

### Workstreams (параллельно, 4 субагента)

#### A1 — Deposits & treasury (`deposit`, accounting, USDC)

**Файл:** `audit/reports/manual-audit/A1-deposit-treasury.md`

Проверить:

- DEP-1..4, AC-1, AC-6
- `treasuryBalance` vs реальный `usdc.balanceOf` — drift при AAVE routing
- `MAX_DEPOSIT_AMOUNT`, zero amount, `anAccount == 0`
- Fee-on-transfer / weird ERC-20 (USDC на mainnet — pause/blacklist — QC для Sepolia mock)
- Rounding при 6 decimals
- Interaction с bridge pause — **нет** (#20); token pause/blacklist external (TD-58)

**SWC-фокус:** SWC-107 (reentrancy), SWC-105 (unprotected ether/token), SWC-132 (unexpected balance).

#### A2 — `verifyBlock` & state machine

**Файл:** `audit/reports/manual-audit/A2-verify-block.md`

Проверить:

- LH-1..9, CC-1..7, AC-3, AC-6
- CEI: verifiers до storage writes; state при revert crypto
- Replay: `blockSeqNo`, `prevMaxLevelLayerHash`, `knownAnchors`
- `finType` Primary vs Fallback — wrong type + valid-looking proofs
- `VerifyBlockDisabled`, zero verifiers
- Fast-forward / gap в seqNo (relayer может пропустить блок — QC)
- `applyBkSetUpdate` / Circuit 3 slot (BK-1..5) если включён

**Attack scenarios:** CC-1..7 из `docs/operations/manual_verification_runbook.md` Phase J.

#### A3 — `withdrawByProof` & nullifiers

**Файл:** `audit/reports/manual-audit/A3-withdraw.md`

Проверить (контракт — **не** circuit):

- `finalRoot ∈ knownAnchors` — anchor не из verifyBlock → reject
- Nullifier replay (`usedNullifiers` / mapping)
- `dstChainId == block.chainid`
- Recipient reconstruction (80+80 bit halves) — truncation, zero address
- `tokenId`, amount vs treasury / USDC balance
- Treasury shortfall path — partial pay vs revert
- Strict forwarding публичных inputs в verifier (MockBridgeWithdrawalVerifier strict mode)
- Interaction: withdraw до первого verifyBlock (no bridge pause gate)

**SWC-фокус:** SWC-120 (authorization), SWC-114 (tx order), double-spend.

#### A4 — AAVE, owner, pause, verifiers, oracle

**Файл:** `audit/reports/manual-audit/A4-aave-ac-verifiers.md`

Проверить:

- AC-2, AC-4, AC-5 — owner не трогает principal
- `liquidReserveBps` cap, `supplyToAave` / `withdrawFromAave` / `emergencyWithdrawAll`
- `harvestYield` — yield vs principal isolation (`accruedYield`)
- Pause: **removed (#20)** — document token pause + relayer ops only
- Verifier adapters: ZK-1..5 — proof length, address(0), try/catch swallow
- Production vs mock verifiers в deploy scripts
- `AxiomBlockHeaderOracle` — fail-closed, immutables

### Шаблон отчёта workstream

```markdown
## Finding BRIDGE-XXX (draft)
- Severity: Critical / High / Medium / Low / Info
- Invariant: CC-3 / custom
- Location: AckiNackiBridge.sol:Lnnn
- Description: ...
- Fund loss?: Yes/No — scenario ...
- Classification: BC / QC / OK
- PoC: pending / audit/findings/BRIDGE-XXX/test.t.sol
- Recommendation: ...
```

### Выход фазы A

| Артефакт | Содержание |
|----------|------------|
| `audit/reports/manual-audit/A1..A4.md` | Полные замечания |
| `audit/reports/findings-summary.md` | Только BC с severity |
| `audit/reports/questions.md` | QC для партнёра/команды |
| `audit/reports/invariants-extended.md` | **Новые** инварианты от аудиторов (WD-#, TR-#, …) |

**Gate:** lead reviewer сводит дубликаты, выставляет priority (P0 = fund loss), блокирует фазу B.

---

## 2. Фаза B — План тестирования (на основе фазы A)

Синтез: каждый **P0/P1 BC** → минимум один тест; каждый **инвариант без покрытия** → строка в test matrix.

См. полная matrix: **`audit/reports/test-matrix.md`** (22 unit + 8 fuzz + 4 E2E gaps).

### Test matrix (шаблон — см. полный файл)

| ID | Invariant | Тип | Файл | Статус |
|----|-----------|-----|------|--------|
| U-DEP-01 | DEP-3 treasury += amount | unit | `audit/spec/ethereum/DepositAccounting.t.sol` | todo |
| F-CC-03 | CC-3 bkSet == stored | fuzz | `audit/spec/ethereum/InvariantsVerifyBlock.t.sol` | todo |
| … | … | … | … | … |

### Три компонента (строгий порядок)

```
Фаза C — Unit tests        → быстрые, детерминированные, один concern на тест
Фаза D — Fuzz / invariant  → forge fuzz + handler-based invariants
Фаза E — E2E               → только после зелёного C+D; real proofs + fork
```

---

## 3. Фаза C — Unit-тесты

**Размещение:** `audit/spec/ethereum/` (новые); расширение существующих suite'ов — только с тегом инварианта в комментарии.

### C1 — Deposits & treasury

- DEP-1..4 каждый — positive + negative
- Accounting: `treasuryBalance` после N deposits; после `supplyToAave` principal unchanged
- Edge: `MAX_DEPOSIT_AMOUNT`, `MAX+1`, amount=0, `anAccount=0`
- Pause asymmetry: `DepositPauseAsymmetry.t.sol` — no `BridgePaused` on deposit

### C2 — verifyBlock (mock verifiers)

- Reuse pattern из `AckiNackiBridgeRelayerLoop.t.sol` — 10-block loop
- Каждый revert reason LH/CC — отдельный test (уже частично в `AckiNackiBridgeVerifyBlock.t.sol`)
- **Новые из manual audit:** state unchanged on crypto reject (CEI)
- `applyBkSetUpdate` если в scope

### C3 — withdrawByProof

- Extend `AckiNackiBridgeWithdrawByProof.t.sol` + strict-pub mock
- Nullifier double-spend
- Unknown anchor, wrong chainId, wrong tokenId
- Treasury insufficiency
- Recipient edge cases (zero hi/lo)

### C4 — AAVE & owner

- Mock path: `AckiNackiBridgeAaveTest` patterns
- Owner cannot withdraw user principal
- `liquidReserveBps` boundaries (0, MAX, MAX+1)

### C5 — Verifier adapters (boundary only)

- Wrong proof length → false, not revert
- Zero address constructor revert (ZK-4)
- Не дублировать crypto soundness — только adapter contract

**Критерий готовности:** все P0/P1 BC имеют red test или уже воспроизведены; `forge test --match-path "audit/spec/ethereum/*"`.

---

## 4. Фаза D — Fuzzing & invariant (Forge)

### D0 — Конфигурация

```toml
# foundry.toml — профиль audit (добавить)
[profile.audit]
test = "audit/spec/ethereum"
fuzz = { runs = 256 }           # локально
invariant = { runs = 128, depth = 50 }
```

CI/night: `FOUNDRY_PROFILE=ci` (5000 fuzz / 1000 invariant) — как в ammalgam `pruvendo_night`.

### D1 — Базовые инварианты (из проекта)

Источник: `docs/operations/bridge_verification.md`.

| Handler | Инварианты |
|---------|------------|
| `VerifyBlockHandler` | LH-4,5,6, CC-3,5,6,7; state monotonicity |
| `DepositHandler` | DEP-1,3; treasury monotonic on deposit |
| `WithdrawHandler` | nullifier ⊆ used; payout ≤ treasury |
| `TreasuryHandler` | `usdc.balanceOf >= treasuryBalance - suppliedPrincipal` (с учётом AAVE) |

Расширить `FuzzAckiNackiBridgeVerifyBlock.t.sol` → вынести в `audit/spec/ethereum/InvariantsVerifyBlock.t.sol` с `invariant_` prefix.

### D2 — Инварианты от аудиторов

Из `audit/reports/invariants-extended.md` — каждый → `invariant_*` или fuzz test с комментарием `// INV: WD-01`.

### D3 — Антипаттерны (ammalgam)

- Запрещено: `vm.assume()` на >95% пространства → `bound()`
- Запрещено: silent `catch {}` в тестах
- После прогона: revision — какие counterexamples нашли BC?

### D4 — Security-focused fuzz campaigns

| Campaign | Property |
|----------|----------|
| **Solvency** | sum(deposits) - sum(withdrawals) ≤ USDC balance + aUSDC principal |
| **Replay** | same nullifier / seqNo never succeeds twice |
| **Access** | non-owner never calls owner functions |
| **Pause** | **No bridge pause (#20)**; USDC token pause external |

**Критерий готовности:** `FOUNDRY_PROFILE=audit forge test` green; counterexamples задокументированы или → BC.

---

## 5. Фаза E — E2E (последний этап)

Только когда C+D закрыты (или P0 явно deferred с QC).

### E1 — Real-proof paths (уже частично есть)

| Suite | Что доказывает |
|-------|----------------|
| `AckiNackiBridgeVerifyBlock.t.sol` | Bound 1A+2 через real Groth16 adapters |
| `AckiNackiBridgeProductionVerifyBlock.t.sol` | R15 SHPLONK production verifiers |
| `AckiNackiBridgeProductionWithdrawByProof.t.sol` | Circuit 4 production path |
| `PrimaryVerifier.t.sol`, `LayerHashesMovementVerifier.t.sol`, `FallbackVerifier.t.sol` | Per-circuit adapters |

**Задача аудита:** добавить **negative E2E** — malformed proof bytes, wrong PI count, swapped blockId между proofs.

### E2 — Multi-block relayer loop

- `AckiNackiBridgeRelayerLoop.t.sol` — 10 blocks; расширить до 50+ при findings
- Restart-from-anchor (`bridge-relayer-daemon` mirror) — optional Rust integration

### E3 — Fork E2E (opt-in)

```bash
FOUNDRY_PROFILE=fork FORK_URL=$RPC forge test --match-contract Fork
```

- `AckiNackiBridgeAaveFork.t.sol` — real AAVE V3
- Pre-deploy: verify immutables on Sepolia (`cast call`)

### E4 — Deploy smoke

- `DeployRealBridge.s.sol` / `DeployShellnetE2EBridge.s.sol` — dry-run `forge script`
- Checklist L5 из `bridge_verification.md`

**Критерий готовности:** E2E matrix in `audit/reports/closeout-eth.md` — P0 scenarios pass or documented as QC with PoC.

---

## 6. Security checklist (bridge-specific)

Обязательный проход **каждым** субагентом A1–A4:

| # | Check |
|---|-------|
| S1 | **Reentrancy** — все mutating paths; CEI order |
| S2 | **Integer** — overflow impossible (0.8.x); rounding on 6 decimals |
| S3 | **Access control** — immutables vs owner; permissionless surfaces |
| S4 | **Replay** — depositId (AN), nullifier, blockSeqNo |
| S5 | **Oracle / anchor** — stale anchor, fake finalRoot |
| S6 | **Verifier bypass** — adapter returns true on garbage; wrong proof length |
| S7 | **Denial of service** — unbounded loops, griefing (bridge stall OK, fund lock NOT) |
| S8 | **Centralization** — owner AAVE/yield paths (no bridge pause #20) |
| S9 | **Cross-function** — deposit + supplyToAave + withdrawByProof ordering |
| S10 | **Token assumptions** — USDC pause/blacklist (document trust model) |

---

## 7. Timeline (ориентир)

| Неделя | Фаза | Deliverable |
|--------|------|-------------|
| 1 | A0 + A1–A4 | Manual reports, findings-summary |
| 2 | B + C | Test matrix, unit PoCs для P0 |
| 3 | D | Invariant suite, fuzz campaigns |
| 4 | E | E2E signoff, audit closeout memo |

---

## 8. Фаза F — AN-контракты (позже)

**Trigger:** после signoff ETH + tooling от Вас.

Ожидаемая аналогия:

| ETH | AN (acki-nacki) |
|-----|-----------------|
| Foundry | `sold` + pytest + `tvm-debugger` |
| `audit/spec/ethereum/` | `audit/spec/an/` |
| Fuzz invariant | Hypothesis / integration pipeline |
| E2E | shellnet + local 5-node cluster |

ZK opcode (`ZKHALO2VERIFYWITHVK`) — **smoke only** (trust boundary); глубокий аудит не повторяем.

---

## 9. Closeout

- [x] `audit/reports/closeout-eth.md` — QC register + auditor views
- [x] Test matrix phases C–E covered
- [x] CI `test:solidity:audit` + `make pre-push-audit`
- [x] Docs fixes (QC-A1-4, QC-A2-1) — author ack pending
- [ ] QC disposition (13 items) — author + agent
- [ ] E-07 post-deploy immutables — **deferred** (manual)
- [ ] Phase F AN — **deferred**

**Deferred explicitly:** fork E2E (E-05), shellnet E2E, AN contracts (§8).

---

## Приложение — существующее покрытие (baseline)

~174 tests / 21 suites в `contracts/ethereum/test/`:

- `AckiNackiBridgeWithdrawByProof.t.sol` (27) — withdraw
- `AckiNackiBridgeVerifyBlock.t.sol` (17) — real proofs verifyBlock
- `AckiNackiBridgeAaveTest.t.sol` (21) — AAVE mock
- `DepositPauseAsymmetry.t.sol` (TD-58); `AckiNackiBridgePause.t.sol` **removed** (#20)
- `FuzzAckiNackiBridgeVerifyBlock.t.sol` (7) — pre-crypto fuzz
- `AckiNackiBridgeRelayerLoop.t.sol` (6) — multi-block mock

**Gap (baseline at plan start; overlay now covers):** solvency invariant handler, withdraw fuzz, cross-circuit negatives, relayer loop E2E — see `audit/reports/test-matrix.md` closeout table.

**Still deferred:** fork AAVE E2E (E-05), E-07 post-deploy immutables, Phase F AN.
