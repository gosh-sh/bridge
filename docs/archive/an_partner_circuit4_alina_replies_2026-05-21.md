> **⚠️ ARCHIVED 2026-08-18 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/EVM-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

# Ответы Алины по Circuit 4 (2026-05-21)

**Контекст**: Алина прислала ответы на Q-C4-1..6 + concept-response
+ follow-up уточнение по trusted-setup ceremony (07:49). Этот файл —
её ответы дословно + наш decoded layout, чтобы протокольная память
хранилась в репо, а не только в мессенджере.

**Update 2026-05-20 (вечер)**: Q-C4-5 расширен — Алина прояснила свою
ментальную модель (Phase 1 PoT universal vs Phase 2 per-circuit) и
оказалась полностью права. Мы привели нашу терминологию в соответствие,
зафиксировали что Phase 2 нужна **per wrapper** (5 ceremonies для
1A/1B/2/3/4) и что PoT (Phase 1) переиспользуется готовый. См. секцию
**Q-C4-5 → Уточнение Алины** ниже.

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

### Первый ответ Алины (07:02–07:40)

> «ну вот этот самый страшный вопрос пожалуй»

### Уточнение Алины (07:49)

> «по Trusted setup ceremony я в целом пока идейно не понимаю полностью
> как эти обёртки gros16 вокруг halo работают. я понимаю так что
> видимо gros16-серкут который умеет проверять halo2-пруф он по сути
> типовой структуры, но всё же он нефиксированный, ведь ты же меняешь
> public inputs и halo2-verification-key под конкретный halo-серкут.
> Это вероятно приводит к тому что серкут gros16-обёртка всё-таки
> меняется и получается что под каждый конкретный halo2 своя
> конкретная gros16-обёртка-серкут и свой verification key надо
> генерить или как?
>
> и если так то под каждый halo2-серкут надо Trusted setup ceremony
> проводить, то бишь вторую фазу ptau, ориентированную на конкретный
> серкут..
>
> мне кажется что возможно у нас какое-то расхождение в терминологии.
> Я просто когда говорю ptau то для меня это синоним Trusted setup
> ceremony. может это не совсем верно..
>
> Но для gros16 у нас там в любом случае 2 фазы:
> 1) собственно сами степени тау сделать, агностик к серкутам и это
>    уже конечно есть готовое, можно взять то что Mysten Sui юзало
>    например для zkLogin
> 2) подготовить уже под конкретный серкут ключи используя степени тау.
>
> и вот я так понимаю что фазу 2) нам надо делать будет под все наши
> серкуты обёрнутые в gros16?»

### Decoded — Алина права на 100%

Её ментальная модель **точна**, и она опередила меня — я в первом
ответе сэкономил technical detail, что и вызвало путаницу. Правильная
терминология которой мы должны держаться:

| Этап | Также называется | Universal / Per-circuit | Откуда брать |
|---|---|---|---|
| **Phase 1** | Powers of Tau, PoT, "степени тау", `pot28_final.ptau` | **Universal** (circuit-agnostic) | Готовый: Hermez (174 contributors, 2^28), Mysten Sui (zkLogin), Perpetual PoT |
| **Phase 2** | Circuit-specific ceremony, ZK-key ceremony, contribution chain, MPC setup | **Per-circuit** (по одной на каждый R1CS) | Делаем сами, MPC ≥ 5 contributors |

Phase 2 нам действительно нужно провести **на каждый** gnark wrapper:
1A, 1B, 2, 3, 4 → **5 отдельных Phase 2 ceremonies**.

Phase 1 (PoT) мы **не** делаем — берём готовый. Именно это означало моё
исходное «не надо ptau, всё есть», но термин «ptau» в коммьюнити часто
используется как синоним всего trusted setup, отсюда расхождение.
**Договорились на терминологии**: «Phase 1 / PoT» = universal SRS,
«Phase 2» = per-circuit ceremony.

### Почему я говорил «не критично прямо сейчас»

Наши gnark wrapper'ы прямо сейчас — **R15 identity stubs**. Они
принимают Halo2-proof как opaque byte-blob и пробрасывают public inputs
наружу без реальной верификации. R1CS у них ~5 constraints. Phase 2
для stub'ов даже single-party безопасна — нечего forge'ить, wrapper
и так пропускает что угодно.

### Когда Phase 2 ceremony станет критичной

После **Phase 8** (наша R&D, 2–4 месяца) — переписать каждый wrapper
чтобы он реально верифицировал Halo2 SHPLONK внутри R1CS. После Phase 8
R1CS вырастает до 10⁷+ constraints (real SHPLONK verification внутри
circuit'а), и без MPC любой с proving key может forge'ить wrapper-
proof'ы. Вот тогда Phase 2 ceremony становится security-critical.

### Bundle vs separate Phase 2

gnark требует **separate R1CS** на каждый wrapper (потому что R1CS
жёстко привязан к Halo2 VK конкретного circuit'а — и сам Halo2 VK, и
схема public inputs hardcode'ятся в R1CS). Поэтому **5 ceremonies**.

Но **contributors могут быть те же** и проводить все 5 в одном sprint
(~2–3 недели по плану). Логистически именно так мы планируем: один
coordinator, один pool of 5–7 contributors, 5 contribution chains
параллельно или последовательно.

### Что Алине делать сейчас

**Ничего по Q-C4-5.** Phase 8 целиком на нашей стороне (Pruvendo R&D),
Phase 9 координируется нами и поднимется отдельным разговором когда
подойдёт время (после стабилизации всех 5 circuit'ов + final VK lock).

### Reference в нашем плане

`docs/an_partner_integration_plan.md` §3:
- **Phase 8** (R&D, real Halo2-in-gnark verification) — задачи 8.1–8.6.
  Acceptance: tampered Halo2 → wrapper prove fails; R1CS ≥ 1 MB on disk.
- **Phase 9** (MPC ceremony) — 8.1 adopt PoT (Hermez `pot28_final`),
  8.2 Phase 2 MPC per circuit с N ≥ 5 contributors, 8.3 on-chain
  verifier rotation, 8.4 published audit trail (PGP-signed waste-
  destruction attestations + hash chain), 8.5 CI guard against
  accidental single-party Setup regression.

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
