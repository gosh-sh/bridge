# TD-17 — Reorg / finality vs confirmation depth

PoC: `tests/td_17_reorg_finality.rs`, helper `is_deposit_block_finalized` in `source.rs`.

## Policy (DEP-REORG / DEP-T15 / DEP-N-4)

| Mechanism | Behaviour |
|-----------|-----------|
| `safe_head = head - confirmations` | Deposit surfaced only when `deposit_block <= safe_head` |
| CLI default `confirmations=12` | Ops default; L2 tip lag buffer |
| `confirmations=0` | **QC** — tip-inclusive (`block at head` ok); reorg-sensitive, not forbidden |
| AN `_acceptedBlockHash` | Proof-bound hash must be in anchor set (≥64 conf off-chain writer policy) |

Relayer does **not** prove/finalize deposits above `safe_head`; shallow reorg (head retreat) hides deposit until head recovers. Stale `blockHash` → `ERR_UNKNOWN_BLOCK` (224), cross TD-03.

## Verdict: **OK**

1. Depth math + production-shaped mock source — fail-closed before prove.
2. Simulated reorg (head drop / hash replace) — no mint until re-finalize + canonical hash.
3. **QC:** fixed `confirmations` vs true L2 finality; anchor writer quorum is separate ops path.
4. **Not BC:** no path observed that finalizes unburied blocks or stale hashes.

**BC** would require relayer tick → `Finalized` with `block > safe_head` or anchor-bypass — not reproduced.

## Gap (forward)

- TD-18 `state.json` durability on reorg replay
- Live fork RPC integration (not mock head)
- TD-04 AN patch for production anchor writer

## Commands

    cd crates/deposit-relayer-daemon && cargo test td_17 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test
