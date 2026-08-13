# F10 — синтез точечных аудитов (Fable subagents, 2026-07-17)

Три параллельных аудита off-chain подсистем + первые F10-тесты в `deposit-relayer-daemon`.

| Subsystem | Agent | Статус |
|-----------|-------|--------|
| `crates/deposit-relayer-daemon/` | [relayer audit](70ede957-0457-4589-a421-a01ffe37339d) | ✅ |
| `deposit-prover/` | [prover audit](6e349ae2-9dfc-4f69-bde9-8263137788cb) | ✅ |
| `crates/acki-nacki-interface/` | [interface audit](910ba2bf-142f-4ac8-aedb-c9c6da21a339) | ✅ |

Базовая threat model: `F10-offchain-deposit-pipeline.md`.

---

## Критичные находки (приоритет тестов / вопросов авторам)

### P0 — liveness при конкурирующих релеерах (**QC-OFF-06**)

`MockAnSubmitter` при дубликате → `AlreadyFinalized` (курсор двигается).  
`AnInterfaceSubmitter` (live): `is_finalized` всегда `false`; дубликат на AN → `Reverted` → `SubmitOutcome::Rejected` → **вечный цикл** на том же `depositId` + head-of-line blocking.

**Следствие:** сценарий «два честных релеера» на mainnet может **навсегда** остановить один демон. Мок-тесты F10-C дают ложную уверенность без live-path PoC.

**Митигация (для авторов):** read API nullifier; маппинг revert reason «already finalized» → `AlreadyFinalized`; out-of-order finalize.

### P1 — операционные грабли до mainnet

| ID | Суть |
|----|------|
| **QC-OFF-09** | `AN_DAPP_ID` default `"0"` в CLI, без preflight vs on-chain |
| **QC-OFF-10** | `EthLogSource::fetch` — полный рескан `[from_block, safe_head]` каждый tick (O(head) RPC) |
| **QC-OFF-11** | `--backoff-multiplier 0` → hot loop; `state.json` без fsync; `Pending`→`Rejected` |

### P1 — defense-in-depth (safety на AN, слабый локальный чек)

| ID | Суть |
|----|------|
| **QC-OFF-07** | `check_binds_to` — только depositId/sender/anAccount; не amount/contract/block |
| **QC-OFF-08** | `state.json` без chain/bridge/dappId; нет lock-файла |

### P1 — deposit-prover (**QC-PROV-01**)

`verify_proof` / `generate_solidity_verifier` в `prover.rs` ожидают **7** public instances, схема `circuit_v2` — **11**. Off-chain verify и генератор Solidity-верификатора рассинхронизированы с боевым layout.

### P2 — interface / ABI

| ID | Суть |
|----|------|
| **QC-OFF-12** | Устаревший ABI (7 scalar args) в `an-bridge-prover/python/contracts/` vs актуальный 2-arg в `scripts/ursus/` |
| **QC-OFF-13** | `TvmAckiNacki::get_transaction_status` всегда `Confirmed` — ветки `Reverted`/`Pending` в submitter не покрыты live-путём |
| **QC-OFF-04** | (уже в регистре) HTTP default node URL, MITM |

---

## Направления тестов и фаззинга (backlog)

### `deposit-relayer-daemon` — сделано ✅

| ID | Файл | Assert |
|----|------|--------|
| F10-C | `tests/f10_competing_submit.rs` | 2 relayers, 1 mint; race |
| F10-F | `tests/f10_head_of_line.rs` | stuck id 0 blocks id 1 |
| F10-B | `types.rs` | `binding_check_ignores_amount_and_contract_poc` (QC-OFF-07) |

**Gate:** `cargo test -p deposit-relayer-daemon` — все тесты green (+4 integration).

### `deposit-relayer-daemon` — next

| ID | Тест | Цель |
|----|------|------|
| F10-B | `corrupted_state_json_fails_load` | битый state → Err, не silent reset |
| F10-B | `source_returning_wrong_id_is_terminal` | `DepositIdMismatch` |
| F10-C | `live_duplicate_maps_to_rejected_poc` | QC-OFF-06 на mock с `Reverted` |
| F10-D | `receipt_above_safe_head_returns_none` | confirmation depth |
| F10-D | `chunked_scan_boundaries` | RPC chunk math |
| F10-E | extend `interface_submitter_*` | Reverted/Pending receipts |
| Fuzz | proptest: PI roundtrip, `RelayerState` SM, `BackoffConfig::bump` | Rust property layer |

### `deposit-prover` — F10-A (приоритет)

| ID | Тест | Статус |
|----|------|--------|
| F10-A-6 | `instance_layout_is_eleven` | ✅ `tests/f10a_binding.rs` |
| F10-A-7 | `qc_prov_01_*` | ✅ `prover.rs` |
| F10-A-1..5 | MockProver binding / dappId | **blocked** QC-PROV-02 (`#[ignore]`) |
| Fuzz | RLP receipt/log, MPT depth | todo |

### `acki-nacki-interface` — F10-E

| ID | Тест | Цель |
|----|------|------|
| F10-E-1 | tvm_client ABI encode `finalizeDeposit` vs эталон `.abi.json` | 2 bytes only |
| F10-E-2 | wrong `bridge_abi_path` (stale 7-arg) fails fast | QC-OFF-12 |
| F10-E-3 | `ExtendedAddress::parse` negative cases | config typos |

---

## Связь с on-chain BC

| BC | Off-chain усиление |
|----|-------------------|
| BC-AN-01 | F10-A-5; QC-OFF-09; relayer `AN_DAPP_ID` |
| BC-AN-02 | F10-A + QC-OFF-07; нет pin в prover (by design VK) |

---

## Обновить при ответах авторов

- `questions-cross-chain.md` § QC-OFF-06..13, QC-PROV-01
- `test-matrix-an.md` F10 статусы
- `HANDOFF-an-cross-chain-ru.txt` — QC-OFF-06 для ops
