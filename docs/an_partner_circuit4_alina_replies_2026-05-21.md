# Ответы Алины по Circuit 4 (2026-05-21)

**Контекст**: Алина прислала ответы на Q-C4-1..6 + concept-response.
Этот файл — её ответы дословно + наш decoded layout, чтобы протокольная
память хранилась в репо, а не только в мессенджере.

---

## Q-C4-1 — `amount` + `recipient` в public

> «amount, recipient я наверное не стала бы упаковывать, сделала бы 2
> отдельных паблик инпута. так вроде удобней всетаки, ну с моей
> стороны. думаю будет норм в плане производительности, сильно большой
> прибавки по времени генерации пруфа я думаю не будет.»

**Decoded**:

- `amount` (uint128) — один public Fr. ✅ согласовано.
- `recipient` (20-byte EVM address) — **2 public Fr** (hi/lo split), не
  один `bytes_to_fr(addr)`. Алина предпочитает 2 Fr на стороне circuit'а.

**Открытый под-вопрос** (нам нужно подтвердить с Алиной): какая
конкретно split-конвенция?

- (α) **bytes_to_fr by halves**: `recipientHi = bytes_to_fr(addr[0..10])`,
  `recipientLo = bytes_to_fr(addr[10..20])`. ETH-сторона:
  `address(uint160(uint256(hi) << 80 | uint256(lo)))`.
- (β) **uint128 + uint32 split** (high 128 bits + low 32 bits, оба
  zero-padded): `recipientHi = first 16 bytes`, `recipientLo = last
  4 bytes`. Для EVM-адресов первые 12 байт будут нулевыми, остальные
  8 в `hi` + 4 в `lo` (немного wasteful по слотам).
- (γ) **uint160 split на uint80 hi + uint80 lo**: ровно по 80 bits с
  каждой стороны, `addr = (hi << 80) | lo`. Эквивалентно (α) если оба
  считаются как big-endian.

Дефолт у нас — (α) или (γ) (это одно и то же численно). Ждём её
подтверждения. Decoder на ETH-стороне ~5 строк в любом случае.

## Q-C4-2 — Nullifier

> «envelope_hash и block_id — два по-разному высчитанные хеша одного и
> того же блока. в целом я бы везде пользовалась block_id, мы его и
> так экстрагируем уже и публично проверяем в серкуте 1а и 2,
> envelope_hash я бы вообще не трогала.
>
> nullifier = Poseidon(block_id, tokenId, amount, recipient,
>                      senderDapp, senderAcc)
>
> envelope_hash я тут убрала, это всё-таки избыточно как мне кажется.
>
> nullifier хеш я же понимаю правильно должен стать дополнительным
> публичным инпутом?
>
> envelope_hash и block_id всё-таки не уникальны per-event — там же
> внутри одного блока теоретически много ивентов может случиться. так
> что я всё-таки за вариант с Poseidon выше.»

**Decoded**:

- Nullifier формула: `Poseidon(block_id, tokenId, amount, recipient,
  senderDapp, senderAcc)`. ✅ согласовано.
- Nullifier — **public input**. ✅ да (наш `mapping(bytes32 => bool)
  _nullifiers` на ETH-стороне его читает).
- envelope_hash в формулу не включается. ✅ согласовано — она права
  что block_id достаточен и canonical.
- Мой исходный fallback "nullifier := envelope_hash" — отброшен, как
  и было сказано в Q-C4-2 ("небезопасный").

Один тонкий момент: **6 полей в Poseidon** (block_id, tokenId, amount,
recipient, senderDapp, senderAcc). Если у Алины Poseidon hash работает
по чанкам (например T=3 RATE=2 ⟶ 2 Fr на абсорбцию), это будет
3 sponge-rounds. Cost negligible. Если recipient идёт как 2 Fr
(см. Q-C4-1 split), то всего 7 Fr в preimage'е — 4 sponge-rounds.

**Уточнение** для Алины: `recipient` в Poseidon-preimage идёт как **1 Fr**
(полная 20-байтная packed форма через `bytes_to_fr(addr)`) или **2 Fr**
(тот же split, что в public inputs)? Логически они должны быть
консистентны — если public inputs показывают hi/lo, то и nullifier-hash
должен брать hi/lo, иначе ETH-сторона не сможет пересчитать `expected_nullifier` для проверки.

## Q-C4-3 — `dstChainId` в public

> «сделаю dstChainId поднять в public. в тестах как и вы в eth буду
> везде ассертить dstChainId == 1»

**Decoded**: вариант (б) ✅. `dstChainId` — public Fr. ETH-контракт
ассертит `== block.chainid` (это `1` на mainnet).

## Q-C4-4 — Variable-length recipient

> «я сделаю ровно 20 байт, там действительно почему-то с запасом
> вроде было зашито на всякий случай»

**Decoded**: ✅ Phase B → ровно 20 байт фиксированный. Variable-length
переезжает на Phase C / never. ✅

## Q-C4-5 — Trusted setup ceremony

> «ну вот этот самый страшный вопрос пожалуй»

**Decoded**: Алина встревожилась. Это понятно — multi-party trusted
setup ceremony это серьёзная логистическая операция (ZkSync, Aztec
делают весь год MPC).

**Что нужно сказать в ответе**: ceremony это **R14, Phase 9** — далеко
за горизонтом текущего спринта. Прямо сейчас её НЕ актуально готовить
по двум причинам:

1. У нас все gnark wrapper'ы — **identity-stub'ы** (R15): они принимают
   Halo2-proof как unverified byte-blob и пробрасывают public inputs.
   Реальная Halo2-in-gnark verification (полноценная SHPLONK → R1CS
   reduction) — это **Phase 8 R&D**, мы за неё ещё не сели. Без Phase
   8 ceremony защищала бы заглушку: «идеально защищённую церемонией
   заглушку».
2. Phase 9 (multi-party ceremony) запускается только после Phase 8,
   когда у нас есть **финальный, production-ready** wrapper-circuit.
   Иначе любое изменение в wrapper'е обнуляет результаты ceremony.

В практическом плане: Алине **сейчас** делать ничего по Q-C4-5 не нужно.
Когда придёт время Phase 8/9 (skeptically, через 3–6 месяцев — после
того как 1A/1B/2/3 и 4 все стабилизируются и все open vk'и
зафиксируются), мы соберёмся отдельно, разберём что такое MPC ceremony
(`snarkjs powersOfTau` или `kzg-ceremony-client`) и спланируем.

Bundle vs separate: вопрос остаётся открытым, но логистически имеет
смысл **bundle**ить 1A/1B/2/3/4 в одну ceremony — экономит вдвое
participant-tooling и обеспечивает общий SRS. Я бы рекомендовал именно
bundle. Но это решается потом, не сейчас.

## Q-C4-6 — `dappFr`/`accFr` semantics + sender public

Алина прямо не ответила, но из её **nullifier-формулы** видно что она
оперирует `senderDapp || senderAcc` отдельно от `dappFr`/`accFr`. Это
подтверждает ответ **(б)** на Q-C4-6:

> `dappFr`/`accFr` это **не** sender, а что-то другое (видимо bridge-
> contract identity на AN-стороне, или AN-side dapp/account binders).
> `sender` — это **отдельные** `senderDappFr` + `senderAccFr`, которые
> мы добавляем как новые public inputs.

✅ Layout получает 2 дополнительных Fr на sender'а.

---

## Финальный consolidated public-input layout (drafted)

```
[ 0]       tokenId         (uint32)              — already public, slot stays
[ 1]       amount          (uint128)             — Q-C4-1, new
[ 2]       recipientHi     (Fr, top half of EVM-addr)  — Q-C4-1, new (split)
[ 3]       recipientLo     (Fr, bottom half)           — Q-C4-1, new
[ 4]       dstChainId      (uint64)              — Q-C4-3 var (б), new
[ 5]       senderDappFr    (Fr)                  — Q-C4-6, new (sender hi)
[ 6]       senderAccFr     (Fr)                  — Q-C4-6, new (sender lo)
[ 7]       dappFr                                — already public (bridge id?)
[ 8]       accFr                                 — already public (bridge id?)
[ 9]       nullifier       (Fr)                  — Q-C4-2, new
[10..109]  layerHashes     (100 candidates)      — already public
```

**Итого 110 public Fr** (вместо 103 в Phase A, вместо 106 в первом
драфте Q-C4-1). Constraint-wise это +7 instance columns, размер
Groth16 proof неизменен (256 bytes), gnark setup потребует rerun
(не страшно — 1 раз).

`tokenId` намеренно остаётся в слоте [0] чтобы наш текущий
`verifyEvent(proof, tokenId)` API максимально совпал с финальной
ABI'й (только нужны 7 новых параметров).

---

## Что меняется у нас на ETH-стороне

После v2 circuit'а:

1. **Hard ABI break** для `BridgeEventVerifier.sol` (103 → 110 public
   Fr, индексы смещены). Это уже было обещано в Q-C4-1 — мы готовы.
2. **`AckiNackiBridge.verifyEvent(...)`** меняется с `(proof, tokenId)`
   на `(proof, tokenId, amount, recipient, dstChainId, senderDapp,
   senderAcc, nullifier)` — все public-input поля передаются explicitly,
   контракт проверяет что `proof.publicInputs[i] == arg[i]`.
3. **New storage**: `mapping(bytes32 => bool) _nullifiers` + check
   `require(!_nullifiers[bytes32(nullifier)], "Replay")`.
4. **`block.chainid == dstChainId`** assertion внутри `verifyEvent`.
5. **ETH-transfer**: `_transferOut(recipient, amount, tokenId)` (для
   nativeETH `tokenId == 0` — выполняем `recipient.call{value: amount}`;
   для ERC-20 — `tokens[tokenId].transfer(recipient, amount)`).
6. **Recipient reconstruction**: `address(uint160(uint256(recipientHi)
   << 80 | uint256(recipientLo)))` (при split-conv (α)/(γ)).

Phase B implementation backlog (на нашей стороне) — закроется ~1 день
после получения v2-circuit'а + регенерации Groth16 setup.

---

## Open questions back to Alina

1. **Q-C4-1**: какая split-конвенция для recipient (α / β / γ — см. выше)?
   Дефолт у нас (α): по 10 байт hi / 10 байт lo.
2. **Q-C4-2**: `recipient` в Poseidon-preimage идёт как 1 Fr или 2 Fr
   (тот же split что в public inputs)? Должны быть консистентны.
3. **Q-C4-6 (implicit confirm)**: `dappFr`/`accFr` это bridge-контракт
   identity или что-то третье?

Никакая из этих неопределённостей **не блокирует** Phase B на нашей
стороне — мы спокойно ждём ответы.
