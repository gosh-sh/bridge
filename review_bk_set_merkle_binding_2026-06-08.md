# Ревью: привязка BK-set update к block_id (Merkle)

**Репозиторий:** [gosh-sh/acki-nacki-to-eth-bridge-halo2-prover](https://github.com/gosh-sh/acki-nacki-to-eth-bridge-halo2-prover)  
**Ветка / коммит:** `main` @ `33ae2e3` (проверено 2026-06-08)  
**Контекст:** запрос на ревью проверки, что BK-set update принадлежит `block_id` (Merkle proof сходится — BK set является листом 8-листового block-id дерева).  
**Метод:** ручной разбор кода + независимая верификация отдельным агентом (read-only, без изменений кода).

---

## 1. Краткий вывод

| Вопрос | Ответ |
|--------|-------|
| Реализована ли привязка BK rotation → `block_id` на `main`? | **Да**, через отдельный **bk-update drain** pipeline (`c5e0ef3`), не через Circuit 3 |
| Есть ли модуль `gql_proof.rs` (порт `acki-nacki/helpers/proof_helper`)? | **Нет** |
| Достаточно ли проверок на пути Circuit 2 (layer bundle)? | **Частично** — только `leaves[2] == bk_set_commitment`; полной off-circuit Merkle-верификации нет |
| Нужен ли был локальный коммит `a88b074` (откачен)? | **Нет как есть** — основная задача на `main` закрыта иначе и глубже (IPC, verifier, state machine) |

---

## 2. Архитектура block-id Merkle tree (напоминание)

8-листовое SHA-256 дерево (нода `acki-nacki/node/src/types/ackinacki_block/mod.rs::block_merkle_leaves`):

```text
                        Root (= block_id)
                       /                  \
                  H_01                      H_23
                 /     \                  /      \
              H_0       H_1           H_2        H_3
             /   \     /   \         /   \      /   \
           L0    L1   L2   L3      L4    L5   L6    L7
```

| Лист | Содержимое |
|------|------------|
| L0 | Poseidon(`history_proofs` preimage) |
| L1 | SHA-256(common_section) |
| **L2** | **Poseidon(old BK set)** |
| **L3** | **Poseidon(new BK set)** |
| L4–L7 | TVM hash, durable state, tx_cnt, referenced blocks root |

При BK rotation в блоке: `L2 ≠ L3`. Без rotation: `L2 = L3 = [0; 32]` (нода не кладёт transition proof data).

Канонический план: `docs/bk_set_update_no_circuit3_plan.md`.

---

## 3. Что реализовано на `main`

### 3.1 BK-update drain (prover daemon)

**Файл:** `bridge-prover-daemon/src/main.rs` (≈403–697)  
**Коммит:** `c5e0ef3` — «bk-set rotation: drain loop + open Merkle proof tracker (no Circuit 3)»

Перед каждым thinned key block (Circuit 2 bundle) демон **дренирует** очередь `bkSetUpdates`:

1. `bk_set_fetcher::next_update_after(cursor)` — следующее событие с `height > stored_last_bk_set_update_seq_no`
2. `gql.query_proof_block_by_seqno(upd_seqno)` → `block_merkle_tree_leaves`
3. `BlockIdMerkleTree::from_leaves` → L2=`leaves[2]`, L3=`leaves[3]`, siblings H0, H23
4. **L2 == `prover_bk_set.commitment`** — prover не out-of-sync
5. Replay `bk_set_update_hex` → `Poseidon(new_pubkeys) == L3`
6. Circuit **1a** (Primary) или **1b** (Fallback) attestation **старым** BK set
7. Запись IPC `bkupd_NNN.json` с L2, L3, `merkle_sibling_h0_hex`, `merkle_sibling_h23_hex`, `block_id_hash_hex = tree.root`
8. Ожидание `bkupd_result_NNN.json` от verifier
9. На success: `ProverBkSet::rotate` + `BridgeState::apply_bk_set_update` → L3

**Сопутствующие модули:**

| Модуль | Назначение |
|--------|------------|
| `bridge-prover-lib/src/prover_bk_set.rs` | Prover-only pubkey table + `prover_bk_set.json` |
| `bridge-prover-lib/src/bridge_state.rs` | `stored_last_bk_set_update_seq_no`, `apply_bk_set_update` |
| `bridge-prover-lib/src/bk_set_fetcher.rs` | `next_update_after`, `parse_bk_set_changes_pub`, `normalize_bk_set_pubkeys` |
| `bridge-prover-lib/src/ipc.rs` | Schema v4, `BkUpdateRequest` / `bkupd_*.json` |
| `bridge-prover-daemon/examples/seed_pre_burst.rs` | Replay historical BK burst на shellnet |

### 3.2 Verifier (bk-update bundle)

**Файл:** `bridge-verifier-daemon/src/main.rs` — `process_bk_update_bundle` (≈682–900)

**4 проверки** (план описывает 3; в коде добавлена 2b):

| # | Проверка | Смысл |
|---|----------|-------|
| 1 | Attestation ZK (1a/1b) | Public instances `[block_id_fr, L2, seq, last_seen]`; `L2 == stored_bk_set_commitment` |
| 2 | Open Merkle | `root = SHA(SHA(H0 ‖ SHA(L2‖L3)) ‖ H23) == block_id_hash` |
| 2b | Fr consistency | `block_id_fr == Σ bytes(block_id_hash) mod Fr` (hash может превышать BN254 modulus) |
| 3 | Monotonicity | `block_seq_no > stored_last_bk_set_update_seq_no` |

На success: `stored_bk_set_commitment ← L3`, cursor продвигается.

Это и есть **привязка L2/L3 к block_id** без Circuit 3: open SHA-256 path фиксирует пару L2/L3 под тем же root, что и attestation.

### 3.3 Circuit 2 path (layer bundle) — частично

**Файл:** `bridge-prover-daemon/src/main.rs` — `generate_layer_proof_for_key_block` (≈1093–1195)

Добавлено на `main`:

- `leaves[2] == bk_set_commitment.to_repr()` — fail-fast при stale BK set
- Комментарий исправлен: L2/L3 — **Poseidon**, не SHA-256

Circuit 2 in-circuit доказывает L0 Merkle path (preimage + siblings); off-circuit полной сверки GQL leaves нет.

### 3.4 Circuit 1B fallback

Merge PR #6 (`beaf3ba`): Primary (1a) vs Fallback (1b) dispatch в drain loop и layer path. Документация: `docs/fallback_path.md`.

---

## 4. Пробелы и риски

### 4.1 Circuit 2 path — нет `verify_gql_block_merkle`

Порт `acki-nacki/helpers/proof_helper/src/gql_proof.rs` **не перенесён**. На layer path отсутствует:

- `block.block_id == MerkleRoot(leaves)` (только log root, без `ensure!`)
- `leaves[0] == history_proofs_l0(history_proofs)`
- Leaf proofs для L2/L3 (на layer path обычно не нужны — rotation уже прошёл через drain)

**Риск:** битые/подменённые GQL leaves могут дойти до Circuit 2 witness builder; падение только in-circuit или на поздней стадии.

### 4.2 BK-update drain — нет prover-side sanity из плана

`docs/bk_set_update_no_circuit3_plan.md` §4.2 step 3 требует:

```text
tree.block_id() == block_id_from_attestation
```

В drain loop **не реализовано**. Prover пишет `tree.root` в bundle и `block_id_fr` из attestation proof, но **не сверяет** `upd_block.block_id` (GQL) с `tree.root` до отправки. Verifier восстанавливает Merkle и Fr-consistency; fail-fast на prover отсутствует.

### 4.3 Misleading comment (self-verify)

`bridge-prover-daemon/src/main.rs` ≈645–646 (`#[cfg(feature = "self-verify")]`):

> «Local checks already enforced by the Merkle equality assertion above»

**Такого assert на prover нет** — локально только L2==commitment и Poseidon==L3.

### 4.4 Layer bundle verifier — нет cross-check block_id

В `bridge-verifier-daemon` при обработке layer bundle (`proof_*.json`) **нет проверки**, что `block_id_hex` из Circuit 1a совпадает с `layer_block_id_hex` из Circuit 2 в одном bundle.

### 4.5 `next_update_after` — lookback 100

`bridge-prover-lib/src/bk_set_fetcher.rs`:

```rust
const NEXT_UPDATE_AFTER_LOOKBACK: u32 = 100;
```

Если prover отстаёт >100 rotation events, cursor может вернуть `None` при наличии необработанных событий. Для shellnet достаточно; для production — риск.

### 4.6 Отсутствующие инструменты и тесты

| Элемент | Статус |
|---------|--------|
| `probe_bk_updates` binary (план §7 Phase 2) | **Не добавлен** (только упоминание в плане) |
| Parity test: реальный shellnet block → Poseidon == `leaves[3]` (план §9) | **Нет** |
| Unit-тесты Merkle binding | **Нет** (есть serde roundtrip `bkupd`, state machine `apply_bk_set_update`, `#[ignore]` cursor walk) |
| `leaves[3]` на layer path | **Явно не проверяется** (комментарий ≈1163–1164) |

---

## 5. Сравнение с откатанным локальным коммитом `a88b074`

Коммит был сделан поверх устаревшего `aac70a0` (до pull `main`) и затем откачен (`git reset --hard aac70a0`).

| Аспект | `a88b074` (откачен) | `main` `c5e0ef3+` |
|--------|---------------------|-------------------|
| Подход | `gql_proof.rs` + вызов перед Circuit 2 | Отдельный bk-update pipeline + verifier |
| BK rotation binding | `verify_gql_proof_block_with_bk_replay` | Open Merkle H0/H23 + attestation + IPC |
| L0 + full root off-circuit | Да | Нет на layer path |
| `ProverBkSet` persistence | Нет | Да |
| Verifier-side enforcement | Нет | 4-step |
| Circuit 1B fallback | Нет | Да |

**Вывод:** коммит `a88b074` **не нужен как есть**. Остаются точечные дополнения (§4), а не дублирование drain pipeline.

---

## 6. Независимая верификация

Отдельный read-only агент проверил все 7 ключевых утверждений ревью на `main` @ `33ae2e3`:

| # | Утверждение | Вердикт |
|---|-------------|---------|
| 1 | Drain loop, не `gql_proof.rs` | ✅ CORRECT |
| 2 | Verifier 4 checks | ✅ CORRECT |
| 3 | Layer path: `leaves[2]` only | ✅ CORRECT |
| 4 | Нет `upd_block.block_id == tree.root` в drain | ✅ CORRECT |
| 5 | Lookback 100 | ✅ CORRECT |
| 6 | `probe_bk_updates` отсутствует | ✅ CORRECT |
| 7 | План в `bk_set_update_no_circuit3_plan.md` | ✅ CORRECT |

Агент дополнительно выявил: нереализованный sanity из плана §4.2, misleading self-verify comment, отсутствие cross-check block_id между Circuit 1a и 2 в layer verifier.

---

## 7. Рекомендации (без приоритизации кода)

1. **Layer path:** добавить off-circuit `verify_gql_block_merkle` (root + L0) перед Circuit 2 — порт из `acki-nacki/helpers/proof_helper` или тонкая обёртка в `bridge-prover-lib`.
2. **Drain path:** fail-fast `upd_block.block_id == tree.root` и `block_id_fr` consistency на prover до IPC (как в плане §4.2).
3. **Исправить** misleading comment в self-verify ветке drain loop.
4. **Layer verifier:** cross-check `block_id` между Circuit 1a и 2 в одном bundle.
5. **`next_update_after`:** пагинация / walk до cursor, не фиксированный lookback 100.
6. **Тесты:** parity shellnet block (Poseidon replay == `leaves[3]`); unit-тест open Merkle reconstruction.
7. **`probe_bk_updates`:** по плану §7 Phase 2 для cadence study на shellnet.

---

## 8. Ключевые файлы и коммиты

| Артефакт | Путь / SHA |
|----------|------------|
| План архитектуры | `docs/bk_set_update_no_circuit3_plan.md` |
| Drain loop | `bridge-prover-daemon/src/main.rs` |
| Verifier bk-update | `bridge-verifier-daemon/src/main.rs` |
| BK cursor walk | `bridge-prover-lib/src/bk_set_fetcher.rs` |
| Prover BK state | `bridge-prover-lib/src/prover_bk_set.rs` |
| IPC schema | `bridge-prover-lib/src/ipc.rs` |
| Block-id tree math | `bridge-prover-lib/src/block_id_tree.rs` |
| Референс (нода) | `acki-nacki/node/.../block_merkle_leaves` |
| Референс (proof helper) | `acki-nacki/helpers/proof_helper/src/gql_proof.rs` |
| Landing commit | `c5e0ef3` |
| Fallback 1B | `beaf3ba` (PR #6) |
| HEAD при ревью | `33ae2e3` |

---

## 9. История проверки

| Дата | Действие |
|------|----------|
| 2026-06-08 | Первичное ревью на устаревшем `aac70a0`; локальный коммит `a88b074` |
| 2026-06-08 | Pull `main` @ `33ae2e3`; откат `a88b074`; повторное ревью |
| 2026-06-08 | Независимая верификация отдельным агентом |
| 2026-06-08 | Сводный отчёт (этот файл) |

---

*Отчёт подготовлен в рамках ревью интеграции AN→ETH bridge prover. Изменений в коде не вносилось.*
