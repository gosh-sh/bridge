> **⚠️ ARCHIVED 2026-08-18 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/EVM-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

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
| `max_key=3` + Track 2 + `chainId` as pure PI + Cancun/Ecotone 20-field header | `006cca5d…191dec05` | 5006 B | commit `c0da8a3`. Same shape (12 PI, advice `[17,13]`, Sepolia) but different constraint system: `expected_chain_id` VK-baked constant removed; `MAX_BLOCK_HEADER_BYTES` 640 → 668; multi-L2 support. **Superseded by next row** |
| … + circuit soundness fixes (receipt bound to the verified MPT root, byte-wise root compare, `depositId`/`amount` range checks) | `3e2a2db2…bf0d049c` | 5006 B | commit `5b0e79a` (PR #26). Shape unchanged (12 PI, advice `[17,13]`, Hermez k=18); only the constraint system, VK points and the 10 proofs differ. **Superseded by next row** |
| … + Prague header support (21-field table incl. `requestsHash`, `MAX_BLOCK_HEADER_BYTES` 668 → 705, 8-byte `gasLimit` for Arbitrum) | `7322fb82…93f92541` | 5006 B | 2026-07-30, PR #20 review R1/R3. Shape unchanged (12 PI, advice `[17,13]`, Hermez k=18). The 10 witnesses were re-canonicalised at the same time (R2), so their `blockHash` PI is now the real Sepolia block hash rather than the hash of a header with `requestsHash` dropped. **Superseded by next row** |
| … + deposit-circuit audit fixes BC-D02/D03/D04/D05/D06 (dropped `tx.to == contractAddress`; `keccak` length == RLP `rlp_len`; log topics/data lengths pinned to 99/128; `dappId` bytes range-checked; `number`/`gasUsed`/`timestamp` slots widened to 8 B, `MAX_BLOCK_HEADER_BYTES` 705 → 717) | **`9dacd998…8360fae3`** | **5006 B** | 2026-08-03, `docs/reviews/deposit_circuit_audit_2026-08-03.md` — current on-disk fixture. Shape still unchanged (12 PI, advice `[17,13]`, Hermez k=18, 11072 B proofs, 384 B public inputs), so the AN side needs only the blob swap. Witnesses unchanged from the previous row — the 10 saved `input.json` were re-proved as-is |

Current `fixtures/deposit_10proofs/deposit_vk_blob.bin` is the **`9dacd998…`** row
(bottom of the table). See `docs/deposit_chain_binding_track2.md` for the
regeneration command (Track 2 adds `--chain-id`, tx-trie witnesses, and 12
public inputs, so it does **not** share the `EthCircuitParams` shape used by the
pre-#19 rows).

**Follow-up:** re-embed the current VkBlob (`9dacd998…8360fae3`, 5006 B, 12 PI)
into shellnet `USDCBridge` — the on-chain blob is still the pre-chain-binding
11-PI `304c1c4e…`. Cross-check hashes with
`docs/partner_note_usdcbridge_chainid_hermez_2026-07-23.md` §"Change 1 —
`VK_BLOB`" and `.cursor/skills/evm-an-deposit-e2e/SKILL.md`.
