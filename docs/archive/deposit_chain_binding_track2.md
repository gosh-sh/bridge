> **⚠️ ARCHIVED 2026-08-18 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/EVM-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

# Deposit Track 2 — proven `chainId` public input + multi-L2

Implements Track 2 of
[`bridge_deposit_chain_binding_fix_proposal_2026_07_20.md`](./bridge_deposit_chain_binding_fix_proposal_2026_07_20.md),
evolved so **`chainId` is a public input** (not VK-baked). One WITHVK VkBlob
can verify deposits from any allowlisted L2; AN `USDCBridge` allowlists
`(chainId → expected bridge Fr)`.

## What changed

- `DepositProofInput` gains `tx_proof: TransactionProof` (typed-tx wire bytes +
  transactions-trie MPT path under `transactionsRoot`).
- `fetch_deposit_proof` / `generate_transaction_proof` reconstruct the tx trie
  (no `eth_getProof` for txs) and require EIP-1559 (`0x02`).
- Circuit Phase 0 additionally:
  1. MPT-includes the enclosing tx against header `transactionsRoot`
  2. Constrains `transaction_type == 2`
  3. Exposes RLP field 0 (`chain_id`) as **public input** slot after
     `contractAddress` (not constrained to a VK constant)
  4. Constrains RLP field 5 (`to`) == Deposit `contractAddress`
- Same `tx_index` AssignedValue is shared with the receipt path (binds Deposit
  log to this tx). **`from` is not in EIP-1559 RLP** — no in-circuit ecrecover;
  same-index + `to` binding is the substitute for proposal step F.

### Public-input layout (**12** scalars including `promiseCommit`)

```
[depositId, sender, amount, contractAddress, chainId,
 dappIdHigh, dappIdLow, anAccountHigh, anAccountLow,
 blockHashHigh, blockHashLow, promiseCommit]
```

CLI `--chain-id` is a **fetch / network selector** only (and feeds the
`SupportedDepositChain` allowlist at fetch time). It is **not** a circuit
soundness constant.

## AN handoff (`USDCBridge`)

Sibling `acki-nacki` must:

1. Parse **12** public inputs (insert `chainId` after `contractAddress`).
2. **Allowlist** `(chainId → expected bridge contract Fr)` for Arbitrum One
   (42161), Base (8453), OP Mainnet (10), Mantle (5000), World Chain (480),
   Blast (81457), plus Sepolia (11155111) for shellnet.
3. Re-embed the regenerated deposit VkBlob after fixture regen lands.

## Regenerated fixtures (2026-08-03, 12-PI + audit fixes)

`fixtures/deposit_10proofs/` rebuilt from Sepolia deposit inputs with
`--chain-id 11155111`. Three rotations have landed in quick succession, all
leaving the shape and PI layout untouched — only the constraint system, VK
points and the 10 proofs differ:

- **2026-07-28, `3e2a2db2…bf0d049c`** — in-circuit soundness fixes from
  `gosh-sh/bridge` PR [#26](https://github.com/gosh-sh/bridge/pull/26): receipt
  bound to the MPT root the chip actually verified, byte-wise root comparison,
  `depositId`/`amount` range checks.
- **2026-07-30, `7322fb82…93f92541`** — Prague header support from the PR #20
  review: the header field table grows to 21 slots for the EIP-7685
  `requestsHash`, `MAX_BLOCK_HEADER_BYTES` 668 → 705, `gasLimit` widened to 8
  bytes for Arbitrum One. The witnesses were re-canonicalised at the same time,
  so each proof's `blockHash` public input is now the block hash Sepolia
  actually reports (previously it hashed a header with `requestsHash` dropped).
- **2026-08-03, `9dacd998…8360fae3`** (current) — deposit-circuit audit fixes,
  `docs/reviews/deposit_circuit_audit_2026-08-03.md`: dropped the
  `tx.to == contractAddress` constraint (BC-D02, unblocks Safe / ERC-4337 /
  router deposits), tied the keccak'd header length to the RLP list length
  (BC-D03), pinned the log topics/data payload lengths to 99/128 (BC-D04),
  range-checked the `dappId` bytes (BC-D05), and widened the `number` /
  `gasUsed` / `timestamp` header slots to 8 bytes with
  `MAX_BLOCK_HEADER_BYTES` 705 → 717 (BC-D06). Witnesses are unchanged — the
  10 saved `input.json` were re-proved as-is.

| | Value |
|---|---|
| VkBlob SHA-256 | `9dacd998af5fd03af8097cb80a571df098c925bba235af61d920cc808360fae3` |
| VkBlob size | 5006 B |
| Public inputs | **384 B** (12 × 32) |
| Proof size | 11072 B each |
| Verify | **10/10** against shared VkBlob |
| `ZKHALO2VERIFYWITHVK` | **10/10 ACCEPTED** (`tvm_vm` `test_zkhalo2_with_vk_deposit_10_real_proofs`) |

```bash
cd deposit-prover
cargo run --release --example export_deposit_proof_set -- \
  --set-dir fixtures/deposit_10proofs --count 10 \
  --degree 18 --max-data-byte-len 256 --max-log-num 20 \
  --chain-id 11155111
```

Opcode consumer copy: `scripts/sync_deposit_opcode_fixtures_to_tvm_sdk.sh`
(commit in `tvm-sdk` separately).
