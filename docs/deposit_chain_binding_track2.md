# Deposit Track 2 — L1 `chain_id` binding

Implements Track 2 of
[`bridge_deposit_chain_binding_fix_proposal_2026_07_20.md`](./bridge_deposit_chain_binding_fix_proposal_2026_07_20.md).

## What changed

- `DepositProofInput` gains `tx_proof: TransactionProof` (typed-tx wire bytes +
  transactions-trie MPT path under `transactionsRoot`).
- `fetch_deposit_proof` / `generate_transaction_proof` reconstruct the tx trie
  (no `eth_getProof` for txs) and require EIP-1559 (`0x02`).
- Circuit Phase 0 additionally:
  1. MPT-includes the enclosing tx against header `transactionsRoot`
  2. Constrains `transaction_type == 2`
  3. Constrains RLP field 0 (`chain_id`) == `CircuitConfig.expected_chain_id`
     (default **1** / mainnet; Sepolia = `11155111`)
  4. Constrains RLP field 5 (`to`) == Deposit `contractAddress`
- Same `tx_index` AssignedValue is shared with the receipt path (binds Deposit
  log to this tx). **`from` is not in EIP-1559 RLP** — no in-circuit ecrecover;
  same-index + `to` binding is the substitute for proposal step F.

Public-input layout stays **11** fields (`chain_id` is VK-baked, not a PI).

## Smoke fixture

```bash
cd deposit-prover
cargo run --release --example test_with_real_data -- \
  --input fixtures/chain_binding_sepolia_dep0/input.json \
  --mock-only --chain-id 11155111
```

(Sepolia depositId=0 on bridge `0x99c3…ce82`, MockProver green 2026-07-22.)

## Production keygen

```bash
# Mainnet VK (EXPECTED_L1_CHAIN_ID = 1)
cargo run --release --example export_vk_blob -- \
  --input <mainnet_deposit_input.json> --chain-id 1 ...
```

A Sepolia-keyed VK will **not** verify mainnet proofs (and vice versa).

## Stale fixtures

`fixtures/deposit_10proofs/*/input.json` predate Track 2 (`tx_proof` absent /
  empty). Re-fetch with `fetch_deposit_data` (now emits `tx_proof`) and
  re-run `export_deposit_proof_set` before relying on that set.
