> **⚠️ ARCHIVED 2026-08-18 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/ETH-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

# Ответ Алине — концептуальная цель ZKP в бридже (2026-05-21)

**Контекст**: Алина согласилась поднять `amount`/`recipient` в public
(Q-C4-1) и спросила: «выходит останется лишь `sender` приватным? Цель
анонимизации в бридже — скрывать чисто сендера?» Плюс честно сказала
что идейно не вполне понимает зачем ZKP в бридже.

Этот документ — драфт ответа. Не отвечает на «удобно ли тебе сделать»,
отвечает на «что концептуально нужно».

---

## Короткий ответ

**В бридже нет цели анонимизировать ничего.** Все 5 полей события
`WithdrawalInitiated` (`dstChainId`, `recipient`, `amount`, `tokenId`,
`sender`) должны быть **public inputs**. Делай `sender` тоже public —
он не должен быть private witness'ом.

## Длинный ответ — почему ZKP в бридже ≠ ZKP в dex'е

Это два очень разных use-case, и название «zero-knowledge proof» в
обоих случаях одинаковое, но семантика противоположная:

| Аспект | DEX | Бридж |
|---|---|---|
| Цель ZKP | **Privacy** | **Trustless verification of remote-chain state** |
| Что прячем | Балансы, who-paid-whom, operation graph | **Ничего** |
| Зачем proof | Чтобы valid operation осталась private | Чтобы remote-chain event был доказан дёшево on-chain |
| Public inputs | Минимум — только то что без чего proof бесполезен | **Максимум** — всё что контракту нужно для действий |

В бридже ZKP это, по сути, **криптографическая подпись AN-чейна над
произошедшим событием**. На ETH-стороне у нас стоит Groth16-verifier,
который говорит: «да, при таком-то state AN на блоке N произошёл event
с такими-то полями». Verifier валидирует подпись, контракт читает
поля, отдаёт ETH получателю.

Альтернатива ZKP здесь — это полноценный light client AN-чейна
встроенный в ETH-контракт. Он будет:
- работать (теоретически) — может сам валидировать AN-блоки,
- стоить ~10+ M gas на блок (BLS-подписи, BTreeMap-парсинг, Merkle-tree
  обходы),
- требовать обновлений ETH-контракта при каждом изменении AN-формата.

ZKP это **дешёвая (~287k gas) и форматно-нейтральная замена**:
вся проверка happens off-chain в circuit'е, on-chain — только Groth16
pairing check.

## Почему НИЧЕГО не нужно прятать

Подумаем что было бы если мы спрятали каждое поле:

- **`recipient` private**: контракт не знает кому платить ⟶ нельзя
  построить withdraw. Невозможен.
- **`amount` private**: контракт не знает сколько платить ⟶ невозможен.
- **`dstChainId` private**: cross-chain replay (proof для BSC реплеится
  как proof для ETH). Q-C4-3 — вариант (б) — public.
- **`tokenId` private**: контракт не знает какой ERC-20 / nativeETH
  отдавать ⟶ невозможен.
- **`sender` private**: контракт работает (sender для withdraw не нужен
  in principle), но мы теряем:
  - **audit trail** off-chain — мониторинг, compliance,
    incident-investigation, debugging типа «кто инициировал этот
    withdraw, давайте найдём баг»;
  - возможность future-proof механизмов вроде «AN-side blacklist
    sender'а ⟶ ETH-side enforce» (если AN решит банить компрометированный
    адрес, ETH должен уметь это уважать);
  - удобный анти-DoS hook (per-sender rate-limit на withdraws).

Т.е. **private sender ≠ feature, это побочный эффект**. Делая его
public, мы ничего не теряем (никакой бизнес-цели прятать sender нет)
и приобретаем три полезных свойства.

## Кому пригодилась бы анонимизация в бридже?

Иногда люди хотят «приватный bridge». Это другой продукт. Они хотят:
- скрыть `sender` (на source-chain);
- скрыть `recipient` (на dest-chain);
- скрыть `amount`.

Для этого нужны commitment'ы + shielded pool на обеих сторонах
(Tornado-style + bridge), nullifier'ы, и обычно отдельный privacy
token. Это не наш scope (по крайней мере не сейчас) — мы строим
**проверяемый, но открытый** bridge. Прозрачность фич, а не баг.

Если в будущем продакт-команда захочет «privacy mode» — это будет
**отдельный circuit + отдельный flow**, не модификация текущего.

## Предлагаемый финальный layout public inputs

Расширяя Q-C4-1 одним полем (sender в public):

```
[0]       tokenId         (uint32)         — уже public
[1]       amount          (uint128)        — Q-C4-1 (новый public)
[2]       recipientFr     (Fr, 20-byte EVM-addr через bytes_to_fr)  — Q-C4-1
[3]       dstChainId      (uint64)         — Q-C4-3 вар (б)
[4]       senderDappFr    (Fr)             — **новый public, audit trail**
[5]       senderAccFr     (Fr)             — **новый public, audit trail**
[6]       dappFr                           — уже public
[7]       accFr                            — уже public
[8..107]  layerHashes     (100 candidates) — уже public
```

Итого **108 public Fr** (вместо 106 в первой версии Q-C4-1). Constraint-
wise разница копейки — это просто 2 internal witness'а pin'нуты как
instance columns, нет новых Poseidon-cap'ов. Размер Groth16 proof
не меняется (он зависит от количества constraint'ов, не от количества
public inputs).

`tokenId` остаётся в слоте [0] чтобы наша Phase A `verifyEvent(proof,
tokenId)` адаптировалась минимально — `tokenId` мы ему передаём явно
и проверяем что он совпадает с `proof.publicInputs[0]`.

## Почему мы упустили это в Q-doc

Чесно говоря, у нас в Q-C4-1 sender не упоминался потому что в текущей
версии circuit'а он уже частично «public-ish» (через `dappFr`/`accFr`,
которые видимо производные от sender'а?). Если они и есть полная
информация о sender'е — то Q-C4-6 ниже не нужен. Если это что-то
другое (например, dapp_id и account_id источника, не sender'а) — тогда
Q-C4-6 нужен.

Уточни, что именно представляют `dappFr` и `accFr`:
- (а) это `bytes_to_fr` от `event.sender.dapp_id` и `event.sender.account_id`?
- (б) это что-то другое (например, dapp_id и account_id самого
  bridge-contract'а на AN-стороне)?

Если (а) — нет новых полей, Q-C4-1 закрывает sender. Если (б) — нужен
**Q-C4-6**: добавить `senderDappFr` + `senderAccFr` в public.

## Что нам нужно от тебя

1. **Подтверди семантику `dappFr`/`accFr`** — это sender или
   bridge-contract identity на AN-стороне? (см. выше).
2. **Если sender идентифицирован отдельно** — поднимай его в public
   тоже (Q-C4-6 ниже).
3. **`amount`/`recipient` в public** — Q-C4-1, ты уже согласилась.
4. **`dstChainId` в public** — Q-C4-3 вариант (б), ждём подтверждения.

Спасибо что подняла этот вопрос — это была у нас идейная дырка в
документации, не у тебя в понимании. Если бы ты не спросила, через
полгода кто-нибудь обнаружил бы что мы потеряли audit trail на
ровном месте. Кладу твой вопрос + этот ответ в наш Q-документ как
**Q-C4-6** для протокольной памяти.
