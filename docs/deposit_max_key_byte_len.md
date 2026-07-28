# Deposit MPT `max_key_byte_len` = 3

Receipt-trie keys are RLP(`tx_index`). axiom-eth pins this to **3** bytes
(`TRANSACTION_IDX_MAX_LEN = 2` → `1 + max_rlp_len_len(2) + 2 = 3`) in
`providers/receipt.rs` / `providers/transaction.rs`. Storage tries use **32**.

`deposit-prover` previously set `max_key_byte_len: 4` (earlier still **32**),
which desynced MPT padding from axiom-eth's RLP key decomposition and broke
`MockProver`. Aligning to **3** restores MockProver and matches the axiom
spec; it **changes the circuit shape** (new VkBlob).

## Fixture rotation (2026-07-22, updated 2026-07-28)

| | SHA-256 | size | notes |
|---|---|---|---|
| `max_key_byte_len=4` (pre-#18) | `304c1c4e…46251a` | 3982 B | broken MockProver vs axiom |
| `max_key=3` only | `724687a4…e79b9c` | 3982 B | #18 |
| `max_key=3` + Track 2 chain binding (Sepolia), VK-baked `chain_id` | `de1dd3ab…7dd8d1` | 5006 B | #19 (commit `c29f30c`); `--chain-id 11155111`; advice `[17,13]`; **superseded by next row** |
| `max_key=3` + Track 2 + `chainId` as pure PI + Cancun/Ecotone 20-field header | **`006cca5d…191dec05`** | **5006 B** | commit `c0da8a3` — current on-disk fixture. Same shape (12 PI, advice `[17,13]`, Sepolia) but different constraint system: `expected_chain_id` VK-baked constant removed; `MAX_BLOCK_HEADER_BYTES` 640 → 668; multi-L2 support |

Current `fixtures/deposit_10proofs/deposit_vk_blob.bin` is the **`006cca5d…`** row
(bottom of the table). See `docs/deposit_chain_binding_track2.md` for the
regeneration command (Track 2 adds `--chain-id`, tx-trie witnesses, and 12
public inputs, so it does **not** share the `EthCircuitParams` shape used by the
pre-#19 rows).

**Follow-up:** re-embed the current VkBlob (`006cca5d…191dec05`, 5006 B, 12 PI)
into shellnet `USDCBridge` — the on-chain blob is still the pre-chain-binding
11-PI `304c1c4e…` — and sync `tvm-sdk` `deposit_10proofs`
(`scripts/sync_deposit_opcode_fixtures_to_tvm_sdk.sh`). Cross-check hashes with
`docs/partner_note_usdcbridge_chainid_hermez_2026-07-23.md` §"Change 1 —
`VK_BLOB`" and `.cursor/skills/evm-an-deposit-e2e/SKILL.md` (both already list
`006cca5d…`).
