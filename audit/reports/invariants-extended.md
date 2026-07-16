# Расширенные инварианты (от аудиторов)

Новые инварианты, предложенные в фазе A ручного аудита. Каждый должен получить
`invariant_*` / fuzz-тест в фазе D (`audit/spec/ethereum/`) с комментарием `// INV: <ID>`.

**A3 (WD-1..12):** см. `audit/reports/manual-audit/invariants-extended.md` — withdraw/nullifiers (покрытие в main tree + audit overlay).

## A1 — Deposits & treasury (TR-#, DEP-#, PS-#)

Источник: `audit/reports/manual-audit/A1-deposit-treasury.md`.

| ID | Statement | Проверка (фаза D) |
|----|-----------|-------------------|
| TR-1 | Solvency: `usdc.balanceOf(bridge) + suppliedPrincipal >= treasuryBalance` после любой последовательности `deposit` / `supplyToAave` / `withdrawFromAave` / `withdrawByProof` / `harvestYield` / `emergencyWithdrawAll` (при честном AAVE-моке) | handler-based invariant (`TreasuryHandler`) |
| TR-2 | Conservation: `treasuryBalance == Σ deposit.amount − Σ withdrawByProof.amount`; AAVE- и owner-функции (`supplyToAave`, `withdrawFromAave`, `emergencyWithdrawAll`, `harvestYield`, сеттеры) не изменяют `treasuryBalance` | ghost-переменные в handler'е |
| TR-3 | `suppliedPrincipal <= aUSDC.balanceOf(bridge)` (при честном AAVE); `harvestYield` никогда не опускает `aUSDC.balanceOf` ниже `suppliedPrincipal` (изоляция principal от yield) | invariant + unit на границе `amount == accruedYield()` |
| TR-4 | После успешного `deposit(amount)`: `Δusdc.balanceOf(bridge) == amount` точно (детектор fee-on-transfer при смене token-assumptions — QC-A1-2) | fuzz unit |
| DEP-5 | `depositCounter` строго +1 на каждый успешный `deposit`; `depositId` в событии == счётчик до инкремента; счётчик монотонен | unit + event assert |
| DEP-6 | `deposit()` изменяет ровно три величины: `depositCounter`, `treasuryBalance`, USDC-баланс контракта; verifyBlock/withdraw/AAVE-состояние (`storedLastSeenBlockSeqNo`, `suppliedPrincipal`, `_nullifiers`, окна слоёв) не затронуто | state-diff unit |
| PS-1 | User-facing entrypoints (`deposit`, `verifyBlock`, `applyBkSetUpdate`, `withdrawByProof`) revert с `BridgePaused` ⟺ `paused == true`; owner-функции (AAVE-управление, сеттеры, pause/unpause) работают независимо от `paused` | fuzz по (paused, entrypoint) |

Пробелы существующего покрытия, закрываемые этими инвариантами: negative-тест `anAccount == 0` (DEP-1),
interleaving deposit+supply+withdrawByProof (TR-1/TR-2), донат-сценарии и `emergencyWithdrawAll`-yield (QC-A1-3).

## A2 — verifyBlock & state machine (A2-INV-#)

Источник: `audit/reports/manual-audit/A2-verify-block.md`.

| ID | Statement | Проверка (фаза D) |
|----|-----------|-------------------|
| A2-INV-1 | При любом revert verifyBlock все `stored*` и `_layerWindows` не изменяются (CEI) | unit: crypto reject + state snapshot |
| A2-INV-2 | Значения в `_layerWindows[L]` только из успешного Circuit-2-bound verifyBlock | integration + mock reject |
| A2-INV-3 | withdraw принимает finalRoot только из последних ≤128 хешей слоя 1 | unit + fuzz window eviction |
| A2-INV-4 | `finType` в event == принявший verifier; cross-type proof rejected | unit Primary vs Fallback |
| A2-INV-5 | Seq gap допустим контрактом, но anchor binding запрещает пропуск реального блока | relayer loop + QC doc |
| A2-INV-6 | applyBkSetUpdate не реплеится (dual cursor + oldCommitment gate) | unit replay after rotation |
| A2-INV-7 | newCommitmentL3 связан с blockId через SHA256 Merkle + old-committee attestation | unit applyBkSetUpdate negatives |

## A4 — AAVE / owner / pause / verifiers / oracle (A4-INV-#)

Источник: `audit/reports/manual-audit/A4-aave-ac-verifiers.md`.

| ID | Statement | Проверка (фаза D) |
|----|-----------|-------------------|
| A4-INV-1 | Для любого owner-достижимого состояния `totalAssets() >= treasuryBalance` (solvency); ни один owner-путь не уменьшает `treasuryBalance` и не переводит principal на EOA | handler-based invariant (owner-ops handler); пересекается с TR-1/TR-2 |
| A4-INV-2 | После `harvestYield(amount)` по-прежнему `aUsdcBalance() >= suppliedPrincipal` (principal полностью обеспечен; harvest ограничен `accruedYield()`) | unit на границе `amount == accruedYield()` + fork-тест |
| A4-INV-3 | `suppliedPrincipal` уменьшается только через `_pullFromAave` / `emergencyWithdrawAll`; USDC уходит с контракта на не-AAVE адрес только как yield (`harvestYield`) или verified payout (`withdrawByProof`) | ghost-трекинг переводов в handler'е |
| A4-INV-4 | При `paused`: `deposit` / `verifyBlock` / `applyBkSetUpdate` / `withdrawByProof` ревертят `BridgePaused`; `supplyToAave` / `withdrawFromAave` / `emergencyWithdrawAll` / `harvestYield` остаются owner-callable | fuzz по (paused, entrypoint); расширяет PS-1 |
| A4-INV-5 | Каждый AN→ETH aggregator-адаптер возвращает `true` только если `proof.length >= (12 + NUM_INNER)*32`, re-exposed instances `[12..]` равны публичным входам от бриджа, **и** Yul SHPLONK verifier принял `instances‖proof` | negative-тесты `ShplonkAggregatorForgery.t.sol` + fuzz length/instance tampering |
| A4-INV-6 | `ShplonkHalo2Verifier.verify` возвращает `true` только когда целевой Yul verifier имеет непустой код и не ревертит — сейчас **не enforced** (QC-A4-1): `staticcall` на адрес без кода даёт `ok == true` | unit: wrapper на EOA/пустой адрес должен отвергать; ждёт решения QC-A4-1 |
| A4-INV-7 | `AxiomBlockHeaderOracle.getBlockHash` fail-closed: revert для future block, для historical без witness и при нулевом `blockhash()` | unit negatives (OR-1..OR-3) |
