# Cross-chain (ETH ↔ AN) — questions for authors

**Date:** 2026-07-17  
**Audience:** Bridge integrators — **both** ETH and AN teams (joint disposition).  
**Scope:** Policy and binding alignment between `AckiNackiBridge` (Sepolia/mainnet) and `USDCBridge` (shellnet/production AN). Not a substitute for per-side registers.

**Related:**

| Side | Register |
|------|----------|
| Ethereum only | `questions-eth.md`, `closeout-eth.md` |
| Acki Nacki only | `questions-an.md`, `an-audit-direction.md` |

---

## Classification policy

Items here are **QC** (design / policy questions). They may reference BC rows on one side (e.g. BC-AN-01) but require **joint** author agreement because both deployments are involved.

---

## Binding matrix (auditor view)

| Concern | Ethereum (`AckiNackiBridge`) | Acki Nacki (`USDCBridge`) | Gap? |
|---------|------------------------------|---------------------------|------|
| Deposit event fields | `depositId`, `sender`, `amount`, `anWorkchain`, `anAccount` | PI fr[0..7] + config `dappId` | `dappId` only off-chain → **BC-AN-01** |
| L1 bridge identity | Fixed deploy address in proof via receipt | `contractAddr` in PI, no allowlist | **BC-AN-02** |
| Max amount | `MAX_DEPOSIT_AMOUNT` (100 USDC per tx) | `uint64` bound on `fr[2]` (QC-AN-01) | QC-AN-J1 |
| Zero recipient | `InvalidAnAccount` if `anAccount==0` | No pre-ZK guard (QC-AN-10) | QC-AN-J3 |
| Emergency pause | `pause()` blocks `deposit` | No pause on `finalizeDeposit` | QC-AN-J2 |
| Workchain | Emitted in `Deposit` | `makeAddrStd(0, anAccount)` always WC 0 | QC-AN-J4 |
| Custody model | USDC in bridge / AAVE | ECC mint (currency #3) | QC-AN-J5 |

---

## QC register — cross-chain

| ID | ETH side | AN side | Auditor view | Ask author (joint) |
|----|----------|---------|--------------|-------------------|
| QC-AN-J1 | `MAX_DEPOSIT_AMOUNT` = 100 USDC | `uint64` cap on mint path (QC-AN-01) | **Asymmetric caps** if deposit circuit allows larger amounts than ETH accepts. | Align documented max deposit ETH↔AN? Single source of truth? |
| QC-AN-J2 | `pause()` on user entrypoints | no pause on AN | ETH incident pause does **not** stop AN `finalizeDeposit` for already-deposited L1 events. | Intentional asymmetric liveness? Add AN pause? |
| QC-AN-J3 | `InvalidAnAccount` | no pre-ZK `anAccount==0` check (QC-AN-10) | ETH stricter than AN on zero recipient. | Harmonize (ETH-style revert on AN)? |
| QC-AN-J4 | `anWorkchain` in `Deposit` event | payout `makeAddrStd(0, account)` | Workchain from L1 not enforced on AN credit path. | Document workchain retired / always 0? |
| QC-AN-J5 | USDC trust (QC-A1-2) | ECC mint, no USDC pause hook on bridge | Independent trust domains (L1 custody vs AN ledger). | Accepted two-domain model for milestone? |

---

## BC items with cross-chain impact

These are filed on the **AN** register but ETH team should ack binding assumptions:

| ID | Why cross-chain |
|----|-----------------|
| **BC-AN-01** | L1 event has no `dappId`; ETH cannot constrain it. Fix is AN-side (or circuit VK per deployment). |
| **BC-AN-02** | L1 `contractAddr` in PI comes from receipt; ETH team confirms canonical bridge address(es) to pin on AN. |

Details: `questions-an.md` § BC register.

---

## 11 public inputs alignment (deposit)

Auditor verified ETH `deposit-prover` layout matches AN `_parsePublicInputs`:

```
[0] depositId  [1] sender  [2] amount  [3] contractAddress
[4] dappIdHigh [5] dappIdLow [6] anAccountHigh [7] anAccountLow
[8] blockHashHigh [9] blockHashLow [10] promiseCommit
```

Recipient on AN: `(anAccountHigh << 128) | anAccountLow` — matches post-#2271 256-bit binding.

**Open:** `dappId` limbs [4–5] are config-supplied (relayer `AN_DAPP_ID`), not in L1 log — see BC-AN-01.

---

## AN → ETH direction (note for integrators)

Withdrawal: user burns ECC on AN via `initiateWithdrawal` → off-chain Circuit 4 proof → `AckiNackiBridge.withdrawByProof` on ETH. That path is audited on **ETH** (`questions-eth.md` § A3), not in the AN contract QC table above.
