# Manual audit — A2: verifyBlock & state machine

**Workstream:** A2
**Subagent model:** Fable 5
**Files:** `AckiNackiBridge.sol` (verifyBlock, knownAnchors, applyBkSetUpdate),
`PrimaryVerifier.sol`, `FallbackAggregatorVerifier.sol`, `LayerHashesMovementVerifier.sol`,
`PrimaryAggregatorVerifier.sol`, `LayerHashesAggregatorVerifier.sol`, `ShplonkAggregatorVerifierBase.sol`
**Invariants:** LH-1..9, CC-1..7, BK-1..5, AC-3, AC-6
**Scope note:** soundness самих Halo2/Groth16-схем — **вне scope** (trust boundary). Аудит проверяет
контрактные инварианты: CEI, replay, cross-circuit binding на уровне контракта, anchor-манипуляции,
adapter-слой (bind публичных inputs к прувам).

---

## Резюме

- **BC: 0.** Контрактные инварианты verifyBlock / applyBkSetUpdate / knownAnchors выдержаны.
  CEI чистый, replay закрыт, tail-garbage закрыт, adapter-слой связывает публичные inputs с прувом.
- **QC: 4** (см. `questions.md`, ID `QC-A2-1..4`) — расхождение кода и документации (AB-Q4),
  liveness-связка `applyBkSetUpdate`↔`verifyBlock`, приём нулевого хеша в активном слое,
  монотонность `_highestActiveLayer` при постоянном сокращении числа слоёв.
- **Новые инварианты аудитора: 7** (`invariants-extended.md`, раздел A2) — `A2-INV-1..7`.

### Топ-3 риска

1. **QC-A2-3 (Medium):** `verifyBlock` проверяет на ноль только хвост (`i ≥ numLayers`).
   Нулевой хеш в **активном** слоте (`layerHashes[i]==0`, `i < numLayers`) принимается,
   а `_appendLayerHashes` пропускает нулевые значения → слой не попадает в своё окно.
   Это может рассинхронизировать `_highestActiveLayer` / `_expectedPrevAnchor` и множество
   якорей для `withdrawByProof`. Реальная эксплуатация требует валидного Circuit-2 прува
   с нулевым активным хешем — вопрос к схеме (возможно ли это в принципе).
2. **QC-A2-1 (Medium/Low):** документация (`bridge_verification.md` LH-3, CC-6, §6.5, §5.3 L6)
   описывает anchor-проверку как `prevMaxLevelLayerHash == storedPrevMaxLevelLayerHash`,
   но код уже использует `_expectedPrevAnchor(numLayers)` (per-layer pick, фикс AB-Q4).
   L6-мониторинг, построенный по задокументированному «плоскому» инварианту, будет ложно
   срабатывать на шагах уменьшения `numLayers`. Операционный риск, не контрактный баг.
3. **QC-A2-2 (Low/Medium):** `applyBkSetUpdate` подаёт `storedLastSeenBlockSeqNo`
   (курсор `verifyBlock`) как публичный input `lastSeenBlockSeqNo` аттестации, но монотонность
   гейтит по отдельному `storedLastBkSetUpdateSeqNo`. Возможна liveness-связка: если `verifyBlock`
   продвинул курсор между генерацией и отправкой прува ротации — ротация отвергается.

---

## Checklist

- [x] Two-proof flow Primary/Fallback + LayerHashes
- [x] CEI: storage on failed crypto
- [x] Replay: blockSeqNo, prevMaxLevelLayerHash
- [x] Layer tail zero enforcement
- [x] knownAnchors population
- [x] VerifyBlockDisabled path
- [x] CC-1..7 attack scenarios
- [x] applyBkSetUpdate (BK-1..5, Circuit-3-adjacent Merkle-binding)
- [x] Adapter layer: bind публичных inputs к прувам (Groth16 + SHPLONK)

---

## Разбор по инвариантам

### CEI / порча состояния на неуспешной криптографии (AC-6, LH-8)

`verifyBlock` (строки 656–741) строго упорядочен: feature-gate → shape/range → anchor-проверки
→ оба верификатора → **только затем** запись state (726–738) → `emit`. Между записями и внешними
вызовами внешних вызовов нет; сами верификаторы объявлены `external view` в интерфейсах, то есть
не могут мутировать state (staticcall-семантика), плюс функция `nonReentrant`. Любой revert
(включая `AttestationProofRejected` / `LayerHashesProofRejected`) происходит **до** первой записи,
поэтому частичной мутации быть не может. Подтверждено тестом
`AckiNackiBridgeRelayerLoop.t.sol::test_relayerLoop_onAttestationReject_stateUntouched`.
→ **A2-INV-1**.

### Replay: blockSeqNo (LH-6, CC-5)

`blockSeqNo <= storedLastSeenBlockSeqNo ⇒ revert BlockSeqNoNotMonotonic` (677–679). Строгая
монотонность. Повтор того же блока после успеха ловится этим же чеком
(`test_verifyBlock_replayAfterSuccess_reverts`). Курсор продвигается ровно на записанное значение.

### Replay / fork: prevMaxLevelLayerHash (LH-3, CC-6) — расхождение с докой

Код использует **не** плоский `storedPrevMaxLevelLayerHash`, а `_expectedPrevAnchor(numLayers)`
(685–688), который берёт `pick = min(numLayers, t)`, `t = _highestActiveLayer()`, и возвращает
последний хеш окна слоя `pick` (или генезис-seed при `t==0`). Это фикс AB-Q4 (регрессия
`AckiNackiBridgeLayerAnchor.t.sol`), зеркалящий партнёрский `prev_max_level_layer_hash_for`.
Инвариант выполняется корректно, но **документация устарела** → QC-A2-1.

### Layer tail garbage (LH-5, CC-7)

`for (i=numLayers; i<MAX; i++) if (layerHashes[i]!=0) revert LayerHashTailNonZero(i)` (669–671).
Плюс на записи перезаписываются все 10 слотов (728–730), включая обнуление хвоста → устаревший
ненулевой хвост от блока с бо́льшим `numLayers` затирается. Хвост чист. **Однако** активные слоты
на ноль не проверяются → QC-A2-3.

### finType Primary vs Fallback

`finType` выбирает верификатор (696–712), но сам прув должен пройти соответствующий VK. Primary-прув
не проходит Fallback-VK и наоборот (разные схемы/пороги 2/3 vs 1/2). Поэтому `finType` в событии
всегда соответствует принявшему верификатору и не подделывается независимо. → **A2-INV-4**.

### Fast-forward / seq gaps

Контракт допускает разрыв `blockSeqNo` (`test_relayerLoop_seqNoFastForward_isPermittedByContract`),
НО per-layer anchor + связывание цепочки внутри Circuit 2 криптографически запрещают пропуск
реального блока AN: `prevMaxLevelLayerHash` входящего блока обязан совпасть с последним принятым
хешем нужного слоя, а схема связывает его с непосредственно предыдущим блоком AN. Разрыв seqNo
проявляется только как stall, не как инъекция разрыва цепочки. → **A2-INV-5**.

### knownAnchors — влияние на будущие withdrawByProof

`_appendLayerHashes` (816–827) добавляет каждый ненулевой `layerHashes[L-1]` в окно слоя `L`.
`withdrawByProof` проверяет `_isKnownLayerAnchor(WITHDRAW_ANCHOR_LAYER=1, finalRoot)` (1042).
Отравить множество якорей произвольным значением нельзя: `layerHashes` связаны публичными inputs
Circuit-2-прува (`LayerHashesAggregatorVerifier` сверяет `_readInstance(proof, 15+i) == layerHashes[i]`).
→ **A2-INV-2**. Окно — кольцевой буфер на `HISTORY_PROOF_WINDOW=128`; старые якоря вытесняются →
вывод должен быть отправлен в пределах ≤128 проверенных блоков (fail-closed). → **A2-INV-3**.

Соответствие индексации: контрактные слои 1-индексные (`L=1..numLayers`, `hashValue=layerHashes[L-1]`);
`WITHDRAW_ANCHOR_LAYER=1 → layerHashes[0]`, партнёр использует `layer_idx=0 → layerHashes[0]`. Совпадает.

### VerifyBlockDisabled (LH-9)

Любой из трёх слотов `address(0) ⇒ revert VerifyBlockDisabled` (657–663). Слоты `immutable`.
Тесты: `test_verifyBlock_disabled_revertsOnFreshBridge`, `test_verifyBlock_partiallyWired_revertsAsDisabled`,
`testFuzz_disabledBridge_alwaysReverts`.

### applyBkSetUpdate (BK-1..5, Circuit-3-adjacent)

- Гейт `oldCommitmentL2 == storedBkSetCommitment` (774–776) + монотонность по
  `storedLastBkSetUpdateSeqNo` (777–779).
- Аттестация над **старым** комитетом (`oldCommitmentL2` как bkSet-PI, 787/795).
- Merkle-binding: `root = SHA256(SHA256(H0 ‖ SHA256(L2‖L3)) ‖ H23) == blockId` (801–806).
  `newCommitmentL3` связан с реальным блоком только через `blockId`, который закоммичен аттестацией
  старого комитета. Прообразостойкость SHA-256 не даёт подделать произвольный новый комитет.
  → **A2-INV-7**.
- Replay закрыт двойным гейтом (после ротации `oldCommitmentL2 != storedBkSetCommitment`
  **и** `blockSeqNo <= storedLastBkSetUpdateSeqNo`). → **A2-INV-6**.
- **QC-A2-2:** `lastSeenBlockSeqNo`-PI подаётся из курсора `verifyBlock`, гейт — по отдельному курсору.

### Adapter layer (S6 — verifier bypass)

- Groth16-адаптеры (`PrimaryVerifier`, `LayerHashesMovementVerifier`): проверка `proof.length == 256`,
  сборка публичных inputs в фиксированном порядке, `try verifyProof {} catch { return false }` —
  нормализуют revert в `false` (LH-7, ZK-2). Garbage-прув отвергается схемой.
- SHPLONK-адаптеры (`PrimaryAggregatorVerifier`, `FallbackAggregatorVerifier`,
  `LayerHashesAggregatorVerifier`): проверка минимальной длины `(12+NUM_INNER)*32`, затем
  `_readInstance` сверяет каждый публичный input по фиксированным offset'ам (12..15 / 12..25)
  **с переданными аргументами** до вызова Yul-верификатора. Это критичная связка: нельзя подать
  произвольный `blockId`/`bkSet`, пока прув коммитит другой. Затем `shplonkVerifier.verify(...)`
  возвращает bool из staticcall (не revert). Предполагается, что Yul-верификатор потребляет instances
  по тем же offset'ам (стандарт snark-verifier-sdk; покрыто `ShplonkAggregatorForgery.t.sol`).

---

## Findings

| ID | Sev | BC/QC/OK | Summary | PoC |
|----|-----|----------|---------|-----|
| QC-A2-1 | Medium/Low | QC | Дока LH-3/CC-6/§6.5/L6 описывает плоский anchor `== storedPrevMaxLevelLayerHash`; код использует `_expectedPrevAnchor(numLayers)` (AB-Q4). L6-мониторинг ложно сработает на shrink. | `AckiNackiBridgeLayerAnchor.t.sol` (демонстрирует per-layer pick) |
| QC-A2-2 | Low/Medium | QC | `applyBkSetUpdate` подаёт `storedLastSeenBlockSeqNo` как PI аттестации, но гейтит по `storedLastBkSetUpdateSeqNo` — liveness-связка с `verifyBlock`. | pending (вопрос к партнёру о prover-семантике `lastSeen`) |
| QC-A2-3 | Medium | QC | Нулевой хеш в активном слоте (`layerHashes[i]==0`, `i<numLayers`) принимается; `_appendLayerHashes` пропускает нули → возможный рассинхрон окна/якоря. | pending (зависит от того, может ли Circuit 2 выдать нулевой активный хеш) |
| QC-A2-4 | Low | QC | `_highestActiveLayer()` монотонно не убывает (окна хранят старые записи); `t` не сокращается при постоянном уменьшении числа слоёв AN → регров берёт возможно устаревший высокий root. | pending (нужна партнёрская семантика при постоянном сокращении) |

**BC: нет.** Все контрактные инварианты A2 выдержаны. Прочие пункты — QC (вопросы партнёру/доке) и
наблюдения (см. новые инварианты).

## New invariants proposed

| ID | Statement |
|----|-----------|
| A2-INV-1 | При любом revert (crypto/shape/anchor) все `stored*` и `_layerWindows` не изменяются побайтно — нет частичных записей (CEI). |
| A2-INV-2 | Каждое значение в `_layerWindows[L]` — это `layerHashes[L-1]` некоторого успешного `verifyBlock`, связанного Circuit-2-прувом; нет пути записать произвольное значение без прохождения прува. |
| A2-INV-3 | `withdrawByProof` принимает `finalRoot` только из последних ≤128 добавленных хешей слоя 1; более старые вытесняются (liveness-граница, fail-closed). |
| A2-INV-4 | Эмитируемый `finType` всегда равен принявшему верификатору; Primary-прув не проходит Fallback-VK и наоборот — `finType` не подделывается независимо. |
| A2-INV-5 | Разрывы `blockSeqNo` допускаются контрактом, но per-layer anchor + Circuit-2 binding криптографически запрещают пропуск реального блока AN — разрыв проявляется только как stall. |
| A2-INV-6 | `applyBkSetUpdate` не реплеится: после ротации `oldCommitmentL2 != storedBkSetCommitment` и `blockSeqNo <= storedLastBkSetUpdateSeqNo` (двойной гейт). |
| A2-INV-7 | `newCommitmentL3` связан с реальным блоком только через `blockId == SHA256(...L2,L3...)`, закоммиченный аттестацией старого комитета; прообразостойкость SHA-256 не даёт подделать новый комитет. |
