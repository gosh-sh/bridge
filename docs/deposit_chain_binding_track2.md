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
     (default **1** / mainnet; Sepolia shellnet fixtures use **`11155111`**)
  4. Constrains RLP field 5 (`to`) == Deposit `contractAddress`
- Same `tx_index` AssignedValue is shared with the receipt path (binds Deposit
  log to this tx). **`from` is not in EIP-1559 RLP** — no in-circuit ecrecover;
  same-index + `to` binding is the substitute for proposal step F.

Public-input layout stays **11** fields (`chain_id` is VK-baked, not a PI).

## Regenerated fixtures (2026-07-22)

`fixtures/deposit_10proofs/` rebuilt from live Sepolia deposits
(`depositId` 0 / 1 / 8 cycled across 10 slots) with `--chain-id 11155111`:

| | Value |
|---|---|
| VkBlob SHA-256 | `de1dd3abda6bcc563530ad9a9b00080c8e8eea7e79434fbc2231e6ad597dd8d1` |
| VkBlob size | 5006 B |
| Proof size | 11072 B each |
| `EthCircuitParams` | `k=18`, `num_advice_per_phase=[17,13]`, `num_rlc_columns=2`, `shard_caps=[64]` |
| Verify | **10/10** against shared VkBlob |

```bash
cd deposit-prover
cargo run --release --example export_deposit_proof_set -- \
  --set-dir fixtures/deposit_10proofs --count 10 \
  --degree 18 --max-data-byte-len 256 --max-log-num 20 \
  --chain-id 11155111
```

Smoke:

```bash
cargo run --release --example test_with_real_data -- \
  --input fixtures/chain_binding_sepolia_dep0/input.json \
  --mock-only --chain-id 11155111
```

Opcode consumer copy: `scripts/sync_deposit_opcode_fixtures_to_tvm_sdk.sh`
(synced locally; commit in `tvm-sdk` separately).

## Production / mainnet keygen

Mainnet VK must use `--chain-id 1` (and mainnet deposit witnesses). A
Sepolia-keyed VkBlob (`11155111`) will **not** verify mainnet proofs.

Shellnet `USDCBridge` must re-embed the Sepolia VkBlob above after this lands.
