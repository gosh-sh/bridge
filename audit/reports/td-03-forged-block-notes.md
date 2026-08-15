# TD-03 — forged / non-canonical L1 block (circuit vs AN anchor)

PoC: `deposit-prover/tests/td_03_forged_block.rs`, `crates/deposit-relayer-daemon/tests/td_03_forged_block_anchor.rs`.

Circuit доказывает inclusion receipt → `block_header_rlp` и `keccak256(header)` в PI; **не** каноничность блока в Ethereum consensus (BC-D01, DEP-N-4). AN `USDCBridge.finalizeDeposit` проверяет `_acceptedBlockHash[chainId][blockHash]` → `ERR_UNKNOWN_BLOCK` (224) если hash не в anchor set.

| Stage | Forged self-consistent witness | Verdict |
|-------|-------------------------------|---------|
| MockProver | pass | **OK** — by design |
| Mock AN (no anchor) | reject 224 | **OK** — fail-closed |
| Mock AN (hash admitted) | finalize | **OK** — control |

**Не BC** при deployed anchor gate: mint без admitted hash не проходит. **BC** если unanchored hash доходит до mint.

TD-04 (AN patch not merged) — отдельный контекст; не смешивать с TD-03 PoC.
