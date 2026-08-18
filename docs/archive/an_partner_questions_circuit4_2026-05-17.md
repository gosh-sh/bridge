# Вопросы по Circuit 4 (`bridge-event-prove-circuit`) — 2026-05-17

> **Статус 2026-05-21 (вечер): Алина ответила почти на всё.** Полная
> расшифровка её ответов + финальный consolidated layout — в
> `docs/an_partner_circuit4_alina_replies_2026-05-21.md`. Здесь оригинал
> сохраняется для протокольной памяти.
>
> **Принято**: Q-C4-1 (`amount` + `recipient` public, но `recipient` =
> 2 Fr вместо 1), Q-C4-2 (nullifier = `Poseidon(block_id, tokenId,
> amount, recipient, senderDapp, senderAcc)`, public), Q-C4-3 (б)
> (`dstChainId` public), Q-C4-4 (фикс 20 байт). Q-C4-6 (implicit):
> sender — отдельные `senderDappFr` + `senderAccFr` (видно из формулы
> nullifier'а; ждём явного подтверждения семантики `dappFr`/`accFr`).
> Q-C4-5 — Алина встревожилась; в ответе её успокаиваем (ceremony это
> R14, Phase 9, далеко за горизонтом).
>
> **Финальный layout**: 110 public Fr (см. файл выше).
> **Открытые под-вопросы**: split-конвенция recipient'а (10/10 vs 16/4),
> recipient в Poseidon-preimage идёт 1 или 2 Fr.

Алина, привет. Прочитал твой `bridge-event-prove-circuit` и
`EVENT_LAYOUT_COMPARISON.md`, на нашей стороне всё нужное для приёма этого
proof-а уже стоит — `AckiNackiBridge` собирает rolling-окно из 100 последних
top-of-chain layer hashes на каждом `verifyBlock`, новый
`verifyEvent(proof, tokenId)` форвардит все 103 public inputs в Groth16
verifier (gnark wrapper тоже scaffold-нут). 16 Foundry-тестов зелёные.

Маленький side-note по layout: твой `EVENT_LAYOUT_COMPARISON.md §5.9` всё
ещё описывает старый `[final_root, tokenId, ephemeral_pubkey]`, а код
`bridge_event_prove_circuit.rs:813-817` уже выдаёт
`[token_id, dapp_fr, acc_fr, layer_hashes…]`. Мы выровнялись по коду
(он — source of truth), но `EVENT_LAYOUT_COMPARISON.md`, видимо, стоит
обновить, чтобы у нас не было двух противоречащих документов.

Но реального `withdraw()` (т.е. «доказательство → выпуск ETH получателю»)
из текущей версии серкута мы построить не можем — `amount`, `recipient`,
`dstChainId` и `sender` остаются private witnesses, контракт просто не знает,
сколько и кому платить. Поэтому Phase A у нас — это attestation-only
(`verifyEvent` эмитит событие и всё), а перед Phase B (настоящий withdraw)
у нас к тебе пять вопросов.

---

## Q-C4-1. Сделать `amount` и `recipient` public inputs

Это самый фундаментальный блокер. Чтобы Ethereum контракт мог отдать
эфир получателю, эти два поля должны быть видны верификатору.

Наше предложение по layout (порядок твой — мы подстроим adapter):

```
[0]            tokenId        (uint32)            ← уже public
[1]            amount         (uint128)           ← new
[2]            recipientFr    (Fr-encoded 20-байтный EVM-адрес —
                              та же конвенция, что у dapp_fr/acc_fr через bytes_to_fr)
[3]            dstChainId     (uint64 или uint256, см. Q-C4-3)
[4]            dappFr                              ← уже public
[5]            accFr                               ← уже public
[6..=105]      layerHashes    (100 кандидатов)    ← уже public
```

Итого 106 public Fr. Изначально мы прикидывали хи/lo-сплит на 20-байтный
адрес, но BN254 `Fr` — 254 бита (≈31.75 байт), а EVM-адрес — 160 бит, так
что один Fr вмещает целиком; та же конвенция, как ты упаковываешь
`account_dapp_id`/`account_id` через `bytes_to_fr`. Это убирает источник
кросс-цепочечной упаковочной неоднозначности у нас на ETH-стороне.

**ABI-break**: новая layout — это hard break для нашего `BridgeEventVerifier.sol`
(сейчас слоты 0..2 захардкожены под `[tokenId, dappFr, accFr]`). Как только
ты опубликуешь v2, мы повторяем gnark setup и переписываем adapter — нам не
страшно. Альтернативный (более мягкий) вариант, если для тебя удобнее: дописать
новые поля в хвост, оставив существующий 103-input префикс нетронутым
(`[103]=amount, [104]=recipientFr, [105]=dstChainId, [106..]=layerHashes`).
На серкуте это бесплатно, а нам сэкономит один re-spin gnark setup.
Выбирай как удобнее.

«Сколько constraints это добавит» — нас интересует, насколько это
поднимает время Halo2-доказательства; если порядок прежний (десятки
секунд, не часы), мы готовы.

## Q-C4-2. Nullifier

Сейчас Circuit 4 без nullifier'а и `verifyEvent` идемпотентно-replay-имый.
Для attestation-only это безобидно, для `withdraw()` — фатально (один
валидный AN-event сразу даёт неограниченный drain через replay).

**Минимально-инвазивный вариант, который мы предлагаем**: ты уже считаешь
`block_leaf = Poseidon96(block_id, envelope_hash, ext_out_root)` как
internal witness для events Merkle-доказательства
(`bridge_event_prove_circuit.rs:719-744`). Просто выставь его (либо
`Poseidon(block_id, envelope_hash)`) public Fr-ом — новых Poseidon-caps не
добавится, лишних constraints нет, а на ETH-стороне у нас получается
готовая `bytes32`-метка для `mapping(bytes32 => bool) _nullifiers`.

Цена на нашей стороне — 1 SSTORE per withdraw (~22 100 газа cold, ~5 000
warm), это нас вполне устраивает.

**Fallback-ы** (если `block_leaf` по каким-то причинам не подходит):

- 7-полевой Poseidon-хеш
  `Poseidon(envelope_hash || block_id || tokenId || amount || recipient || senderDapp || senderAcc)` —
  все ингредиенты уже привязаны проверками SHA-256 cell-tree, так что
  collision-устойчивость не страдает, но это лишние Poseidon-cap'ы.
- Просто `nullifier := envelope_hash` — самый дешёвый, **но небезопасный**:
  два одинаковых `WithdrawalInitiated` от одного аккаунта в одном блоке
  дадут одинаковый `repr_hash`, и второй withdraw потеряется. `block_id`
  обязан быть в формуле.

Какой вариант для тебя дешевле в реализации?

## Q-C4-3. Семантика `dstChainId`

Сейчас `dstChainId` в серкуте private, но никаких ограничений на него
не накладывается. На ETH-стороне нам нужно гарантировать, что доказательство
для «withdraw to BSC» нельзя зареплеить как «withdraw to Ethereum».

Два варианта:

- **(а)** `dstChainId` остаётся приватным, но ты выпускаешь по одному
  circuit + VK на каждую целевую цепочку, и серкут хардкодит
  `dstChainId == EXPECTED` внутри тела (с `EXPECTED` запечённым как
  circuit-constant per VK). Минусы: по одной церемонии trusted setup на
  каждую цепочку, по одному VK в наших контрактах, не масштабируется.
- **(б)** `dstChainId` поднять в public. Тогда ETH-контракт ассертит
  `dstChainId == block.chainid` и один серкут поддерживает любое количество
  целевых цепочек.

Мы рекомендуем (б) — масштабируется лучше, хорошо компонуется с Q-C4-1,
и `block.chainid` корректен на mainnet/testnet/любом форке. Окей?

## Q-C4-4. Variable-length `recipient`

В `EVENT_LAYOUT_COMPARISON.md §5.6` упоминается путь для recipient'ов
переменной длины, но текущий серкут жёстко зашит на
`RECIPIENT_LEN_FIXED = 20` (EVM-адрес 20 байт). Для Phase B нас это
устраивает — ETH-сторона всё равно требует ровно 20 байт.

Подтверди, что переменная длина — это Phase C (не Phase B), и мы не
тратим circuit-бюджет сейчас на эту обобщённость.

## Q-C4-6. Семантика `dappFr`/`accFr` + `sender` в public (добавлено 2026-05-21)

**Контекст**: Алина в обсуждении Q-C4-1 (2026-05-20 evening) подняла
концептуальный вопрос — «выходит после Q-C4-1 останется лишь `sender`
приватным? Цель анонимизации в бридже — скрывать чисто сендера?».
Полный идейный ответ с разбором «ZKP в бридже ≠ ZKP в dex'е»
вынесен в `docs/an_partner_circuit4_concept_response_2026-05-21.md`.

Краткое: **в бридже нет цели анонимизировать ничего**. ZKP здесь это
trustless verification of remote-chain state, не privacy. Все поля
события должны быть public — иначе мы либо теряем функциональность
(`recipient` private ⟶ не знаем кому платить), либо audit trail
(`sender` private ⟶ нельзя off-chain мониторить откуда withdraw).

**Конкретный вопрос для Алины**: что такое `dappFr`/`accFr` в текущем
layout (`bridge_event_prove_circuit.rs:813-817`)?

- (а) `bytes_to_fr(event.sender.dapp_id)` + `bytes_to_fr(event.sender.account_id)`
  — т.е. это и есть sender, разложенный на два Fr. Тогда `sender` УЖЕ
  public, ничего добавлять не нужно, Q-C4-1 layout достаточен.
- (б) Что-то другое (dapp_id / account_id самого bridge-контракта на
  AN-стороне, или какие-то circuit-internal binders). Тогда **поднимай
  ещё два public Fr** для самого sender'а:
  `senderDappFr = bytes_to_fr(event.sender.dapp_id)` и
  `senderAccFr = bytes_to_fr(event.sender.account_id)`.

В случае (б) предлагаемый итоговый layout:

```
[0]       tokenId         (uint32)
[1]       amount          (uint128)         — Q-C4-1
[2]       recipientFr     (Fr, EVM-addr)    — Q-C4-1
[3]       dstChainId      (uint64)          — Q-C4-3 (б)
[4]       senderDappFr    (Fr)              — Q-C4-6
[5]       senderAccFr     (Fr)              — Q-C4-6
[6]       dappFr                            — уже public
[7]       accFr                             — уже public
[8..107]  layerHashes     (100 candidates)  — уже public
```

108 public Fr вместо 106. Constraint-wise: 2 internal witness'а
pin'нуты как instance, нет новых Poseidon-cap'ов, размер Groth16
proof не меняется. Цена нулевая, audit trail полный.

**Без блокера для Phase B** — Phase B может стартовать на (а)-layout
(106 public) и подняться до (б)-layout без второго gnark-respin, если
Алина в первой же v2 включит оба `senderDappFr`+`senderAccFr` сразу.

## Q-C4-5. Trusted setup ceremony

Маленькое уточнение по терминологии у нас в плане. Phase A wrapper для
Circuit 4 сейчас — identity-stub (наша внутренняя ризка **R15**), такой же
как для 1A/1B/2. Real Halo2-in-gnark verification (полноценная reduction
SHPLONK → R1CS) — это Phase 8 R&D у нас, мы за неё ещё не сели. Phase 9
(multi-party trusted setup ceremony / **R14**) становится осмысленной
**только после** Phase 8 — иначе мы получим «идеально защищённую церемонией
заглушку».

Сам вопрос: когда придёт время Phase 9, имеет смысл бандлить Circuit 4 в
общую церемонию с 1A/1B/2/3, или запускать для него отдельную? Логистически
нам всё равно, и Phase 9 — далеко за горизонтом текущего спринта. Но если
ты уже планируешь Circuit 4 как часть «MPC-набора», скажи — мы добавим его
в Phase 9 scope сейчас, а не постфактум.

---

## Дополнительно — не вопрос, а просьба

Когда будет первый «настоящий» Halo2-proof из `bridge-event-prove-circuit`
(пусть на тестовых данных), пришли нам `halo2_proof.json` в том же формате,
что для Circuit 2 (см.
`crates/bridge-snark-utils/src/proof_export.rs:17-22`, структура
`Halo2ProofData` с полями `public_inputs`, `proof_bytes`, `protocol`).
Тогда у нас встанет полноценный E2E:

```
halo2_proof.json
  → crates/bridge-snark-utils/gnark-wrappers/circuit-4/
       (go run . setup …; go run . prove …)
  → BridgeEventGroth16VerifierGenerated.sol
  → AckiNackiBridge.verifyEvent(proof, tokenId)  ✅
```

И ещё мелочь по терминологии — мы у себя `Poseidon96` называем то, что ты
используешь в `bridge_event_prove_circuit.rs:36, :670` (Poseidon на
3 × 32-байтных Fr, `T=3, RATE=2, R_F=8, R_P=57`, из
`bridge-event-prove-circuit/src/poseidon.rs:7-10`). Подтверди, что это
один и тот же примитив, и у тебя в утилитах он уже есть — это упрощает
Q-C4-2.

---

## Приоритеты

- **Q-C4-1 + Q-C4-2 + Q-C4-3** — гейтят Phase B (настоящий `withdraw()`
  на Ethereum). Без всех трёх контракт не может ни заплатить, ни
  защититься от replay, ни от cross-chain-replay соответственно.
- **Q-C4-4** — confirm-only, скорее всего «да, это Phase C», на 30
  секунд.
- **Q-C4-5** — стратегический, не блокирует ничего сейчас.
- **Q-C4-6** — 1-минутный clarification (что такое `dappFr`/`accFr`).
  Если ответ (б), бесплатно бандлится в v2 одним коммитом — Phase B не
  блокируется ни в одном из двух кейсов.

Phase A (attestation-only `verifyEvent` + rolling layer-window) уже
зелёная и продолжает работать независимо от ответов.

Спасибо!
