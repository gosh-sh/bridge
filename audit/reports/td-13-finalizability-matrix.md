# TD-13 — Universal finalizability matrix (deposit ETH → AN)

Связанные PoC: `deposit-prover/tests/td_13_universal_finalizability.rs`, TD-11/12 regression tests, `audit/spec/ethereum/DepositFinalizability.t.sol`.

**Свойство (INV):** deposit с envelope из столбца «supported» → MockProver pass под `production_capacity_config()`. Envelope из «unsupported» → prove fail-closed (QC-by-design: L1 может быть ok, mint на AN без валидного proof невозможен).

Константы circuit (`deposit-prover/src/circuit_v2.rs`):

| Параметр | Лимит |
|----------|-------|
| enclosing tx type | EIP-1559 `0x02` only |
| receipt logs | `MAX_LOG_NUM = 3` |
| Deposit log data | 128 B (BC-D04) |
| receipt MPT depth | `RECEIPT_PF_MAX_DEPTH = 10` |
| block header RLP | `MAX_BLOCK_HEADER_BYTES = 717` |
| chainId | witness `tx_bytes` field 0 + allowlist на AN |

## Матрица envelope → finalizable?

| Envelope | L1 `deposit()` | Prover / MockProver | Verdict | PoC |
|----------|----------------|---------------------|---------|-----|
| EIP-1559 `0x02`, 1 log, data 128B | ok | pass | **finalizable** | `proof_00`, synthetic |
| EIP-1559 `0x02`, 3 logs, deposit not at 0 | ok | pass | **finalizable** | TD-10/11 boundary |
| Sepolia fixture `proof_00` (3 logs, index 2) | ok | pass | **finalizable** | committed fixture |
| 4+ receipt logs | ok (TD-11 Foundry) | reject | **QC-not-finalizable-by-design** | TD-11 |
| event data > 128B | — | reject | **QC-not-finalizable-by-design** | TD-11 |
| MPT depth > 10 | — | reject | **QC-not-finalizable-by-design** | TD-11 |
| header RLP > 717B | — | reject | **QC-not-finalizable-by-design** | TD-11 |
| legacy tx (no `0x02`) | ok (Foundry default) | reject | **QC-not-finalizable-by-design** | TD-12 |
| type `0x01` / `0x03` / `0x04` wire | — | reject | **QC-not-finalizable-by-design** | TD-12 |
| wrong `log_index` | ok | reject | **not finalizable** (soundness) | TD-10 OK |
| forged block (self-consistent, not mainnet) | — | pass (inclusion only) | **not canonical** — AN anchor | TD-03 OK |
| forged block / wrong MPT | — | reject | **not finalizable** | TD-09 OK |

## Классификация

| Класс | Смысл |
|-------|-------|
| **OK** | supported envelope доказуем; unsupported fail-closed |
| **QC** | L1 успешен, prove недоступен — задокументированный policy gap (TD-11/12) |
| **BC** | supported envelope не prove-able ИЛИ unsupported проходит prove (не наблюдается) |

Текущий статус PoC: **OK** — supported pass, unsupported reject; BC finding не заведён.

## Forward

| ID | Тема |
|----|------|
| TD-03 | forged / non-canonical L1 block |
| TD-14 | TR-1 solvency (L1 accounting) |
