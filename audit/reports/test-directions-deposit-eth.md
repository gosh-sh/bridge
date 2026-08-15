# ETH deposit — направления тестирования (индекс)

**Полный каталог (дедуп, 68 TD, приоритеты, маппинг агентов):**  
**[test-directions-deposit-eth-catalog.md](test-directions-deposit-eth-catalog.md)**

**Источники:** Opus 5 (`58f296d9-c697-4600-8079-a447eb098449`), Grok 4.5 (`9a04f1cc-76f7-441d-8c06-49d414af4a9f`), GPT 5.6 (`b33f627e-ce53-4e43-8da7-87e12ebffa1c`).

## Консенсус в одном абзаце

L1 `deposit()` покрыт (~69 tests). Писать PoC на границе **event → 12 PI → prover → relayer → AN** и **RPC/persistence** — не на ещё один happy-path deposit.

## P0 (18 TD) — начать PoC

TD-01..TD-18 в каталоге §3: PI mutation, layout drift, forged block, AN patch gate, dappId, scan_cursor, dual-RPC, MPT, multi-log, circuit bounds, EIP-1559-only, finalizability, TR-1, chainId collision, Sepolia allowlist, reorg, state.json.

## Фазы PoC

См. каталог §6: Фаза 0 (P0) → Фаза 1 (P1) → Фаза 2 (P2/meta).

## Не дублировать

TD-45..48 — уже covered (`baseline-deposit-eth-locked.md`).
