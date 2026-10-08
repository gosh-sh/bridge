# Изменение `minBK` на шелнете (вкл/выкл ротацию Block Keeper'ов)

Инструкция как на шелнете **включить ротацию BK** (`minBK = 4`) и **выключить её** (`minBK = 5`).

---

## TL;DR

Шелнет работает на **5 активных BK**. Параметр `minBlockKeepers` в контракте `BlockKeeperContractRoot`
задаёт минимально допустимое число активных BK. BK может выйти из эпохи (освободить слот под нового)
**только если после выхода активных останется не меньше `minBK`**.

| `minBK` | `activeBK - 1 >= minBK` (при 5 BK) | Поведение | Зачем |
|--------:|:----------------------------------:|:----------|:------|
| **4**   | `5 - 1 = 4 >= 4` → **true**        | BK может уйти → слот освобождается → **ротация идёт** | нужна ротация BK |
| **5**   | `5 - 1 = 4 >= 5` → **false**       | BK держат в эпохе (`cantDelete`) → **ротации нет**    | ротация не нужна |

Меняется одним вызовом метода **`setConfig`** контракта `BlockKeeperContractRoot`
(адрес `0:7777…7777`, dapp 0), подписанным ключом-владельцем.

---

## Как это работает (механика)

Контракт: [`contracts/bksystem/BlockKeeperContractRoot.sol`](../../contracts/bksystem/BlockKeeperContractRoot.sol).

Поле состояния:

```solidity
uint128 _minBlockKeepers = 12;   // default в исходнике; на шелнете задан через setConfig
```

Гейт ротации — в `decreaseActiveBlockKeeper(...)` (вызывается, когда эпоха BK завершилась и BK хочет выйти):

```solidity
if (_numberOfActiveBlockKeepers - 1 >= _minBlockKeepers) {
    _numberOfActiveBlockKeepers -= 1;
    // ... BlockKeeperEpoch(msg.sender).canDelete(...)  → BK удаляется, слот свободен → приходит новый
} else {
    BlockKeeperEpoch(msg.sender).cantDelete(...);       // → BK остаётся, ротации нет
}
```

Итог: сеть **не даёт** числу активных BK опуститься ниже `minBK`. Поэтому:

- при `minBK = 5` и ровно 5 BK — ни один не может уйти (упали бы до 4) → все запинены, ротации нет;
- при `minBK = 4` — один BK может уйти (останется 4 ≥ 4) → слот освобождается, кандидат заходит → цикл ротации.

> ⚠️ **Для реальной ротации при `minBK = 4` в очереди должны быть кандидаты** (BK-кошельки,
> отправившие stake-request). Если кандидатов нет — старый BK уйдёт, а замены не будет, и сеть
> просто просядет до 4 активных.

---

## ⚠️ Главный подводный камень: `setConfig` перезаписывает ВЕСЬ конфиг

Сигнатура (из скомпилированной ABI `contracts/0.79.3_compiled/bksystem/BlockKeeperContractRoot.abi.json`):

```
setConfig(
    uint64  epochDuration,
    uint128 minBlockKeepers,
    bool    isNeedNumberOfActiveBlockKeepers,
    uint128 needNumberOfActiveBlockKeepers,
    uint8   walletTouch,
    uint128 nlinit
)
```

Метод **не «патчит» одно поле — он затирает все шесть за один вызов**. Плюс из `epochDuration`
пересчитываются производные:

```solidity
_epochCliff = epochDuration / 10;   // CONFIG_CLIFF_DENOMINATOR
_waitStep   = epochDuration / 20;   // CONFIG_WAIT_DENOMINATOR
```

Значит, чтобы поменять **только** `minBlockKeepers`, надо передать **текущие** значения остальных
пяти полей. Иначе случайно сбросишь длину эпохи / walletTouch / nlinit.

### Текущие значения, которые надо сохранить

| Поле | Значение на шелнете | Как узнать |
|:-----|:--------------------|:-----------|
| `epochDuration` | прочитать вживую | getter **`getConfig`** → `epochDuration` (дефолт деплоя `660`) |
| `isNeedNumberOfActiveBlockKeepers` | `false` | зашито при генерации зеростейта |
| `needNumberOfActiveBlockKeepers` | `0` | зашито при генерации зеростейта |
| `walletTouch` | `200` | **нет геттера** — из деплой-env (`WALLET_TOUCH`, дефолт 200) |
| `nlinit` | `5000` | **нет геттера** — из деплой-env (`NLINIT`, дефолт 5000) |

> ⚠️ Геттера для `minBlockKeepers`, `walletTouch`, `nlinit`, `needNumber…` **нет** — их значения
> нельзя прочитать из контракта. `epochDuration` читается через `getConfig`, остальные бери из
> деплой-конфигурации шелнета. **Если шелнет деплоился с нестандартными env — сверь их перед вызовом.**

Источник дефолтов: [`contracts/scripts/generate_zerostate.py`](../../contracts/scripts/generate_zerostate.py)
(`EPOCH_LENGTH_AFTER_ZEROSTATE=660`, `MIN_BLOCKKEEPERS=5`, `WALLET_TOUCH=200`, `NLINIT=5000`,
`isNeed=false`, `needNumber=0`).

---

## Ключи и права

`setConfig` защищён модификатором `onlyOwner`:

```solidity
modifier onlyOwner { require(msg.pubkey() == tvm.pubkey(), ERR_NOT_OWNER); _; }
```

→ подписывать **ключом-владельцем `BlockKeeperContractRoot`** — тем keypair'ом, которым деплоили
зеростейт шелнета (`config/BlockKeeperContractRoot.keys.json` соответствующего деплоя).

> Перед вызовом уточни у SeHor05, какой именно keys-файл владелец BK-root на текущем шелнете.

---

## Пошагово

### Self-service параметры окружения (всё локально — Сергей не нужен)

```bash
# tvm-cli v3.1.0 (локальная сборка, arm64 Mac). ВАЖНО: /usr/bin/tvm-cli 2.24.x несовместим
# с новой нодой — не использовать.
CLI=/Users/alinat/HALO2_TVM_EXPERIMENTS/V2_tools/tvm-cli

# Публичный GraphQL endpoint шелнета
ENDPOINT="https://shellnet.ackinacki.org/graphql"

# BlockKeeperContractRoot (dapp 0) — зашитый адрес, одинаковый на всех деплоях
ROOT=0:7777777777777777777777777777777777777777777777777777777777777777

# ABI из локального клона acki-nacki
ABI=/Users/alinat/HALO2_TVM_EXPERIMENTS/acki-nacki/contracts/0.79.3_compiled/bksystem/BlockKeeperContractRoot.abi.json

# Ключ-владелец BK-root шелнета (подтверждено для деплоя 14-07-2026)
#   pubkey: 44577cbc0d95eaa5fdca4defdbef3a8bf4a8acd6ae58f9fe368d7952bb01ea41
KEYS=/Users/alinat/HALO2_TVM_EXPERIMENTS/shellnet_config_14_07_2026/BlockKeeperContractRoot.keys.json

# Эндпоинт в CLI-конфиг (чтобы не передавать --url в каждом вызове)
$CLI config --url "$ENDPOINT"

# Проверить что CLI и ABI на месте
$CLI version | head -1    # → "tvm-cli 3.1.0"
test -r "$ABI"   && echo "ABI ok"
test -r "$KEYS"  && echo "KEYS ok"
```

> ⚠️ Эти пути подтверждены на этой машине. Файлы (`ABI`, `KEYS`, `CLI`) не должны меняться во
> время эксперимента — не перекомпилируй `acki-nacki` и не трогай `shellnet_config_14_07_2026/`
> между Шагами 1–5.

### Шаг 1. Прочитать текущий `epochDuration` и зафиксировать «до»

```bash
$CLI -j runx --abi "$ABI" --addr "$ROOT" -m getConfig
# → {"epochDuration":"660","epochCliff":"66","waitStep":"33"}

$CLI -j runx --abi "$ABI" --addr "$ROOT" -m getDetails
# → {"minStake":"...","numberOfActiveBlockKeepers":"5",...}
```

- Возьми `epochDuration` из первого вывода (ниже в примерах — `660`). Если вернулось другое —
  подставляй **своё** значение во все `setConfig` вызовы ниже; `_epochCliff/_waitStep`
  пересчитаются автоматически как `epochDuration/10` и `epochDuration/20`.
- Сохрани `numberOfActiveBlockKeepers` из второго вывода в блокнот — это baseline для Шага 4.
  Для шелнета 14-07-2026 ожидание = `5`.

> `walletTouch` и `nlinit` **геттера не имеют**. Шелнет 14-07-2026 деплоился со стандартными env
> → дефолты `walletTouch=200`, `nlinit=5000` (см. `generate_zerostate.py`). Если есть
> подозрение что деплой нестандартный — вытащи их из `shellnet_config_14_07_2026/zerostate`
> или подтверди у админа **до** Шага 2. С неверными значениями `setConfig` затрёт их на 200/5000.

### Шаг 1.5. Dry-run (без отправки)

Чтобы убедиться что JSON-пэйлоад парсится правильно и ABI совпадает, прогони **то же тело**
через `runx` (локальный симулятор, без подписи и broadcast):

```bash
$CLI -j runx --abi "$ABI" --addr "$ROOT" -m setConfig \
  '{"epochDuration":660,"minBlockKeepers":4,"isNeedNumberOfActiveBlockKeepers":false,"needNumberOfActiveBlockKeepers":0,"walletTouch":200,"nlinit":5000}'
```

- exit code 0 и пустой `{}` в выводе → payload валиден.
- `Wrong data format` / `Field * is missing` → поправь JSON, не отправляй `callx`.
- `Not found` на поле → **ABI не тот**; проверь путь.

### Шаг 2а. ВКЛЮЧИТЬ ротацию → `minBK = 4`

```bash
$CLI callx --abi "$ABI" --addr "$ROOT" --keys "$KEYS" -m setConfig \
  '{"epochDuration":660,"minBlockKeepers":4,"isNeedNumberOfActiveBlockKeepers":false,"needNumberOfActiveBlockKeepers":0,"walletTouch":200,"nlinit":5000}'
```

### Шаг 2б. ВЫКЛЮЧИТЬ ротацию → `minBK = 5`

```bash
$CLI callx --abi "$ABI" --addr "$ROOT" --keys "$KEYS" -m setConfig \
  '{"epochDuration":660,"minBlockKeepers":5,"isNeedNumberOfActiveBlockKeepers":false,"needNumberOfActiveBlockKeepers":0,"walletTouch":200,"nlinit":5000}'
```

> Подставь **свои** `epochDuration` (из Шага 1), `walletTouch`, `nlinit`, если деплой был нестандартным.

### Шаг 3. Проверка

1. **Транзакция прошла** — в выводе `callx` нет ошибок, exit code 0.
2. **Число активных BK** (геттера для самого `minBK` нет):

   ```bash
   $CLI -j runx --abi "$ABI" --addr "$ROOT" -m getDetails
   # → {"minStake":"...","numberOfActiveBlockKeepers":"5"}
   ```

3. **По факту** — понаблюдать за сетью в течение эпохи (`epochDuration` секунд):
   - при `minBK = 4` — по завершении эпохи один из BK уходит в cooler, на его место заходит кандидат
     (число активных ≈ держится, состав меняется);
   - при `minBK = 5` — состав активных BK не меняется, все досиживают/переизбираются на месте.

4. **BK-set по GQL** — что увидит relayer:

   ```bash
   # «последнее обновление BK-set, которое попало на цепь»
   curl -s "$ENDPOINT" -H 'content-type: application/json' -d '{
     "query":"{ bk_set_update(limit:1, order_by:{seq_no:desc}) { seq_no bk_set_commitment } }"
   }' | jq
   ```

   - при `minBK = 4` этот seq_no должен расти примерно раз в эпоху;
   - при `minBK = 5` — застыть на последнем значении.

### Шаг 5. Rollback (ВЫКЛЮЧИТЬ обратно) — на случай аварии

Та же команда что Шаг 2б, но её стоит держать наготове в отдельном терминале **до** Шага 2а:

```bash
$CLI callx --abi "$ABI" --addr "$ROOT" --keys "$KEYS" -m setConfig \
  '{"epochDuration":660,"minBlockKeepers":5,"isNeedNumberOfActiveBlockKeepers":false,"needNumberOfActiveBlockKeepers":0,"walletTouch":200,"nlinit":5000}'
```

Срабатывает мгновенно по контракту, но физически ротация остановится только когда очередной BK
попытается выйти и получит `cantDelete` — т.е. не раньше следующего завершения эпохи.

> ⚠️ Rollback **не возвращает** состав BK назад: если за время эксперимента состав поменялся,
> останется тот, что есть на момент rollback. Это норма — текущий состав BK — рабочий, в сеть
> уже вошёл; минимально влияет только на дальнейшие циклы.

---

## Координация с relayer-ом (bridge daemon)

Запуск BK-set-update ротации на шелнете — это именно та нагрузка, под которую мы готовили
`live_relayer_bridge_runbook.md`. Два безопасных сценария:

### Сценарий A: daemon уже запущен (steady state на `minBK=5`)

1. Убедиться что daemon здоров (см. Health checks в runbook'е).
2. Выполнить Шаг 2а (включить `minBK=4`).
3. Наблюдать `applyBkSetUpdate`-транзакции в логе daemon'а, начиная со следующего обновления
   BK-set по GQL (Шаг 4, п.4).
4. Если что-то пошло не так — Шаг 5 (rollback) **НЕ останавливая daemon**. Daemon дорешит
   уже-в-полёте обновление и дальше встанет на `verifyBlock`-only.

### Сценарий B: daemon ещё не запущен

1. Выполнить Шаг 2а (включить `minBK=4`) заранее — пусть первое обновление BK-set попадёт на
   цепь **до** cold-start.
2. Дождаться следующего `bk_set_update` seq_no (Шаг 4, п.4).
3. Запустить daemon по Case 1 из runbook'а — он подхватит обновление штатно.
4. Rollback (Шаг 5) по готовности, уже на живом daemon'е.

### Чего НЕ делать

- **НЕ** переключать `minBK` 4↔5 чаще раза в эпоху — relayer может не успеть обработать
  предыдущий `applyBkSetUpdate` (см. Case 9 в runbook'е: Phase 1 Stuck).
- **НЕ** перезаписывать `bk_set.shellnet.json` вручную до того как daemon увидел первое
  обновление (см. Case 10). Daemon сам стартует с bootstrap-снэпшотом, а дальше живёт по
  `prover_bk_set.json` и `/v2/bk_set_update`.

---

## Troubleshooting

| Симптом | Причина | Что делать |
|:--------|:--------|:-----------|
| `callx` → `exit_code = 100` (`ERR_NOT_OWNER`) | KEYS не те | Сверить `pubkey` в `BlockKeeperContractRoot.keys.json` с тем, что был при деплое. Для шелнета 14-07-2026 — `44577cbc…ea41`. |
| `callx` → `Wrong data format` | JSON-пэйлоад не соответствует ABI | Прогнать Шаг 1.5 (dry-run). Проверить что все 6 полей переданы, типы — числа/bool как в ABI. |
| `callx` → `Account not found` или `No account` | ENDPOINT не тот / нода не отвечает | `curl -s "$ENDPOINT" -d '{"query":"{info{version}}"}' \| jq` должен вернуть версию. |
| `getConfig` возвращает прежние `epochCliff/waitStep` после `setConfig` | Транзакция ещё не финализирована | Подождать ~5–10 сек, повторить. |
| После `setConfig minBK=4` состав BK не меняется ни через эпоху, ни через две | В очереди нет stake-кандидатов | Проверить что кто-то отправил stake-request. Без кандидатов ротация физически невозможна (BK просто остаются в coolers → активных < 5). |
| `applyBkSetUpdate` в daemon логе не поднимается | GQL `bk_set_update` не растёт | Повторить Шаг 4 п.4. Если застыло — проверь на ноде что `BlockKeeperContractRoot` реально получил `setConfig` (`getConfig` → `_epochCliff = epochDuration/10`). |

---

## Заметки

- `minBK` **не влияет на экономику стейка**: при `isNeedNumberOfActiveBlockKeepers = false`
  `minStake` считается от `numberOfActiveBlockKeepersAtBlockStart`, а не от `minBK`. Параметр
  работает **только** как гейт выхода/ротации.
- `setConfig` можно вызывать сколько угодно раз — переключать 4 ↔ 5 туда-обратно безопасно
  (главное — каждый раз корректно передавать остальные 5 полей).
- Порог считается от **текущего числа активных BK**. Формула гейта — `activeBK - 1 >= minBK`.
  Для другого размера сети (не 5 BK) подбирай `minBK` под желаемое поведение по этой же формуле.
- Изменение действует со следующего цикла завершения эпохи, не мгновенно.
