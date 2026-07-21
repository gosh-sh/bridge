# AN contracts & deposit proofs — questions for authors

**Date:** 2026-07-21 (author responses from Pruvendo QA Stage II — Bridge §)  
**Audience:** Acki Nacki bridge team (`USDCBridge.sol`, `DepositVoucher.sol` in `acki-nacki`).  
**Scope:** AN-side **contract logic** for ETH→AN deposits (`finalizeDeposit` / `confirmDeposit`), withdraw **initiation** (`initiateWithdrawal`), admin/TIP-3. ZK opcode internals (`tvm-sdk`) out of scope — verified via fixtures only.  
**Out of scope here:** Ethereum `AckiNackiBridge`, Circuit 1A/1B/2/4 provers, cross-chain policy — see sibling docs.

**Branch / artefacts:** audit overlay `audit/spec/an/`; synced contract `audit/spec/an-contracts/` from `acki-nacki@contracts/dex_bridge`.  
**Test gate:** `make audit-an-test` → **74 passed** (2026-07-20, incl. F8-F BC regressions).

**Closeout:** `audit/reports/closeout-an.md` (draft — partial author ack).

**Author responses:** recorded from *AckiNacki — Pruvendo QA Stage II* (Bridge §, 2026-07-21). Rows without a dev answer stay **in progress**; no further outbound mail for this round.

Manual reviews: `manual-audit/F1-usdcbridge-deposit.md`, `manual-audit/F2-usdcbridge-withdraw-admin.md`.

---

## Classification policy

| Label | PoC | Meaning |
|-------|-----|---------|
| **OK** | Usually yes | Matches intent (after author confirms) |
| **QC** | **Required** | Reproduced; auditor view stated; **author must confirm** bug vs feature / centralization |
| **BC** | **Required** | Bug candidate — fund loss or broken binding under stated assumptions |

QC ≠ «не проверяли». QC = «проверили, просим автора подтвердить intent».

**This pass:** BC = **1** open (BC-AN-02). BC-AN-01 **closed**. QC AN-only: **4 closed**, **4 partial ack**, **3 open** (no Stage II answer). Withdraw/admin surface: **0 BC**.

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

### BC-AN-01 — `dappId` in replay key, prover-configured (High) — **closed**

| | |
|--|--|
| **Area** | `USDCBridge.finalizeDeposit` — `depositHash = hash(depositId, contractAddr, dappId)` |
| **Issue** | Two valid ZK proofs over the **same** L1 deposit with different `dappId` → two vouchers → **two mints** for one `depositId`. |
| **Fix** | `acki-nacki@contracts/dex_bridge`: `f.dappId = 0` in `_parsePublicInputs` — PI dapp limbs ignored; all deposits land in dapp 0. |
| **PoC / regression** | ✅ `integration/test_bc_an_01_dapp_id_double_mint.py` — `test_bc_an_01_double_mint_same_deposit_two_dapp_ids` |
| **Analysis** | `audit/findings/BC-AN-01/analysis.md` |
| **Disposition** | **Closed** — author ack 2026-07-20 (repo) + Stage II: «fixed. Принудительно dapp_id делаю 0». |

---

### BC-AN-02 — no L1 bridge `contractAddr` allowlist (Medium) — **open**

| | |
|--|--|
| **Area** | `fr[3]` `contractAddr` in replay key; no `require(f.contractAddr == EXPECTED_L1_BRIDGE)` |
| **Issue** | Proof valid for a deposit on bridge deployment A could finalize if circuit only checks “some contract emitted Deposit”, not a pinned Sepolia address. |
| **PoC** | **Partial** — `test_bc_an_02_no_l1_bridge_allowlist_pre_zk` (no revert before ZK). Full PoC needs second valid proof with different `contractAddr` PI. |
| **Analysis** | `audit/findings/BC-AN-02/analysis.md` |
| **Author (Stage II)** | *No answer* — in progress. |

**Still open:**

1. Should `USDCBridge` pin `EXPECTED_L1_BRIDGE` (immutable)?
2. Is multi-L1-bridge → single AN bridge an intended deployment model?

---

## QC register — AN-only

### Deposit / finalizeDeposit

| ID | PoC | Auditor view | Author (Stage II) | Disposition |
|----|-----|--------------|-------------------|-------------|
| QC-AN-01 | `unit/test_usdcbridge_finalize_negative.py` | `fr[2]` must fit `uint64` (exit 214) before mint. | «лучше в эфире увеличить до u64. Надо подумать про минимальный размер» | **partial ack** — raise ETH cap direction; min deposit TBD (see QC-AN-J1) |
| QC-AN-08 | `unit/test_usdcbridge_deposit_edge.py` | Truncated PI rejected before mint. | (no question in Stage II) | **closed (test)** |
| QC-AN-09 | code review (`tvm.accept()` before ZK) | Permissionless submit; bad proof wastes bridge gas — griefing, not theft. | *No answer* | **open** — in progress |
| QC-AN-10 | `unit/test_usdcbridge_deposit_edge.py` | No `anAccount==0` guard pre-ZK (ETH reverts `InvalidAnAccount`). | «добавлю require» on AN; «убрать проверку в эфире» — burn on L1 allowed | **partial ack** — deliberate asymmetry; await code on both sides (see QC-AN-J3) |

### Admin / upgrade / ops

| ID | PoC | Auditor view | Author (Stage II) | Disposition |
|----|-----|--------------|-------------------|-------------|
| QC-AN-02 | `unit/test_usdcbridge_admin.py` ADM-AN-02..03 | Owner `mintAndSend` without cross-chain proof — centralization. | *No answer* | **open** — in progress |
| QC-AN-03 | F2 manual + code review | `updateCode` / `onCodeUpgrade` only path to rotate voucher code. | «обновлять код ваучера нельзя — старые пруфы позволят наминтить. Тестов нет» | **closed (ack)** — immutable voucher code by design |
| QC-AN-04 | code review | No pause on AN; ETH `pause()` blocks L1 `deposit`. | *No answer* | **open** — in progress (see QC-AN-J2) |
| QC-AN-05 | `integration/test_cross_counter_fuzz.py` | `_totalMinted` vs bridged minted decoupled. | «нет, оставляем отдельно. Ещё посмотрю на превышение» | **partial ack** — separate counters OK; burned>m minted under review |
| QC-AN-06 | integration fixtures + `VK_BLOB` | VkBlob hash must match deployed circuit. | «тестируем тестовыми пруфами» | **partial ack** — fixture smoke, not CI hash gate |

### Infrastructure / edge cases

| ID | PoC | Auditor view | Author (Stage II) | Disposition |
|----|-----|--------------|-------------------|-------------|
| QC-AN-07 | `integration/test_finalize_deposit_voucher_brick.py` | Missing `_depositVoucherCode` → no mint after ZK. | «да. Ваучер будет установлен сразу при деплое» | **closed (ack)** — prod embeds voucher at deploy |

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
| BC-AN-02 | `immutable EXPECTED_L1_BRIDGE` | Medium |
| QC-AN-10 | `require(f.anAccount != 0)` on AN; dev plans to drop ETH `InvalidAnAccount` | Partial ack — asymmetric burn model (Stage II) |

---

## Reproduce (reviewers)

```bash
./scripts/setup_an_audit_tools.sh
./scripts/sync_an_contracts.sh
make audit-an-test
```

BC-AN-01 dual proofs (optional regression regen, ~30 min):

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
