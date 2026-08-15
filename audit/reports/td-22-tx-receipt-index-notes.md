# TD-22 — Tx trie vs receipt trie `transaction_index` coupling

PoC: `tests/td_22_tx_receipt_index_coupling.rs`.

## Binding

Single `tx_idx` witness drives:

1. Receipt trie MPT path (`receipt_proof.to_mpt_input(tx_index, …)`)
2. Transaction trie MPT path (`tx_proof.to_mpt_input(tx_index, …)`)
3. `EthReceiptInputAssigned.tx_idx` and `EthTransactionInputAssigned.transaction_index` (same AssignedValue)

## Verdict: **OK**

| Case | MockProver |
|------|------------|
| Synthetic / proof_00 aligned index | Pass |
| Event index ≠ receipt trie path | Reject |
| Receipt@i + tx proof nodes@j (i≠j) | Reject |
| Corrupt tx leaf bytes | Reject |

**Not BC** — mismatched indices do not satisfy.

## Commands

    cd deposit-prover && cargo test td_22 -- --nocapture
    cd deposit-prover && cargo test
