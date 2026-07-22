# Deposit MPT `max_key_byte_len` = 3

Receipt-trie keys are RLP(`tx_index`). axiom-eth pins this to **3** bytes
(`TRANSACTION_IDX_MAX_LEN = 2` → `1 + max_rlp_len_len(2) + 2 = 3`) in
`providers/receipt.rs` / `providers/transaction.rs`. Storage tries use **32**.

`deposit-prover` previously set `max_key_byte_len: 4` (earlier still **32**),
which desynced MPT padding from axiom-eth's RLP key decomposition and broke
`MockProver`. Aligning to **3** restores MockProver and matches the axiom
spec; it **changes the circuit shape** (new VkBlob).

## Fixture rotation (2026-07-22)

| | SHA-256 | size |
|---|---|---|
| Previous (`max_key_byte_len=4`) | `304c1c4e…46251a` | 3982 B |
| Current (`max_key_byte_len=3`) | `724687a4…e79b9c` | 3982 B |

Regenerated with:

```bash
cargo run --release --example export_deposit_proof_set -- \
  --set-dir fixtures/deposit_10proofs --count 10 \
  --degree 18 --max-data-byte-len 256 --max-log-num 20
```

`EthCircuitParams` stayed `num_advice_per_phase=[13,10]`, `shard_caps=[64]`.

**Follow-up:** re-embed this VkBlob into shellnet `USDCBridge` and sync
`tvm-sdk` `deposit_10proofs` (`scripts/sync_deposit_opcode_fixtures_to_tvm_sdk.sh`).
