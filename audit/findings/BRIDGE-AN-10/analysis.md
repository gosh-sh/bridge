# BRIDGE-AN-10 — zero AN account / empty withdraw recipient

**Class:** **QC** (unspendable mint / burn-then-stuck ETH withdraw; not a second-spend)  
**Status:** **open → patched in this change** (AN require; ETH guards **kept**)  
**Area:** AN `finalizeDeposit` / `confirmDeposit` / `initiateWithdrawal`; ETH `deposit` / `withdrawByProof`  
**Source:** Stage II Q&A PDF QC-AN-10 / QC-AN-09 / WD-AN-07 / QC-AN-J3  
**Invariant:** AN-DEP-8, WD-Q2 — do not mint to `0:0…0`; do not burn ECC for an empty ETH recipient that `withdrawByProof` will later reject

## Summary

ETH already fail-fasts: `deposit` reverts `InvalidAnAccount` if `anAccount == 0`; `withdrawByProof` reverts `InvalidRecipient` if the reconstructed payout is `address(0)`. Stage II notes mixed “drop the ETH guard / allow burn on L1.” That is **not** the product: burning USDC or ECC into an unspendable destination is loss, not a feature.

AN previously reconstructed `makeAddrStd(0, anAccount)` with no pre-ZK zero check, so a proven `anAccount==0` would mint ECC to `0:0…0`. `initiateWithdrawal` with `recipient.length == 0` burned ECC and emitted; ETH then rejected `InvalidRecipient` forever.

## PoC

Gate (this repo, snapshot `eccUSDCBridge`):

```bash
.venv-an-audit/bin/python -m pytest -q \
  audit/spec/an/unit/test_usdcbridge_deposit_edge.py::test_finalize_deposit_zero_an_account_rejected_pre_zk \
  audit/spec/an/unit/test_usdcbridge_withdraw_admin_negative.py::test_initiate_withdrawal_empty_recipient_reverts \
  audit/spec/an/unit/test_withdraw_properties.py
```

ETH (unchanged): `DepositNegative` / WD-Q2 `InvalidRecipient`.

| Path | Before | After |
|------|--------|--------|
| AN `finalizeDeposit` `anAccount==0` | parse + ZK, then mint to `0:0…0` | `ERR_ZERO_RECIPIENT` **before** `tvm.accept` / ZK |
| AN `confirmDeposit` `anAccount==0` | voucher callback could mint | same require |
| AN `initiateWithdrawal` empty `recipient` | burn ECC, emit | require **before** `tvm.accept` |
| ETH `deposit` / `withdrawByProof` | already reject zero | **kept** |

## Fix

- Snapshot `audit/spec/an-contracts/exchange/`: `ERR_ZERO_RECIPIENT = 223` (next free in that error file).
- Production sibling `acki-nacki/contracts/exchange/`: `ERR_ZERO_RECIPIENT = 230` — **223 is already `ERR_WRONG_DAPP` there.** `scripts/sync_an_contracts.sh` from sibling will overwrite the snapshot; pytest helpers must then use **230**.
- Do **not** remove ETH `InvalidAnAccount` / `InvalidRecipient`.

Sibling `acki-nacki` is a different git repo — this finding does not commit it. Deploy AN from that tree separately.
