# Manual audit — A1: Deposits & treasury

**Workstream:** A1  
**Subagent model:** Fable 5  
**Files:** `contracts/ethereum/src/AckiNackiBridge.sol` (deposit, treasuryBalance, USDC/AAVE accounting)  
**Invariants:** DEP-1..4, AC-1, AC-6  
**Scope note:** ZK-верификаторы — trust boundary; soundness Halo2 вне scope. `withdrawByProof`/`verifyBlock` рассмотрены только в части влияния на treasury-учёт (полный разбор — A2/A3).

## Checklist

- [x] Entry: `deposit(uint256 amount, int8 anWorkchain, bytes32 anAccount)` — L579–595
- [x] `Deposit` event fields match state — L594: `depositId` = `depositCounter` до инкремента (L591, post-increment), `sender = msg.sender`, `amount`, `anWorkchain`, `anAccount`, `block.timestamp`. Поля события согласованы с состоянием. **OK**
- [x] `treasuryBalance` accounting vs `usdc.balanceOf` — см. анализ ниже (A1-F3..F5)
- [x] `MAX_DEPOSIT_AMOUNT`, zero amount, zero account — L584–586: `InvalidAmount` / `DepositTooLarge` / `InvalidAnAccount`. Per-call cap обходится дроблением (A1-F2)
- [x] Reentrancy surface — единый guard `_reentrancyStatus` на всех mutating entrypoints (`deposit`, `verifyBlock`, `applyBkSetUpdate`, `withdrawByProof`, `supplyToAave`, `withdrawFromAave`, `emergencyWithdrawAll`, `harvestYield`). Незащищённые функции (`pause`, `unpause`, `setAaveEnabled`, `setLiquidReserveBps`, `setYieldRecipient`, `transferOwnership`) — `onlyOwner`, без внешних вызовов. Mainnet USDC (FiatTokenProxy) не имеет transfer-хуков. Cross-function reentrancy закрыта общим guard. **OK**
- [x] Pause interaction — `deposit` под `whenNotPaused` (L582); owner-only AAVE-функции намеренно не гейтятся паузой (эвакуация средств). Покрыто `AckiNackiBridgePause.t.sol`. Перманентная пауза = заморозка средств пользователей (centralization, A1-F6)
- [x] SWC-107 (reentrancy) — закрыто guard'ом; см. также A1-F1 (порядок interaction/effects)
- [x] SWC-105 (unprotected withdrawal) — все пути вывода USDC: `withdrawByProof` (proof-gated + nullifier), `harvestYield` (onlyOwner, только yield поверх principal). Произвольного owner-вывода principal нет: `emergencyWithdrawAll` возвращает средства **на контракт**, не owner'у. **OK**
- [x] SWC-132 (unexpected balance) — `_amountSupplyable` (L1223–1228) и `accruedYield` (L1258) используют сырые `balanceOf`; донаты USDC/aUSDC меняют поведение, но не создают потерь пользователей (A1-F4)

## Карта entrypoint `deposit` (caller → state → external calls)

| Шаг | Что | Строки |
|---|---|---|
| Checks | `amount != 0`, `amount <= 100e6`, `anAccount != 0`; `nonReentrant`, `whenNotPaused` | 579–586 |
| Interaction | `usdc.transferFrom(msg.sender, this, amount)` — **до** effects | 587–589 |
| Effects | `depositCounter++`, `treasuryBalance += amount` | 591–592 |
| Event | `Deposit(...)` | 594 |

Reentrancy surface: единственный внешний вызов — `transferFrom` на immutable `usdc`. Для mainnet USDC хуков нет; guard блокирует повторный вход во все mutating-функции.

## Findings

| ID | Sev | BC/QC/OK | Summary | PoC |
|----|-----|----------|---------|-----|
| A1-F1 | Info | QC (docs) | `deposit()` L587–592: interaction (`transferFrom`) **до** effects — противоречит букве **AC-6** («state mutation precedes external call», `bridge_verification.md` §9.1). Митигировано `nonReentrant` + USDC без колбэков; сам паттерн «pull-then-account» корректен. Аналогично `emergencyWithdrawAll` (L1141: withdraw до `suppliedPrincipal = 0`, L1145) и `harvestYield` (вообще без state-effects). Требуется правка формулировки AC-6, не кода | n/a (docs) |
| A1-F2 | Low | QC | `MAX_DEPOSIT_AMOUNT` (L59, комментарий «prevents whale deposits») — лимит **per-call**, суммарный депозит не ограничен: N вызовов по 100 USDC обходят cap полностью (нет per-address и глобального cap). Если intent — ограничить TVL/exposure milestone'а, лимит не работает | pending (тривиально: цикл deposit ×N) |
| A1-F3 | Low | QC | Trust-модель USDC не зафиксирована: mainnet USDC — upgradeable proxy (Circle может добавить fee-on-transfer → `treasuryBalance += amount` при фактическом получении < amount ⇒ скрытая неплатёжеспособность, убыток последним выводящим); blacklist/pause бриджа Circle'ом = полная заморозка deposit/withdraw/AAVE-эвакуации. Код корректен для текущего USDC; assumption надо в `PROJECT_FACTS.md` | n/a (trust assumption) |
| A1-F4 | Info | OK | Донат USDC напрямую на контракт: `_amountSupplyable` (L1223–1228) считает от сырого `balanceOf`, поэтому донат может быть отправлен в AAVE и засчитан в `suppliedPrincipal` — он не станет ни yield (`accruedYield = aUSDC.balanceOf − suppliedPrincipal`), ни выводимым (payout ограничен `treasuryBalance`). Донат заперт навсегда = дополнительный solvency-буфер. Потери пользователей нет. Донат aUSDC инфлирует `accruedYield`, но harvest сжигает донат-aUSDC — principal не затрагивается | n/a |
| A1-F5 | Low | QC | `emergencyWithdrawAll` (L1136–1149): (а) накопленный yield приходит на контракт как liquid USDC и после `suppliedPrincipal = 0` не может быть ни выведен через `harvestYield` (yield считается только от aUSDC), ни изъят owner'ом — заперт как в A1-F4; (б) при недостаче AAVE (`received < principal`) `treasuryBalance` не уменьшается ⇒ переучёт активов; убыток ложится на последних выводящих (`WithdrawTransferFailed`/`WithdrawTreasuryShortfall`), механизма социализации потерь нет. Принято ли это как residual risk? | pending |
| A1-F6 | Info | OK | Централизация: owner может (а) поставить перманентную паузу — deposit/withdrawByProof заморожены навсегда; (б) `transferOwnership` без 2-step. Украсть principal owner не может (см. SWC-105 выше). Стандартный residual risk для milestone; документировать | n/a |
| A1-F7 | Info | OK | `anWorkchain` не валидируется (любой int8), `anAccount` — только non-zero: опечатка в AN-адресе сжигает средства депозитора. Валидация на EVM-стороне невозможна (нет чексуммы TVM-адреса); ответственность UX/relayer | n/a |
| A1-F8 | Info | OK | `IERC20.transferFrom` требует возврата bool (L587). USDC возвращает `true` — ок; non-standard-токены (USDT-стиль без return) — deposit fail-closed (revert на decode), потерь нет. `approve` non-zero→non-zero (L1116) для USDC допустим; allowance к immutable AAVE pool потребляется полностью | n/a |

**Итог по drift `treasuryBalance` vs фактические активы:** в нормальном режиме (USDC без fee, AAVE платёжеспособен) выполняется `usdc.balanceOf(bridge) + suppliedPrincipal ≥ treasuryBalance`; строгое равенство += только через донаты/yield-остатки. Уменьшение `treasuryBalance` — единственный путь `withdrawByProof` (L1060, до внешних вызовов, CEI соблюдён); увеличение — только `deposit` (L592). AAVE-функции `treasuryBalance` не трогают (проверено по всем записям в storage). Ни одного пути unauthorized-уменьшения/кредита не найдено ⇒ **BC: 0**.

**Проверка тестового покрытия (не «зеленил», только сверка):** DEP-1..3 покрыты `FuzzAckiNackiBridgeDepositTest` (`testFuzz_DepositAmountInvariants`, `testFuzz_DepositInvalidAmountReverts`, `testFuzz_MultipleDepositsInvariant` — FuzzVerifiers.t.sol L166–257); pause-гейт — `AckiNackiBridgePause.t.sol::test_deposit_blockedWhilePaused`; AAVE-учёт — `AckiNackiBridgeAave.t.sol` (в т.ч. `testFuzz_totalAssetsCoversTreasury`). Пробелы: нет negative-теста `anAccount == 0` (DEP-1), нет solvency-invariant handler'а (deposit+supply+withdraw interleaving), нет теста на A1-F4/F5 (донаты, emergency-yield). → фаза C/D, см. инварианты ниже.

## New invariants proposed

(дублируются в `audit/reports/invariants-extended.md`, раздел A1)

| ID | Statement |
|----|-----------|
| TR-1 | Solvency: `usdc.balanceOf(bridge) + suppliedPrincipal >= treasuryBalance` после любой последовательности `deposit`/`supplyToAave`/`withdrawFromAave`/`withdrawByProof`/`harvestYield`/`emergencyWithdrawAll` (при честном AAVE) |
| TR-2 | Conservation: `treasuryBalance == Σ deposit.amount − Σ withdrawByProof.amount`; никакая AAVE/owner-функция не изменяет `treasuryBalance` |
| TR-3 | `suppliedPrincipal <= aUSDC.balanceOf(bridge)` (при честном AAVE); `harvestYield` не уменьшает `aUSDC.balanceOf` ниже `suppliedPrincipal` |
| TR-4 | После `deposit(amount)`: `Δusdc.balanceOf(bridge) == amount` точно (ловит fee-on-transfer при смене token-assumptions, A1-F3) |
| DEP-5 | `depositCounter` строго +1 на каждый успешный `deposit`; `depositId` в событии равен значению счётчика до инкремента; счётчик никогда не уменьшается |
| DEP-6 | `deposit()` изменяет ровно три слота состояния: `depositCounter`, `treasuryBalance`, баланс USDC контракта — и ничего из verifyBlock/withdraw-состояния |
| PS-1 | `deposit` (и все user-facing entrypoints) revert с `BridgePaused` ⟺ `paused == true`; owner-функции AAVE/паузы работают независимо от `paused` |
