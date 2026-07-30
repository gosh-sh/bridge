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

## Regenerated fixtures (2026-07-30, 12-PI + Prague header table)

`fixtures/deposit_10proofs/` rebuilt from Sepolia deposit inputs with
`--chain-id 11155111`. Two rotations landed in quick succession, both leaving
the shape and PI layout untouched — only the constraint system, VK points and
the 10 proofs differ:

- **2026-07-28, `3e2a2db2…bf0d049c`** — in-circuit soundness fixes from
  `gosh-sh/bridge` PR [#26](https://github.com/gosh-sh/bridge/pull/26): receipt
  bound to the MPT root the chip actually verified, byte-wise root comparison,
  `depositId`/`amount` range checks.
- **2026-07-30, `7322fb82…93f92541`** (current) — Prague header support from the
  PR #20 review: the header field table grows to 21 slots for the EIP-7685
  `requestsHash`, `MAX_BLOCK_HEADER_BYTES` 668 → 705, `gasLimit` widened to 8
  bytes for Arbitrum One. The witnesses were re-canonicalised at the same time,
  so each proof's `blockHash` public input is now the block hash Sepolia
  actually reports (previously it hashed a header with `requestsHash` dropped).

| | Value |
|---|---|
| VkBlob SHA-256 | `7322fb8257a3ab9024a6cff2317452b91dbf5c5b7dcd7584564eabc293f92541` |
| VkBlob size | 5006 B |
| Public inputs | **384 B** (12 × 32) |
| Proof size | 11072 B each |
| Verify | **10/10** against shared VkBlob |
| Sepolia MockProver | PASS (`chain_binding_sepolia_dep0`) |

```bash
cd deposit-prover
cargo run --release --example export_deposit_proof_set -- \
  --set-dir fixtures/deposit_10proofs --count 10 \
  --degree 18 --max-data-byte-len 256 --max-log-num 20 \
  --chain-id 11155111
```

Opcode consumer copy: `scripts/sync_deposit_opcode_fixtures_to_tvm_sdk.sh`
(commit in `tvm-sdk` separately).
