# Findings summary

**BC = 0** this pass. **QC = 13** — PoC + auditor view in `closeout-eth.md`; **author confirm pending** (items not deleted).

| ID | Severity | Invariant | PoC | Status |
|----|----------|-----------|------|--------|
| — (A1: BC не найдено) | — | DEP-1..4, AC-1, AC-6, TR-1..4 | покрытие сверено: `FuzzVerifiers.t.sol` (deposit fuzz), `AckiNackiBridgeAave.t.sol`, `AckiNackiBridgePause.t.sol` | A1 закрыт, 0 BC; 3 QC → `questions.md` (QC-A1-1..3) |
| — (A2: BC не найдено) | — | LH-1..9, CC-1..7, BK-1..5, AC-3, AC-6 | покрытие сверено: `AckiNackiBridgeVerifyBlock.t.sol`, `AckiNackiBridgeRelayerLoop.t.sol`, `FuzzAckiNackiBridgeVerifyBlock.t.sol`, `AckiNackiBridgeLayerAnchor.t.sol`, `AckiNackiBridgeApplyBkSetUpdate.t.sol` | A2 закрыт, 0 BC; 4 QC → `questions.md` (QC-A2-1..4); 7 инвариантов → `invariants-extended.md` §A2 |
| — (A3: BC не найдено) | — | WD-1..12 | покрытие сверено: `AckiNackiBridgeWithdrawByProof.t.sol`, `AckiNackiBridgeWithdrawByProofOrder2.t.sol`, `AckiNackiBridgeProductionWithdrawByProof.t.sol`, `AckiNackiBridgePause.t.sol` | A3 закрыт, 0 BC; 4 QC → `questions.md` (WD-Q1..4); 1 Info; 12 инвариантов → `invariants-extended.md` §A3 |
| A3-01 | Medium (QC) | WD-6 | предложен `test_withdrawByProof_evictedAnchor_reverts` (129+ `verifyBlock`) | Open — L1-окно якорей кольцевое (128 = `HISTORY_PROOF_WINDOW`); `finalRoot` старше 128 блоков вытесняется → `withdrawByProof` навсегда reverts `UnknownAnchor`. Ликвидность зависит от возможности перегенерировать пруф под свежий якорь. Средства не теряются (остаются в казне). (WD-Q1) |
| A3-02 | Low (QC) | WD-5 | `test_withdrawByProof_zeroRecipientWorks` (фиксирует текущее поведение) | Open — `recipient == address(0)` принимается контрактом; реальный USDC reverts перевод на нулевой адрес ⇒ tx откатывается, нуллификатор не расходуется, событие вывода становится невыплачиваемым навсегда. Не кража (средства в казне). (WD-Q2) |
| A3-03 | High (QC / известно, Phase 8) | WD-11 | `AckiNackiBridgeProductionWithdrawByProof.t.sol` (SHPLONK-путь) | Open — `BridgeWithdrawalVerifier.sol` (Groth16-адаптер) — R15 identity-stub: по своему NatSpec НЕ проверяет Halo2-пруф. Если этот адаптер попадёт в живой деплой — любой 256-байтный пруф с корректными 10 PI сливает казну (ограничение только `amount ≤ treasuryBalance`). Прод-скрипты подключают `BridgeWithdrawalAggregatorVerifier` (реальный SHPLONK). Нужен deploy-инвариант: stub-адаптер не используется на mainnet. (WD-Q3) |
| A3-04 | Info | OK | — | Noted — `pub.amount == 0` принимается: нулевая выплата расходует нуллификатор и эмитит `WithdrawalByProofExecuted(amount=0)`. Безвредно, но недокументировано. |
| A3-05 | Info (QC) | WD-6 | — | Open — `WITHDRAW_ANCHOR_LAYER` жёстко = 1, связано с партнёрским witness builder (`layer_idx = 0`). Смена якорного слоя партнёром ⇒ все выводы fail-closed (`UnknownAnchor`) — безопасно, но полный halt ликвидности. Комментарий кода упоминает будущий PI-слот [10] `anchorLayer`. (WD-Q4) |
| — (A4: BC не найдено) | — | AC-2,4,5,6, ZK-1..5, OR-1..4 | покрытие сверено: `AckiNackiBridgeAave.t.sol`, `AckiNackiBridgePause.t.sol`, `ShplonkAggregatorForgery.t.sol`, `ShplonkDeployLib.t.sol`, `FuzzVerifiers.t.sol`, `AckiNackiBridgeAaveFork.t.sol` | A4 закрыт, 0 BC; 1 QC → `questions.md` (QC-A4-1); 4 Info (A4-02..05); 7 инвариантов → `invariants-extended.md` §A4 |
| — (A3: BC не найдено) | — | WD-1..12 | покрытие сверено: `AckiNackiBridgeWithdrawByProof.t.sol`, `AckiNackiBridgeProductionWithdrawByProof.t.sol`, `AckiNackiBridgePause.t.sol` | A3 закрыт, 0 BC; 4 QC (A3-01..03,05) → `questions.md` (WD-Q1..4); 12 инвариантов → `invariants-extended.md` §A3 |
| A4-01 | Low (QC) | A4-INV-6 / ZK-2 | `ShplonkDeployLibTest`, `ShplonkAggregatorForgeryTest` | Open — `ShplonkHalo2Verifier.verify` без `extcodesize`; см. A4-Q1 |
| A4-02 | Info (OK) | A4-INV-4 | `AckiNackiBridgePauseTest` | Noted — владелец может держать `pause()` бесконечно, замораживая `withdrawByProof`/`deposit` (централизация); принципал не крадётся. |
| A4-03 | Info (OK) | A4-INV-2/3 | `AckiNackiBridgeAaveForkTest::test_fork_emergencyWithdrawAllPullsAaveDown` | Noted — дубль QC-A1-3(a): после `emergencyWithdrawAll` остаточный yield заперт как over-collateral казны. |
| A4-04 | Info (OK) | AC-5 | — | Noted — docs drift: `blockHeaderOracle` не `immutable` (сеттера нет); AC-5 перечисляет несуществующие `verifier`/`wethGateway`/`aWETH`. |
| A4-05 | Info (OK) | OR-1..4 | `AxiomBlockHeaderOracleTest` | Noted — оракул fail-closed; `isBlockHashAvailable` оптимистичен, `uint32(blockNumber)` truncation; публичной поверхностью не используется. |
| — (F2: BC не найдено) | — | withdraw/admin/TIP-3 | `test_usdcbridge_admin.py`, `test_usdcbridge_withdraw_*.py` | F2 закрыт, 0 BC; QC → `questions-an.md`, cross-chain → `questions-cross-chain.md` |


BC rows must link to a test in `audit/findings/BRIDGE-XXX/`.

## Workstream status

| WS | Reviewed | BC | QC | Report |
|----|----------|----|----|--------|
| A1 Deposits & treasury | 2026-07-16 | 0 | 4 (QC-A1-1..4) | `audit/reports/manual-audit/A1-deposit-treasury.md` |
| A2 verifyBlock & state | 2026-07-16 | 0 | 4 (QC-A2-1..4) | `audit/reports/manual-audit/A2-verify-block.md` |
| A3 withdrawByProof | 2026-07-16 | 0 | 4 (A3-01..03,05) | `audit/reports/manual-audit/A3-withdraw.md` |
| A4 AAVE / verifiers | 2026-07-16 | 0 | 1 (QC-A4-1) | `audit/reports/manual-audit/A4-aave-ac-verifiers.md` |
