# F1 — USDCBridge / DepositVoucher manual audit

**Branch:** `acki-nacki@origin/dev` (`ab95fb1b2`, `history_cursor` merged)  
**Scope:** cross-chain deposit path (`finalizeDeposit` → `DepositVoucher` → `confirmDeposit`).  
**Out of scope:** tvm-sdk opcode internals (smoke via fixtures only), shellnet e2e.

---

## Surface map

| Flow | Entry | AuthZ | Replay |
|------|-------|-------|--------|
| TIP-3 → ECC stripe | `onTransferReceived` | `msg.sender == _usdcWallet` | N/A (wallet callback) |
| Owner mint | `mintAndSend` / `mintAccumulator` | `onlyOwnerPubkey` + nonce | `_mintNonce` / `_mintAccumulatorNonce` |
| AN → external | `initiateWithdrawal` | burns attached ECC | event for off-chain proof |
| **ETH → AN** | `finalizeDeposit` | permissionless; **proof** | `DepositVoucher` deterministic deploy |
| Mint payout | `confirmDeposit` | `msg.sender == voucher address` | voucher one-shot |

Fixed bridge address: `0:1a1a1a…1a1a` (`USDC_BRIDGE_ADDRESS` in modifiers).

---

## finalizeDeposit (cross-chain inbound)

**Code:** `USDCBridge.sol` L343–375, `_parsePublicInputs` L563–584.

### Ordering (gas / CEI)

1. `_parsePublicInputs` + `require(amount > 0)` — **before** `tvm.accept()` (cheap fail).
2. `tvm.accept()` then `gosh.zkhalo2VerifyWithVK(VK_BLOB, publicInputs, proof)` — ZK runs post-accept (multi-second; cannot run in pre-accept budget).
3. Deploy `DepositVoucher` with `stateInit` keyed on `tvm.hash(abi.encode(depositId, contractAddr, dappId))`.
4. Voucher constructor calls `confirmDeposit` → mint ECC[3] + transfer to `makeAddrStd(0, anAccount)`.

**Auditor view:** permissionless submit is intentional; griefing = wasted bridge gas on bad proofs only.

### Public inputs (11 PI circuit, 8 read on-chain)

| Fr | Field | On-chain use |
|----|-------|----------------|
| 0 | depositId | replay key + events |
| 1 | sender | not used in mint path (proof-bound, ETH-side) |
| 2 | amount | minted ECC amount (`uint128`, `≤ uint64` enforced) |
| 3 | contractAddr | replay key (L1 bridge) |
| 4–5 | dappId hi/lo | replay key |
| 6–7 | anAccount hi/lo | **256-bit recipient** `(hi<<128)|lo` |
| 8–10 | hashes | ignored on AN (receipt binding in circuit) |

Matches ETH `deposit-prover` 11-PI layout. **OK** vs ETH audit assumptions.

### PoC (spec)

- `unit/test_usdcbridge_finalize_negative.py` — zero amount (204), overflow (214), bad proof (220).
- `integration/test_finalize_deposit_fixture.py` — real `proof_00` accepts in tvm-debugger.

---

## DepositVoucher

**Code:** `DepositVoucher.sol` L29–45.

- Constructor checks `msg.sender == USDC_BRIDGE_ADDRESS` and `tvm.hash(encode(id, contract, dapp)) == _depositHash`.
- amount/recipient **not** in hash — fixed by proof; replay cannot re-route (same identity → same address → deploy no-op on chain).
- Forwards `confirmDeposit` with 0.5 vmshell.

**Auditor view:** design matches ETH-side nullifier story; on-chain replay = deterministic address collision (documented).

### Missing in tvm-debugger tests (F4)

Full voucher → confirmDeposit → ECC credit chain needs `MessagePipeline.process()` on deploy message; single `call` stops at outbound internal. **Deferred** to F4 integration.

---

## confirmDeposit

**Code:** L382–416.

- Recomputes voucher address from `depositHash` + `_depositVoucherCode`; `ERR_INVALID_SENDER` (207) otherwise.
- `gosh.mintecc(amount, USDC_ECC_ID)`; transfer to `makeAddrStd(0, anAccount)`.
- Updates `_totalMintedBridgeByToken[USDC_ECC_ID]` (observability only — no supply cap).

**PoC:** `test_confirm_deposit_wrong_sender` (207).

---

## Admin / upgrade

| Function | Risk | Note |
|----------|------|------|
| `setPubkey` | centralization | owner rotation |
| `mintAndSend` / `mintAndSendAccumulator` | **QC-AN-02** | owner can mint ECC without proof |
| `updateCode` / `onCodeUpgrade` | **QC-AN-03** | only path to rotate `_depositVoucherCode` (B2 fix — no standalone setter) |
| `triggerTransaction` | low | owner-only ping |

No pause flag on bridge contract.

---

## QC register (initial — author ack pending)

| ID | Topic | PoC | Auditor view |
|----|-------|-----|--------------|
| QC-AN-01 | `fr[2]` uint64 cap | `test_finalize_deposit_amount_overflow` | Intentional bound for `mintecc`; document max deposit |
| QC-AN-02 | Owner `mintAndSend` | code review | Centralization / ops mint — not cross-chain path |
| QC-AN-03 | `updateCode` rotates voucher | code review | Intended (B2); multisig on owner key? |
| QC-AN-04 | No bridge pause | — | Liveness vs emergency — compare ETH pause |
| QC-AN-05 | `_totalMinted` vs `_totalMintedBridgeByToken` split | code review | Observability only; no invariant — document |
| QC-AN-06 | VK_BLOB pin in source | integration fixture | Must match deployed shellnet VK; CI should hash-check |

### BC candidates (author confirm)

| ID | Sev | Topic | PoC plan |
|----|-----|-------|----------|
| BC-AN-01 | High | `dappId` in replay key but config-supplied in circuit (not in L1 event) | Two valid proofs, same receipt, different `dappId` → two mints |
| BC-AN-02 | Medium | No on-chain allowlist for L1 `contractAddr` | Mint from proof tied to wrong ETH bridge deployment |

**BC = 2 candidates** (BC-AN-01, BC-AN-02 — author confirm). See `audit/reports/an-audit-direction.md`.

---

## Cross-refs ETH audit

- ETH `AckiNackiBridge.deposit` 11-PI binding ↔ AN `_parsePublicInputs` — **aligned**.
- ETH QC-A1-2 (USDC trust) — ETH custody; AN mint is ECC[3] not TIP-3 pull.
- Recipient 256-bit — fixed in #2271; reassembly in `_parsePublicInputs` matches prover.

---

## Next (F2–F4)

- [ ] Test matrix rows for withdrawal / owner mint negatives
- [ ] MessagePipeline: voucher deploy → confirmDeposit → minted counter
- [ ] Replay same `proof_00` twice (minted unchanged) — debugger limitation on persistent ECC TBD
- [ ] Shellnet e2e — deferred (infra)
