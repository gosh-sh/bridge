# Deposit MPT `max_key_byte_len` = 3

Receipt-trie keys are RLP(`tx_index`). axiom-eth pins this to **3** bytes
(`TRANSACTION_IDX_MAX_LEN = 2` → `1 + max_rlp_len_len(2) + 2 = 3`) in
`providers/receipt.rs` / `providers/transaction.rs`. Storage tries use **32**.

`deposit-prover` previously set `max_key_byte_len: 4` (earlier still **32**),
which desynced MPT padding from axiom-eth's RLP key decomposition and broke
`MockProver`. Aligning to **3** restores MockProver and matches the axiom
spec; it **changes the circuit shape** (new VkBlob).

## Fixture rotation (2026-07-22)

| | SHA-256 | size | notes |
|---|---|---|---|
| `max_key_byte_len=4` (pre-#18) | `304c1c4e…46251a` | 3982 B | broken MockProver vs axiom |
| `max_key=3` only | `724687a4…e79b9c` | 3982 B | #18 |
| `max_key=3` + Track 2 chain binding (Sepolia) | `de1dd3ab…7dd8d1` | 5006 B | #19; `--chain-id 11155111`; advice `[17,13]` |

Current `fixtures/deposit_10proofs/deposit_vk_blob.bin` is the Track 2 Sepolia row.
See `docs/deposit_chain_binding_track2.md` for the regeneration command (Track 2
adds `--chain-id`, tx-trie witnesses, and 12 public inputs, so it does **not**
share the `EthCircuitParams` shape used by the pre-#19 rows).

**Follow-up:** re-embed the current VkBlob (`de1dd3ab…7dd8d1`, 5006 B, 12 PI)
into shellnet `USDCBridge` — the on-chain blob is still the pre-chain-binding
11-PI `304c1c4e…` — and sync `tvm-sdk` `deposit_10proofs`
(`scripts/sync_deposit_opcode_fixtures_to_tvm_sdk.sh`).
