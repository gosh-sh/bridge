> **⚠️ ARCHIVED 2026-08-18 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/ETH-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

# PR 2112 (`gosh-sh/acki-nacki`) — Review: `TokenBridge.sol` + `DepositVoucher.sol`

**Reviewer**: Sergey Egorov (bridge integration / Pruvendo)
**Date**: 2026-05-13
**Scope**: only the bridge-relevant Solidity surface
(`contracts/exchange/TokenBridge.sol`, `contracts/exchange/DepositVoucher.sol`,
`contracts/exchange/modifiers/{errors,modifiers}.sol`). The rest of the PR
(ansible, docker, zerostate keys, block-manager refactors, etc.) is
out of scope for this review.

---

## Overall

Хорошее, продуманное направление. Главные правильные решения:

1. **Один контракт на все токены, а не контракт-на-токен.** `tokenId` уехал
   в параметры `initiateWithdrawal` / `finalizeDeposit` / `confirmDeposit`
   и в события (`WithdrawalInitiated`, `DepositFinalized`). Это снимает
   именно ту проблему, которая обсуждалась во вторник: серкут на стороне
   AN остаётся прибит к одному адресу-эмиттеру, а шапка блока не пухнет
   от количества поддерживаемых токенов. Точно тот pattern, который
   используют Wormhole, Polygon zkEVM Bridge, zkSync Era L1ERC20Bridge,
   Across, OP StandardBridge — индустриальный стандарт.
2. **Раздельный учёт двух потоков USDC**: `_totalMinted` для TIP-3 → ECC
   (старый stripe-path) и `_totalMintedBridge` / `_totalBurnedBridge`
   для кросс-чейн пути. Не смешано, легко аудиторить.
3. **`DepositVoucher` как deterministic one-shot anti-replay** —
   изящный TVM-натуральный паттерн: адрес ваучера = `tvm.hash(stateInit)`,
   где `stateInit` через `static _depositHash` привязан к параметрам
   депозита. Повторный `finalizeDeposit` с теми же параметрами врубается
   в уже существующий аккаунт ваучера и no-op'ит. Параметры ваучера
   дополнительно валидируются через `tvm.hash(abi.encode(...)) == _depositHash`
   в конструкторе — атакующий не сможет подменить детали и переиспользовать
   тот же слот.
4. **`confirmDeposit` access control через `msg.sender == address.makeAddrStd(0, tvm.hash(stateInit))`** —
   корректно. Никто, кроме deterministically-deployed ваучера, в этот
   путь не зайдёт.
5. **Nonce'ы на owner-mint путях** (`_mintNonce`, `_mintAccumulatorNonce`)
   с строгим `nonce == _mintNonce + 1` — нормальная защита от
   off-chain replay подписанных запросов на mint.
6. **NatSpec густой и осмысленный**, читать приятно.

Дальше — что нужно поправить ДО мейнета.

---

## Блокеры (must fix before deploy на сеть, где free mint неприемлем)

### B1. `finalizeDeposit` принимает `proof`, но не проверяет его

```solidity
function finalizeDeposit(bytes proof, uint256 srcDappId, ..., uint256 srcDepositId) public view {
    // TODO: verify `proof` via gosh.zkhalo2verify (see contracts/dex_dev_halo).
    //       Currently no verification — proof is accepted as-is and ignored.
    proof;
    ...
}
```

Это в текущем виде **публичный free-mint faucet любых ECC-токенов**: кто
угодно может вызвать `finalizeDeposit(any_bytes, any_srcDappId, any_sender,
any_recipient, any_amount, any_tokenId, fresh_srcDepositId)` и получить
mint в `confirmDeposit`. Anti-replay через `DepositVoucher` защищает только
от *повторного* минта тех же параметров, а не от *выдуманных новых*.

В комменте это честно отмечено: «DO NOT deploy this version to a network
where free mint is unacceptable». Этот комментарий должен превратиться в
явный `require(false, "PROOF_VERIFICATION_NOT_IMPLEMENTED");` в начале
функции — иначе у любого кто соберёт mainnet zerostate из этого коммита
будет открытый кран. Альтернатива: на уровне контракта поставить
`onlyOwnerPubkey(_ownerPubkey)` модификатор на `finalizeDeposit` до тех
пор, пока ZK-проверка не подключена.

Когда `gosh.zkhalo2verify` появится — в проверку должны попасть **те же
шесть параметров**, что идут в `_depositHash`, плюс `proof`-блоб. Желательно
сделать так, чтобы `_depositHash` напрямую был одним из public inputs
ZK-серкута, тогда контракт сверяет:

```solidity
require(gosh.zkhalo2verify(proof, vkRef, depositHash, ...other public inputs...), ERR_INVALID_PROOF);
```

— и единственная ответственность контракта это поднимать ваучер по
проверенному хешу.

### B2. `setDepositVoucherCode` — backdoor под mint, без таймлока

```solidity
function setDepositVoucherCode(TvmCell code) public onlyOwnerPubkey(_ownerPubkey) accept { ... }
```

Owner может в любой момент заменить код ваучера на такой, который
пропускает проверку `tvm.hash(...) == _depositHash` в конструкторе и
сразу зовёт `confirmDeposit` с произвольными параметрами. Эффективно —
owner может в одиночку напечатать любое количество ECC любого токена.

Для v1 на тестовой сети — приемлемо. Для мейнета — **обязательно**
один из вариантов:

1. **Timelock** (например, 7 дней) на смену кода ваучера, с публичным
   событием `DepositVoucherCodeChangeProposed(newCodeHash, executableAfter)`.
2. **Multi-sig** ключ вместо одного `_ownerPubkey` (`m-of-n` подписей).
3. **Immutable voucher code**: убрать `setDepositVoucherCode`, кодировать
   ваучер один раз в zerostate, эскалация при необходимости идёт через
   `updateCode`-апгрейд самого `TokenBridge` (тоже под owner key, но у него
   единый риск-профиль — апгрейд ВСЕГО контракта — а не точечно ваучера).

Прямо сейчас в комменте сказано «can be called after upgrade if
`_depositVoucherCode` was passed as an empty cell, or to rotate the
voucher implementation». «Rotate the voucher implementation» в одиночку
по подписи одного ключа — это критическая централизация, её нужно
явно зафиксировать как принимаемый риск либо устранить.

---

## Существенные замечания (high severity, желательно до merge)

### M1. Атрибут `view` на `finalizeDeposit` — семантически сомнителен

Функция:

- зовёт `tvm.accept()` (списывает газ из контракта);
- зовёт `ensureBalance()` → `gosh.mintshellq` (минтит vmshell);
- разворачивает `new DepositVoucher{...}` (деплоит новый аккаунт).

Storage самого `TokenBridge` действительно не модифицируется в этой
функции — это происходит позже в `confirmDeposit` через колбек. В
TVM-Solidity такая семантика `view` может быть формально допустимой,
но **внешне** функция радикально не view: эмитит side effects,
тратит газ, создаёт аккаунты. Это путает auditor'ов и tooling (ABI-парсеры
могут предполагать read-only).

Рекомендация: убрать `view`. Если есть какая-то специфическая
TVM-Solidity причина оставить — добавить NatSpec-объяснение прямо
над функцией.

### M2. `confirmDeposit` обновляет `_totalMintedBridge`, но никогда не сверяется с `_totalBurnedBridge`

Класс невидимых багов: если по какой-то причине (баг в ZK-прувере, баг
в node-side relayer'е) на одной стороне сминтили больше, чем сожгли на
другой, мост не получит сигнала. Можно добавить дешёвую sanity-проверку:

```solidity
// Bridge can never be "under-collateralised" — minted vs burned must agree
// modulo same-token, but cross-token comparison is fine as a cheap canary.
require(_totalMintedBridge <= _totalBurnedBridge + MAX_BRIDGE_IMBALANCE, ERR_BRIDGE_IMBALANCED);
```

Или, ещё проще, опубликовать инвариант в getter'е `getTotalBridged()` и
заводить мониторинг внешне. На уровне контракта — хотя бы события
`BridgeImbalanceWarning(...)` при превышении некоторого дельта.

(Замечание: в текущей модели один мост обрабатывает много `tokenId`, и
суммарный `_totalMintedBridge` не равен сумме burned по тому же tokenId —
эти счётчики «по всем токенам». Лучше держать `mapping(uint32 => uint128)
_totalMintedBridgeByToken` и зеркальный для burned, тогда инвариант
проверяется per-token. Изменение storage layout, на новой минорной версии
имеет смысл сделать сразу.)

### M3. `initiateWithdrawal` — нет минимальной длины `recipient`

```solidity
require(recipient.length <= 64, ERR_RECIPIENT_TOO_LONG);
```

Только верхняя граница. `recipient.length == 0` пройдёт — пользователь
сожжёт ECC на «никуда». На AN-стороне это, возможно, безобидно, но на
destination-chain это потерянные средства. Минимально стоит
`require(recipient.length >= 20, ERR_RECIPIENT_TOO_SHORT);` (20 байт —
длина EVM-адреса; для AN-стороны через TVM можно 32).

Лучше — параметризовать минимум по `dstChainId` через
`mapping(uint256 => (uint8 minLen, uint8 maxLen)) _chainRecipientBounds`,
устанавливаемый owner'ом.

### M4. Нет глобального уникального `withdrawalId` в событии `WithdrawalInitiated`

```solidity
event WithdrawalInitiated(uint256 dstChainId, bytes recipient, uint128 amount, uint32 tokenId, address sender);
```

Если два пользователя выполнят `initiateWithdrawal` с одинаковыми
`(dstChainId, recipient, amount, tokenId, sender)` — в логе они
неразличимы по содержанию. Relayer вынужден упорядочивать их по
`(block_seq_no, message_lt)` через node-side метаданные. Это работает,
но создаёт хрупкую зависимость от консистентного порядка обработки
сообщений между нодами.

Стандартное решение — внутренний инкрементируемый счётчик:

```solidity
uint64 _withdrawalCounter;
...
emit WithdrawalInitiated(_withdrawalCounter++, dstChainId, recipient, amount, tokenId, msg.sender);
```

Тогда у каждого withdrawal есть глобальный уникальный id внутри
этого контракта, и relayer / destination-chain получают
строгий монотонный nonce.

### M5. Нет паузы / emergency stop

Если в `gosh.zkhalo2verify` найдут баг (или в Алинином серкуте,
или в наших Solidity-адаптерах с другой стороны), нужен механизм
заморозки бриджа без полного передеплоя. Минимум — owner-controlled
boolean `_paused`, который блокирует `finalizeDeposit` и
`initiateWithdrawal`:

```solidity
bool _paused;
modifier whenNotPaused() { require(!_paused, ERR_PAUSED); _; }
function pause()   public onlyOwnerPubkey(_ownerPubkey) accept { _paused = true; emit Paused(); }
function unpause() public onlyOwnerPubkey(_ownerPubkey) accept { _paused = false; emit Unpaused(); }
```

Для mainnet — желательно с тем же timelock'ом / multi-sig на `unpause`,
что и на `setDepositVoucherCode`.

### M6. `tvm.accept()` вызван дважды в `initiateWithdrawal`

```solidity
function initiateWithdrawal(uint256 dstChainId, bytes recipient) public {
    tvm.accept();            // line 174
    ensureBalance();
    require(recipient.length <= 64, ERR_RECIPIENT_TOO_LONG);
    ...
    tvm.accept();            // line 188
    ensureBalance();
    ...
}
```

Идемпотентно, но смотрится как либо забытый remove, либо неуверенность
автора. Один вызов в начале, после самых базовых require'ов — стандартный
паттерн.

### M7. Storage layout `onCodeUpgrade` хрупкий

```solidity
(uint256 pubkey, address usdcWallet, uint128 totalMinted, uint64 mintNonce,
 uint64 mintAccumulatorNonce, uint128 totalMintedBridge, uint128 totalBurnedBridge,
 TvmCell depositVoucherCode) = abi.decode(cell, (uint256, address, uint128, uint64, uint64, uint128, uint128, TvmCell));
```

8 полей, любой будущий апгрейд добавит ещё. Каждый раз нужно отдельно
помнить, что у migration-скрипта формат cell'а изменился. Рекомендация
— ввести `uint16 layoutVersion` в первом поле и явно switch'иться:

```solidity
(uint16 v, TvmCell rest) = abi.decode(cell, (uint16, TvmCell));
require(v >= 1 && v <= CURRENT_LAYOUT_VERSION, ERR_UPGRADE_VERSION);
if (v == 1) { /* decode v1 layout */ }
else if (v == 2) { /* decode v2 layout */ }
```

Дополнительный плюс: можно безопасно ронять старые fields в новой версии
(добавляешь only-new fields decode, считая старые нулём).

### M8. `version = "1.1.0"`, а layout `onCodeUpgrade` поломан (5 → 8 полей)

В комменте написано: «for migration from v1.0.x pre-populate them with
current state (or zeros / empty cell) and set the voucher code later via
`setDepositVoucherCode` if needed». То есть мигрирует через ручное
дополнение cell'а в скрипте. По semver это **major break** (старый
upgrade payload не валиден), а не minor. Я бы поднял до `2.0.0`.
Или, опять же, ввёл `layoutVersion` (M7) и принимал бы оба формата.

---

## Мелкие замечания (low severity, polish)

### L1. `DepositVoucher.sol` — устаревший комментарий

```solidity
/// @notice Deploy callback: forwards confirmation to Exchange so it can
///         mint ECC for the recipient. Only callable by Exchange.
```

`Exchange` теперь `TokenBridge`. Поправить два упоминания (line 21 + 22).

### L2. `address(this).balance > MIN_BALANCE` vs `>=`

В `ensureBalance` используется строгое `>`. На границе ровно `MIN_BALANCE`
вызовется `mintshellq(MIN_BALANCE)` без необходимости. Косметика.

### L3. Имя контракта — `TokenBridge` слегка вводит в заблуждение

Контракт делает три разные вещи:

1. TIP-3 USDC → ECC[3] bridge (`onTransferReceived`).
2. Прямой owner-mint ECC[3] / mint+Accumulator (`mintAndSend*`).
3. Кросс-чейн bridge для произвольных ECC tokenId
   (`initiateWithdrawal` / `finalizeDeposit`).

«TokenBridge» больше подходит к (3), а первые два — это «stripe-mint
gateway» и «owner mint admin». Если переименование уже сделано, не
менять, но для документации полезно отдельно описать каждый из трёх
поддерживаемых flow.

### L4. Поле `_usdcWallet` теперь делит namespace с обобщённым
кросс-чейн путём

Сейчас `_usdcWallet` это адрес TIP-3 USDC TokenWallet'а, и
`onTransferReceived` уже жёстко привязан к USDC через `USDC_ECC_ID`.
Если когда-то захочется добавить другой TIP-3-токен на тот же путь —
надо будет расширять `_usdcWallet` до `mapping(address wallet => uint32 eccId)`
и проверять `eccId` в колбеке. Не требуется прямо сейчас, но
архитектурно стоит зафиксировать в комменте, что `_usdcWallet` —
*sole* TIP-3 источник, не расширяемый.

### L5. `confirmDeposit` уязвимости к collision в `abi.encode`

```solidity
uint256 depositHash = tvm.hash(abi.encode(srcDappId, srcSender, recipient_an, amount, tokenId, srcDepositId));
```

`bytes srcSender` имеет переменную длину. В Ethereum-Solidity `abi.encode`
length-prefixes динамические типы и коллизии исключены. В TVM-Solidity это
тоже так (по AbiV2-семантике), но я бы прямо в комменте подчеркнул, что
коллизии между разными значениями параметров невозможны именно потому,
что `abi.encode` (а не `abi.encodePacked`!) length-prefixes `bytes`. Это
не баг, а защита от подобной ошибки в будущем при правках.

Дополнительно: тест с попыткой подобрать коллизию (два разных
`(srcSender, recipient_an)`, дающих один `depositHash`) — стоит добавить
в test-suite. Negative-test, должен фейлиться в констукторе ваучера.

### L6. `_depositVoucherCode` captured at `finalizeDeposit` time, но
verified at `confirmDeposit` time

В обеих функциях `stateInit` строится с **текущим** `_depositVoucherCode`.
Если между `finalizeDeposit` и `confirmDeposit` owner вызовет
`setDepositVoucherCode(newCode)`:

- ваучер уже задеплоен из старого кода и зовёт `confirmDeposit`;
- `expected = address.makeAddrStd(0, tvm.hash(stateInit_with_NEW_code))`;
- `msg.sender == expected` → false → revert.

Депозит in-flight теряется. Owner-only функция, так что не атак-вектор,
но в timelock на `setDepositVoucherCode` (см. B2) стоит добавить отдельное
правило: между proposed и executable timestamp `setDepositVoucherCode` не
влияет на старые `_depositVoucherCode`, активные in-flight ваучеры
дорабатывают по старому коду. Реализация — двух-cell хранилище кода
(`_activeVoucherCode`, `_pendingVoucherCode`, `_pendingActivationTs`).

### L7. Нет события на successful upgrade / setPubkey / setDepositVoucherCode

Каждое из этих действий — security-critical owner operation, и
наблюдатели должны иметь возможность отследить их по on-chain логам.
Добавить:

```solidity
event PubkeyChanged(uint256 oldPubkey, uint256 newPubkey);
event DepositVoucherCodeChanged(uint256 oldHash, uint256 newHash);
event CodeUpgraded(uint256 newCodeHash, string newVersion);
```

### L8. `_mintNonce + 1` overflow

Пройдёт через `unchecked` арифметику TVM-solidity при `u64::MAX`. На
практике не достижимо, но добавлю в чек-лист тестов: `require(_mintNonce
!= type(uint64).max, ERR_NONCE_OVERFLOW);` перед инкрементом — копеечная
страховка.

### L9. `triggerTransaction` — открытое окно для отправки

```solidity
function triggerTransaction(address txAddr) public view onlyOwnerPubkey(_ownerPubkey) accept {
    txAddr.transfer({value: 1 vmshell, bounce: true, flag: 1});
}
```

Owner может в любой адрес отправить 1 vmshell с `bounce: true`. Само по
себе не критично (vmshell у бриджа намайнен через `gosh.mintshellq`, не
из user funds), но как admin-функция должна быть отделена от
prod-flow'а — отдельный getter "discovered Transaction contracts to
trigger" + event лог на каждый вызов был бы аккуратнее.

---

## Тесты, которые я бы добавил в этот PR

Если в `dex` уже есть test-suite на старый `Exchange.sol` — большую
часть переименовать и пройти как было. Дополнительно нужны:

- **`finalizeDeposit_replay_blocked`**: вызвать дважды с теми же
  параметрами — второй вызов должен быть no-op (ваучер уже есть).
- **`finalizeDeposit_replay_different_id_succeeds`**: тот же `srcDappId/srcSender/recipient/amount/tokenId`,
  но новый `srcDepositId` — должен пройти как новый депозит. Это и есть
  critical-test текущего free-mint (см. B1): без ZK-проверки этот тест
  показывает, что любой может бесконечно минтить через инкремент
  `srcDepositId`.
- **`confirmDeposit_wrong_sender_reverts`**: попытка позвать
  `confirmDeposit` с произвольного адреса должна реверт-нуть
  `ERR_INVALID_SENDER`.
- **`confirmDeposit_after_voucher_code_change_reverts`**: подняли
  ваучер старым кодом, сменили `_depositVoucherCode`, ваучер зовёт
  `confirmDeposit` — должен реверт-нуть (см. L6).
- **`initiateWithdrawal_no_ecc_reverts`** + **`initiateWithdrawal_multiple_ecc_reverts`**:
  уже покрыто require'ами, нужны юнит-тесты.
- **`initiateWithdrawal_zero_amount_reverts`**, **`overflow_reverts`**.
- **`nonce_must_be_strictly_incrementing`**: `nonce = _mintNonce + 2` —
  revert; `nonce = _mintNonce` — revert.
- **`upgrade_with_old_layout_reverts`** (если введём `layoutVersion` — M7).
- **Negative**: попытка вызвать `mintAndSend` с не-owner pubkey — revert
  `ERR_NOT_OWNER`.

---

## Что я хотел бы видеть в PR-описании / changelog

1. Явное упоминание, что **бридж сейчас без ZK-проверки** и должен быть
   защищён хотя бы owner-only модификатором до подключения
   `gosh.zkhalo2verify`. См. B1.
2. Что меняется storage-layout — список полей до/после и инструкция по
   подготовке upgrade-cell'а для существующих instance'ов.
3. Описание контракта interaction-схемы между `TokenBridge` ↔
   `DepositVoucher` — диаграмма bring-up/replay-protection-flow была бы
   полезна.
4. Пометка про централизацию: `_ownerPubkey` контролирует pause (когда
   появится), voucher code, pubkey rotation, code upgrade — это нужно
   зафиксировать как принимаемый риск (или анонсировать timelock/multi-sig
   roadmap для mainnet).

---

## TL;DR

- Архитектурно — **очень правильный шаг**, и совпадает с тем, что мы
  обсуждали по поводу единого контракта на много токенов.
- Один **блокер** — `finalizeDeposit` пропускает proof, что в текущем
  виде делает контракт free-mint faucet'ом. Обязательно либо
  `require(false, ...)` либо `onlyOwnerPubkey` модификатор до момента
  включения `gosh.zkhalo2verify`. Иначе любой коммит этого PR в зерostate
  mainnet-подобной сети — катастрофа.
- Один **существенный backdoor** — `setDepositVoucherCode` без timelock'а.
  Для тестовой сети ок, для mainnet — обязательно timelock или multi-sig.
- Остальное — улучшения и polish, не блокеры.

Готов помочь с любым из пунктов: набросать ZK-verify-стаб, добавить pause,
переписать `onCodeUpgrade` с `layoutVersion`, написать negative-tests.
Дайте знать, что брать на себя.
