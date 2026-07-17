# AN contracts & deposit proofs — questions for authors

**Date:** 2026-07-17  
**Audience:** Acki Nacki bridge team (`USDCBridge.sol`, `DepositVoucher.sol` in `acki-nacki`).  
**Scope:** AN-side **contract logic** for ETH→AN deposits (`finalizeDeposit` / `confirmDeposit`), withdraw **initiation** (`initiateWithdrawal`), admin/TIP-3. ZK opcode internals (`tvm-sdk`) out of scope — verified via fixtures only.  
**Out of scope here:** Ethereum `AckiNackiBridge`, Circuit 1A/1B/2/4 provers, cross-chain policy — see sibling docs.

**Branch / artefacts:** audit overlay `audit/spec/an/`; synced contract `audit/spec/an-contracts/` from `acki-nacki@dev`.  
**Test gate:** `make audit-an-test` → **37 passed** (2026-07-17).

Manual reviews: `manual-audit/F1-usdcbridge-deposit.md`, `manual-audit/F2-usdcbridge-withdraw-admin.md`.

---

## Classification policy

| Label | PoC | Meaning |
|-------|-----|---------|
| **OK** | Usually yes | Matches intent (after author confirms) |
| **QC** | **Required** | Reproduced; auditor view stated; **author must confirm** bug vs feature / centralization |
| **BC** | **Required** | Bug candidate — fund loss or broken binding under stated assumptions |

QC ≠ «не проверяли». QC = «проверили, просим автора подтвердить intent».

**This pass:** BC = **2 candidates** (author confirm). QC = **10** AN-only (+ accepted QC-AN-09). Withdraw/admin surface: **0 BC**.

---

## Flow context (deposit path)

```text
Ethereum Deposit event  →  off-chain deposit-prover (dappId from AN_DAPP_ID config)
                        →  external msg USDCBridge.finalizeDeposit(proof, publicInputs)
                        →  DepositVoucher → confirmDeposit → mint ECC to anAccount
```

`dappId` is **not** in the L1 `Deposit` event. It enters the proof in `fetch_deposit_data --dapp-id` (or `deposit-relayer --dapp-id` / `AN_DAPP_ID`). `finalizeDeposit` is **permissionless** — no relayer whitelist.

---

## BC register — bug candidates (author confirm)

### BC-AN-01 — `dappId` in replay key, prover-configured (High)

| | |
|--|--|
| **Area** | `USDCBridge.finalizeDeposit` — `depositHash = hash(depositId, contractAddr, dappId)` |
| **Issue** | Two valid ZK proofs over the **same** L1 deposit with different `dappId` → two vouchers → **two mints** for one `depositId`. Not a ZK soundness bug: circuit correctly binds proof to PI; `dappId` is a free config tag, not from L1. |
| **PoC** | ✅ `integration/test_bc_an_01_dapp_id_double_mint.py` — `test_bc_an_01_double_mint_same_deposit_two_dapp_ids` (Hermez dual proofs; regen: `scripts/bootstrap_hermez_srs_k18.sh` + `scripts/audit/generate_bc_an_01_dual_proofs.sh`) |
| **Supporting tests** | `test_bc_an_01_tampered_dapp_id_rejects_same_proof`, `test_bc_an_01_replay_key_differs_by_dapp_id` |
| **Analysis** | `audit/findings/BC-AN-01/analysis.md` |

**Ask author:**

1. Confirm this is an unintended fund-loss path (not multi-dapp-by-design)?
2. Preferred fix: immutable `EXPECTED_DAPP_ID` in `USDCBridge`, remove `dappId` from replay hash, and/or bind `dappId` as circuit constant (new VK)?
3. Should `AN_DAPP_ID` be enforced to match the bridge deployment's `dapp_id::account_id` prefix on-chain?

---

### BC-AN-02 — no L1 bridge `contractAddr` allowlist (Medium)

| | |
|--|--|
| **Area** | `fr[3]` `contractAddr` in replay key; no `require(f.contractAddr == EXPECTED_L1_BRIDGE)` |
| **Issue** | Proof valid for a deposit on bridge deployment A could finalize if circuit only checks “some contract emitted Deposit”, not a pinned Sepolia address. |
| **PoC** | **Partial** — `test_bc_an_02_no_l1_bridge_allowlist_pre_zk` (no revert before ZK). Full PoC needs second valid proof with different `contractAddr` PI. |
| **Analysis** | `audit/findings/BC-AN-02/analysis.md` |

**Ask author:**

1. Should `USDCBridge` pin `EXPECTED_L1_BRIDGE` (immutable)?
2. Is multi-L1-bridge → single AN bridge an intended deployment model?

---

## QC register — AN-only

### Deposit / finalizeDeposit

| ID | PoC | Auditor view | Ask author |
|----|-----|--------------|------------|
| QC-AN-01 | `unit/test_usdcbridge_finalize_negative.py` (`test_finalize_deposit_amount_overflow`) | `fr[2]` must fit `uint64` (exit 214) before mint — likely intentional `mintecc` bound. | Confirm max deposit / document alignment with ETH `MAX_DEPOSIT_AMOUNT`? (Cross-ref QC-AN-J1.) |
| QC-AN-08 | `unit/test_usdcbridge_deposit_edge.py` | Truncated PI rejected before mint. | **Closed (test)** — behaviour documented. |
| QC-AN-09 | code review (`tvm.accept()` before ZK) | Permissionless submit; bad proof wastes bridge gas only — **griefing**, not theft. | Accept as intentional? |
| QC-AN-10 | `unit/test_usdcbridge_deposit_edge.py` | No `anAccount==0` guard pre-ZK (ETH reverts `InvalidAnAccount`). | Accept vs add on-chain revert mirroring ETH? |

### Admin / upgrade / ops

| ID | PoC | Auditor view | Ask author |
|----|-----|--------------|------------|
| QC-AN-02 | `unit/test_usdcbridge_admin.py` ADM-AN-02..03 | Owner `mintAndSend` mints ECC **without** cross-chain proof — **centralization**, not deposit-path bug. | Multisig / operational controls on `_ownerPubkey` acceptable? |
| QC-AN-03 | F2 manual + code review | `updateCode` / `onCodeUpgrade` only path to rotate `_depositVoucherCode` (B2 — no standalone setter). | Upgrade playbook + voucher rotation tested on shellnet? |
| QC-AN-04 | code review | **No pause** on AN; ETH `pause()` blocks L1 `deposit`. | Should `finalizeDeposit` be pausable on AN? |
| QC-AN-05 | code review + `integration/test_cross_counter_fuzz.py` | `_totalMinted` vs bridged minted decoupled; owner path can make `burned > minted`. | Document ops monitoring for both counters? |
| QC-AN-06 | integration fixtures + synced `VK_BLOB` | VkBlob pinned in source; must match deployment (Hermez `304c1c4e…` on current audit sync). | CI hash-check VK blob on contract sync? |

### Infrastructure / edge cases

| ID | PoC | Auditor view | Ask author |
|----|-----|--------------|------------|
| QC-AN-07 | `integration/test_finalize_deposit_voucher_brick.py` | If `_depositVoucherCode` missing, ZK may pass but **no mint** in pipeline. | **Closed (test)** — confirm production deploy always embeds voucher code. |

---

## Withdraw / admin (F2) — no BC found

| Surface | PoC coverage | Note |
|---------|--------------|------|
| `initiateWithdrawal` | `unit/test_usdcbridge_withdraw_*.py` | Burn + event; Circuit 4 proof verified on **ETH**, not in this AN pass |
| TIP-3 `onTransferReceived` | TIP-AN-01..02 | Wallet callback |
| Owner `setPubkey`, `triggerTransaction` | ADM-AN-07 | Centralization |

---

## Recommended mitigations (auditor — confirm before implementing)

| ID | Suggested action | If author agrees |
|----|------------------|------------------|
| BC-AN-01 | `immutable EXPECTED_DAPP_ID` + `require(f.dappId == EXPECTED_DAPP_ID)` **or** drop `dappId` from replay hash | High — stops double mint |
| BC-AN-02 | `immutable EXPECTED_L1_BRIDGE` | Medium |
| QC-AN-10 | `require(f.anAccount != 0)` before accept | Low / consistency with ETH |

---

## Reproduce (reviewers)

```bash
./scripts/setup_an_audit_tools.sh
./scripts/sync_an_contracts.sh
make audit-an-test
```

BC-AN-01 dual proofs (optional, ~30 min):

```bash
./scripts/bootstrap_hermez_srs_k18.sh
./scripts/audit/generate_bc_an_01_dual_proofs.sh
```

---

## Sibling registers

| Doc | Topic |
|-----|-------|
| `questions-eth.md` | Ethereum contracts & AN-state proofs on ETH |
| `questions-cross-chain.md` | ETH ↔ AN alignment (caps, pause, recipient binding) |
